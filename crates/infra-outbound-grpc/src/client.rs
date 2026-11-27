use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use http::{Request, Response};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioIo;
use infra_grpc_core::protocol::{GRPC_TIMEOUT, grpc_timeout, set_grpc_timeout};
use operation_context::{Deadline, OperationContext};
use shared_rules::error_chain::{self, DisplayErrorChain};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_util::either::Either;
use tonic::body::Body;
use tonic::client::TrackResponseStatus;
use tonic::transport::{Channel, Endpoint};
use tower::{BoxError, Service, ServiceExt as _};
use tracing::Instrument as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;

use crate::ClientError;
use crate::call::{
    ClientCustody, ClientGuards, ClientObservation, LifetimeGuard, StatusHook,
    attach_client_response, cancel_on_drop, check_open, client_deadline_status,
    client_stopped_status, guard_upload,
};
use crate::tls::ClientTlsMaterial;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HTTP2_CONNECTION_WINDOW: u32 = 8 * 1024 * 1024;
const HTTP2_STREAM_WINDOW: u32 = 2 * 1024 * 1024;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type TlsTarget = (TlsConnector, rustls::pki_types::ServerName<'static>);
type TransportStream = Either<TcpStream, TlsStream<TcpStream>>;

pub(crate) fn unbracket_host(host: &str) -> &str {
    host.strip_circumfix('[', ']').unwrap_or(host)
}

#[derive(Clone, Copy)]
struct ContextConflict;

#[derive(Clone, Copy, Debug)]
pub enum ClientSecurity<'a> {
    Plaintext,
    Tls(ClientTlsMaterial<'a>),
}

impl ClientSecurity<'static> {
    #[must_use]
    pub const fn tls() -> Self {
        Self::Tls(ClientTlsMaterial {
            ca_certificate_pem: None,
            identity: None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientTimeout {
    FullRpc(Duration),
    OpeningOnly(Duration),
}

impl From<Duration> for ClientTimeout {
    fn from(timeout: Duration) -> Self {
        Self::FullRpc(timeout)
    }
}

impl ClientTimeout {
    fn deadlines(
        self,
        started_at: Instant,
        caller_timeout: Option<Duration>,
    ) -> (Deadline, Option<Deadline>) {
        let (Self::FullRpc(local) | Self::OpeningOnly(local)) = self;
        let opening = Deadline::new(
            started_at,
            caller_timeout.map_or(local, |budget| budget.min(local)),
        );
        let lifetime = match self {
            Self::FullRpc(_) => Some(opening),
            Self::OpeningOnly(_) => caller_timeout.map(|budget| Deadline::new(started_at, budget)),
        };
        (opening, lifetime)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ClientBuilder<'a> {
    destination: &'a str,
    security: ClientSecurity<'a>,
    timeout: ClientTimeout,
    opening_limit: Option<NonZeroUsize>,
}

impl ClientBuilder<'_> {
    #[must_use]
    pub fn opening_limit(mut self, limit: impl Into<Option<NonZeroUsize>>) -> Self {
        self.opening_limit = limit.into();
        self
    }

    pub fn build(self) -> Result<Client, ClientError> {
        if self
            .opening_limit
            .is_some_and(|limit| limit.get() > Semaphore::MAX_PERMITS)
        {
            return Err(ClientError::InvalidOpeningLimit);
        }
        let (uri, origin, tls) = parse_destination(self.destination, self.security)?;
        let endpoint = grpc_endpoint(uri);
        let server_address = crate::observe::ServerAddress::from_uri(endpoint.uri());
        let http = tcp_connector();
        Ok(Client {
            inner: Arc::new(ClientInner {
                origin,
                channel: Channel::new(GrpcConnector { http, tls }, endpoint),
                server_address,
                timeout: self.timeout,
                opening_slots: self
                    .opening_limit
                    .map(|limit| Arc::new(Semaphore::new(limit.get()))),
                series: crate::observe::Series::client(),
            }),
        })
    }
}

fn grpc_endpoint(uri: http::Uri) -> Endpoint {
    Endpoint::from(uri)
        .http2_keep_alive_interval(Duration::from_mins(1))
        .keep_alive_timeout(Duration::from_secs(20))
        .initial_connection_window_size(HTTP2_CONNECTION_WINDOW)
        .initial_stream_window_size(HTTP2_STREAM_WINDOW)
}

fn tcp_connector() -> HttpConnector {
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_nodelay(true);
    http.set_connect_timeout(Some(CONNECT_TIMEOUT));
    http.set_keepalive(Some(Duration::from_mins(1)));
    http.set_keepalive_interval(Some(Duration::from_secs(15)));
    http.set_keepalive_retries(Some(3));
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    http.set_tcp_user_timeout(Some(Duration::from_secs(30)));
    http
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OriginScheme {
    Http,
    Https,
}

struct OriginParts<'a> {
    scheme: OriginScheme,
    host: &'a str,
    port: u16,
}

impl<'a> OriginParts<'a> {
    fn parse(uri: &'a http::Uri) -> Option<Self> {
        if !matches!(uri.path(), "" | "/") || uri.query().is_some() {
            return None;
        }
        let scheme_str = uri.scheme_str()?;
        let scheme = match scheme_str {
            "http" => OriginScheme::Http,
            "https" => OriginScheme::Https,
            _ => return None,
        };
        let authority = uri.authority()?;
        let host = authority.host();
        let port = shared_rules::uri::http_origin_port(scheme_str, authority.as_str(), host)?;
        Some(Self { scheme, host, port })
    }
}

struct ClientOrigin {
    scheme: OriginScheme,
    host: Box<str>,
    port: u16,
}

impl ClientOrigin {
    fn from_parts(parts: &OriginParts<'_>) -> Self {
        Self {
            scheme: parts.scheme,
            host: Box::from(parts.host),
            port: parts.port,
        }
    }

    fn matches(&self, other: &OriginParts<'_>) -> bool {
        self.scheme == other.scheme
            && self.host.eq_ignore_ascii_case(other.host)
            && self.port == other.port
    }
}

#[derive(derive_more::Debug)]
struct ClientInner {
    channel: Channel,
    #[debug(skip)]
    origin: ClientOrigin,
    #[debug(skip)]
    server_address: Option<crate::observe::ServerAddress>,
    timeout: ClientTimeout,
    opening_slots: Option<Arc<Semaphore>>,
    series: &'static crate::observe::Series,
}

#[derive(Clone, Debug)]
pub struct Client {
    inner: Arc<ClientInner>,
}

impl Client {
    #[must_use]
    pub fn builder<'a>(
        destination: &'a str,
        security: ClientSecurity<'a>,
        timeout: impl Into<ClientTimeout>,
    ) -> ClientBuilder<'a> {
        ClientBuilder {
            destination,
            security,
            timeout: timeout.into(),
            opening_limit: None,
        }
    }

    #[must_use]
    pub fn uses_tls(&self) -> bool {
        matches!(self.inner.origin.scheme, OriginScheme::Https)
    }

    #[must_use]
    pub fn matches_origin(&self, origin: &http::Uri) -> bool {
        OriginParts::parse(origin).is_some_and(|other| self.inner.origin.matches(&other))
    }

    #[must_use]
    pub fn with_context(&self, context: &OperationContext) -> WithContext<Self> {
        WithContext::new(self.clone(), context)
    }
}

enum ResponseFutureInner {
    Err(std::future::Ready<tonic::Status>),
    Pending(BoxFuture<Result<Response<Body>, tonic::Status>>),
}

#[must_use = "futures do nothing unless you `.await` or poll them"]
#[derive(derive_more::Debug)]
#[debug("ResponseFuture {{ .. }}")]
pub struct ResponseFuture(ResponseFutureInner);

impl ResponseFuture {
    pub fn from_prepared<F>(
        prepared: Result<PreparedCall, tonic::Status>,
        send: impl FnOnce(PreparedCall) -> F,
    ) -> Self
    where
        F: Future<Output = Result<Response<Body>, tonic::Status>> + Send + 'static,
    {
        match prepared {
            Ok(prepared) => Self(ResponseFutureInner::Pending(Box::pin(send(prepared)))),
            Err(status) => Self(ResponseFutureInner::Err(std::future::ready(status))),
        }
    }
}

impl Future for ResponseFuture {
    type Output = Result<Response<Body>, tonic::Status>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.get_mut().0 {
            ResponseFutureInner::Err(err) => Pin::new(err).poll(cx).map(Err),
            ResponseFutureInner::Pending(pending) => pending.as_mut().poll(cx),
        }
    }
}

impl Service<Request<Body>> for Client {
    type Response = Response<Body>;
    type Error = tonic::Status;
    type Future = ResponseFuture;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        ResponseFuture::from_prepared(PreparedCall::prepare(self, request), PreparedCall::send)
    }
}

#[derive(Clone, Debug)]
pub struct WithContext<S> {
    inner: S,
    context: OperationContext,
}

impl<S> WithContext<S> {
    #[must_use]
    pub fn new(inner: S, context: &OperationContext) -> Self {
        Self {
            inner,
            context: context.clone(),
        }
    }
}

impl<S> Service<Request<Body>> for WithContext<S>
where
    S: Service<Request<Body>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<Body>) -> Self::Future {
        if request.extensions().get::<OperationContext>().is_some() {
            request.extensions_mut().insert(ContextConflict);
        } else {
            request.extensions_mut().insert(self.context.clone());
        }
        self.inner.call(request)
    }
}

#[must_use = "send the prepared call or drop it intentionally"]
#[derive(derive_more::Debug)]
pub struct PreparedCall {
    #[debug(skip)]
    channel: Channel,
    #[debug(skip)]
    request: Request<Body>,
    opening: OperationContext,
    watch_stop: bool,
    #[debug(skip)]
    call: crate::observe::Call,
    #[debug("{:?}", &**lifetime)]
    lifetime: LifetimeGuard,
    #[debug(skip)]
    permit: Option<OwnedSemaphorePermit>,
    #[debug(skip)]
    on_status: Option<StatusHook>,
}

impl PreparedCall {
    pub fn prepare(client: &Client, mut request: Request<Body>) -> Result<Self, tonic::Status> {
        let inner = client.inner.as_ref();
        let context_conflict = request
            .extensions_mut()
            .remove::<ContextConflict>()
            .is_some();
        let started_at = Instant::now();
        let caller_timeout = grpc_timeout(request.headers());
        if caller_timeout.is_none() {
            request.headers_mut().remove(GRPC_TIMEOUT);
        }
        let call =
            crate::observe::start_call(inner.series, &request, inner.server_address.as_ref());
        if context_conflict {
            return Err(call.refuse(tonic::Status::internal(
                "request operation context conflicts with client context",
            )));
        }
        let contexts = derive_call_contexts(
            inner.timeout,
            started_at,
            caller_timeout,
            request.extensions().get::<OperationContext>(),
        );
        otel_http::inject_context(
            &tracing_opentelemetry_instrumentation_sdk::find_context_from_tracing(call.span()),
            request.headers_mut(),
        );
        let permit = match acquire_opening_permit(inner.opening_slots.as_ref(), &contexts.opening) {
            Ok(permit) => permit,
            Err(status) => return Err(call.refuse(status)),
        };
        Ok(Self {
            channel: inner.channel.clone(),
            request,
            opening: contexts.opening,
            watch_stop: contexts.watch_stop,
            call,
            lifetime: contexts.lifetime,
            permit,
            on_status: None,
        })
    }

    #[must_use]
    pub const fn opening_context(&self) -> &OperationContext {
        &self.opening
    }

    pub fn try_step(
        mut self,
        step: impl FnOnce(&OperationContext, &mut http::HeaderMap) -> Result<(), tonic::Status>,
    ) -> Result<Self, tonic::Status> {
        match step(&self.opening, self.request.headers_mut()) {
            Ok(()) => Ok(self),
            Err(status) => Err(self.call.refuse(status)),
        }
    }

    pub async fn try_await<T>(
        self,
        step: impl AsyncFnOnce(&OperationContext) -> Result<T, tonic::Status>,
    ) -> Result<(Self, T), tonic::Status> {
        match step(&self.opening).await {
            Ok(value) => Ok((self, value)),
            Err(status) => Err(self.call.refuse(status)),
        }
    }

    pub fn on_status(mut self, hook: impl FnOnce(tonic::Code) + Send + 'static) -> Self {
        self.on_status = Some(match self.on_status.take() {
            Some(previous) => Box::new(move |code| {
                previous(code);
                hook(code);
            }),
            None => Box::new(hook),
        });
        self
    }

    pub async fn send(self) -> Result<Response<Body>, tonic::Status> {
        self.send_with_dispatch(|_| Ok(())).await
    }

    pub async fn send_with_dispatch(
        self,
        prepare_dispatch: impl FnOnce(&mut http::HeaderMap) -> Result<(), tonic::Status> + Send,
    ) -> Result<Response<Body>, tonic::Status> {
        let Self {
            mut channel,
            request,
            opening,
            watch_stop,
            call,
            lifetime,
            permit,
            on_status,
        } = self;
        let (mut parts, body) = request.into_parts();
        let track_decoded_status = parts.extensions.remove::<TrackResponseStatus>().is_some();
        let (body, upload_guard) = guard_upload(body);
        let mut request = Request::from_parts(parts, body);
        let result = match opening
            .run(async {
                let ready = channel
                    .ready()
                    .await
                    .map_err(|err| report_transport_failure(&err))?;
                check_open(&opening)?;
                prepare_dispatch(request.headers_mut())?;
                check_open(&opening)?;
                apply_forwarded_timeout(request.headers_mut(), lifetime.deadline())?;
                ready
                    .call(request)
                    .await
                    .map_err(|err| report_transport_failure(&err))
            })
            .instrument(call.span().clone())
            .await
        {
            Ok(sent) => sent,
            Err(reason) => Err(client_stopped_status(reason)),
        };
        drop(permit);
        let mut response = match result {
            Ok(response) => response,
            Err(status) => return Err(call.refuse(status)),
        };
        let observation = if track_decoded_status {
            ClientObservation::decoded(call, &mut response)
        } else {
            ClientObservation::Transport(call)
        };
        Ok(attach_client_response(
            response,
            observation,
            ClientCustody {
                watch_stop,
                guards: ClientGuards {
                    upload_guard,
                    lifetime,
                },
                on_status,
            },
        ))
    }
}

#[derive(Clone)]
struct GrpcConnector {
    http: HttpConnector,
    tls: Option<TlsTarget>,
}

impl Service<http::Uri> for GrpcConnector {
    type Response = TokioIo<TransportStream>;
    type Error = BoxError;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.http.poll_ready(cx).map_err(BoxError::from)
    }

    fn call(&mut self, uri: http::Uri) -> Self::Future {
        let mut http = self.http.clone();
        let tls = self.tls.clone();
        Box::pin(async move {
            let dial = async move {
                let tcp = http.call(uri).await?.into_inner();
                match tls {
                    None => Ok::<_, BoxError>(TokioIo::new(Either::Left(tcp))),
                    Some((connector, server_name)) => {
                        let tls_stream = connector.connect(server_name, tcp).await?;
                        if tls_stream.get_ref().1.alpn_protocol() != Some(crate::tls::ALPN_H2) {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "TLS peer did not negotiate h2 ALPN",
                            )
                            .into());
                        }
                        Ok(TokioIo::new(Either::Right(tls_stream)))
                    }
                }
            };
            tokio::time::timeout(CONNECT_TIMEOUT, dial)
                .await
                .map_err(|elapsed| std::io::Error::new(std::io::ErrorKind::TimedOut, elapsed))?
        })
    }
}

fn parse_destination(
    raw: &str,
    security: ClientSecurity<'_>,
) -> Result<(http::Uri, ClientOrigin, Option<TlsTarget>), ClientError> {
    if raw.contains('#')
        || shared_rules::uri::has_whitespace_or_control(raw)
        || shared_rules::uri::authority_has_userinfo(raw)
    {
        return Err(ClientError::InvalidDestination);
    }
    let uri = http::Uri::try_from(raw).map_err(|_| ClientError::InvalidDestination)?;
    let parts = OriginParts::parse(&uri).ok_or(ClientError::InvalidDestination)?;
    let origin = ClientOrigin::from_parts(&parts);
    match (security, parts.scheme) {
        (ClientSecurity::Plaintext, OriginScheme::Http) => Ok((uri, origin, None)),
        (ClientSecurity::Tls(material), OriginScheme::Https) => {
            let unbracketed_host = unbracket_host(parts.host);
            let server_name = rustls::pki_types::ServerName::try_from(unbracketed_host)
                .map_err(|_| ClientError::InvalidDestination)?
                .to_owned();
            let config = crate::tls::client_tls_config(&material)?;
            Ok((uri, origin, Some((TlsConnector::from(config), server_name))))
        }
        (ClientSecurity::Plaintext, OriginScheme::Https)
        | (ClientSecurity::Tls(_), OriginScheme::Http) => {
            Err(ClientError::DestinationSecurityMismatch)
        }
    }
}

struct CallContexts {
    opening: OperationContext,
    lifetime: LifetimeGuard,
    watch_stop: bool,
}

fn derive_call_contexts(
    timeout: ClientTimeout,
    started_at: Instant,
    caller_timeout: Option<Duration>,
    parent: Option<&OperationContext>,
) -> CallContexts {
    let (opening_before_parent, lifetime_before_parent) =
        timeout.deadlines(started_at, caller_timeout);
    let parent_deadline = parent.and_then(OperationContext::deadline);
    let cancellation = parent.map_or_else(tokio_util::sync::CancellationToken::new, |parent| {
        parent.cancellation().child_token()
    });
    let opening = OperationContext::new(
        earliest(Some(opening_before_parent), parent_deadline),
        cancellation.clone(),
    );
    let lifetime = OperationContext::new(
        earliest(lifetime_before_parent, parent_deadline),
        cancellation,
    );
    let watch_stop = parent.is_some() || lifetime.deadline().is_some();
    CallContexts {
        opening,
        lifetime: cancel_on_drop(lifetime),
        watch_stop,
    }
}

fn acquire_opening_permit(
    opening_slots: Option<&Arc<Semaphore>>,
    opening: &OperationContext,
) -> Result<Option<OwnedSemaphorePermit>, tonic::Status> {
    check_open(opening)?;
    let Some(opening_slots) = opening_slots else {
        return Ok(None);
    };
    Arc::clone(opening_slots)
        .try_acquire_owned()
        .map(Some)
        .map_err(|_| {
            opening.stopped().map_or_else(
                || tonic::Status::resource_exhausted("gRPC client openings at capacity"),
                client_stopped_status,
            )
        })
}

fn earliest(left: Option<Deadline>, right: Option<Deadline>) -> Option<Deadline> {
    left.into_iter().chain(right).reduce(Deadline::earlier)
}

fn apply_forwarded_timeout(
    headers: &mut http::HeaderMap,
    forwarded: Option<Deadline>,
) -> Result<(), tonic::Status> {
    let Some(deadline) = forwarded else {
        headers.remove(GRPC_TIMEOUT);
        return Ok(());
    };
    let remaining = deadline.remaining();
    if remaining.is_zero() {
        return Err(client_deadline_status());
    }
    set_grpc_timeout(headers, remaining);
    Ok(())
}

fn report_transport_failure(error: &tonic::transport::Error) -> tonic::Status {
    if error_chain::contains::<tonic::TimeoutExpired>(error) {
        return client_deadline_status();
    }
    tracing::warn!(error = %DisplayErrorChain(error), "grpc_client_transport_failed");
    tonic::Status::unavailable("transport unavailable")
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use tls_test_support::TlsMaterial;
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    use super::*;
    use crate::ClientTlsError;

    #[tokio::test]
    async fn strict_destination_validation_rejects_malformed_and_mismatched_uris() {
        for invalid in [
            "",
            "example.test:50051",
            "http:///missing",
            "https:///missing",
            "https://user@example.test",
            "http://user:pass@example.test:50051",
            "https://example.test/path",
            "http://example.test/v1/echo",
            "https://example.test?q=1",
            "https://example.test/?q=1",
            "https://example.test#frag",
            " https://example.test",
            "https://example.test\n",
            "unix:/tmp/grpc.sock",
            "unix:///tmp/grpc.sock",
            "ftp://example.test",
        ] {
            for security in [ClientSecurity::Plaintext, ClientSecurity::tls()] {
                assert_matches!(
                    Client::builder(invalid, security, Duration::from_secs(1))
                        .build()
                        .unwrap_err(),
                    ClientError::InvalidDestination,
                    "{invalid:?}"
                );
            }
        }

        assert_matches!(
            Client::builder(
                "https://example.test",
                ClientSecurity::Plaintext,
                Duration::from_secs(1),
            )
            .build()
            .unwrap_err(),
            ClientError::DestinationSecurityMismatch
        );
        assert_matches!(
            Client::builder(
                "http://example.test",
                ClientSecurity::tls(),
                Duration::from_secs(1),
            )
            .build()
            .unwrap_err(),
            ClientError::DestinationSecurityMismatch
        );
        assert_matches!(
            Client::builder(
                "http://example.test",
                ClientSecurity::Plaintext,
                Duration::from_secs(1),
            )
            .opening_limit(NonZeroUsize::new(Semaphore::MAX_PERMITS + 1))
            .build(),
            Err(ClientError::InvalidOpeningLimit)
        );

        let bad_ca = ClientSecurity::Tls(ClientTlsMaterial {
            ca_certificate_pem: Some(
                "-----BEGIN CERTIFICATE-----\nbm90LWEtY2VydA==\n-----END CERTIFICATE-----\n",
            ),
            identity: None,
        });
        assert_matches!(
            Client::builder("https://example.test", bad_ca, Duration::from_secs(1))
                .build()
                .unwrap_err(),
            ClientError::Tls(ClientTlsError::InvalidCaCertificate(Some(_)))
        );

        for valid_http in [
            "http://example.test",
            "http://example.test/",
            "http://[::1]:50051",
        ] {
            Client::builder(
                valid_http,
                ClientSecurity::Plaintext,
                Duration::from_secs(1),
            )
            .build()
            .unwrap();
        }
        for valid_https in [
            "https://example.test",
            "https://example.test/",
            "https://[::1]:50051",
            "https://127.0.0.1:50051",
        ] {
            Client::builder(valid_https, ClientSecurity::tls(), Duration::from_secs(1))
                .build()
                .unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn budgets_and_forwarded_timeout_preserve_entry_origin_and_strip_invalid_headers() {
        let origin = Instant::now();
        let local = Duration::from_secs(2);
        let caller_budget = Duration::from_secs(5);
        let (opening, full_life) =
            ClientTimeout::FullRpc(local).deadlines(origin, Some(caller_budget));
        let (_, open_only_life) =
            ClientTimeout::OpeningOnly(local).deadlines(origin, Some(caller_budget));
        assert!(
            ClientTimeout::OpeningOnly(local)
                .deadlines(origin, None)
                .1
                .is_none()
        );

        tokio::time::advance(Duration::from_secs(1)).await;
        let mut headers = http::HeaderMap::new();
        apply_forwarded_timeout(&mut headers, Some(Deadline::new(origin, caller_budget))).unwrap();
        assert_eq!(grpc_timeout(&headers), Some(Duration::from_secs(4)));
        assert_eq!(opening.remaining(), Duration::from_secs(1));
        assert_eq!(full_life.unwrap().remaining(), Duration::from_secs(1));
        assert_eq!(open_only_life.unwrap().remaining(), Duration::from_secs(4));

        apply_forwarded_timeout(&mut headers, Some(Deadline::new(origin, Duration::MAX))).unwrap();
        assert_eq!(headers[GRPC_TIMEOUT], "99999999H");

        tokio::time::advance(Duration::from_secs(4)).await;
        assert_eq!(
            apply_forwarded_timeout(&mut headers, Some(Deadline::new(origin, caller_budget)))
                .unwrap_err()
                .code(),
            tonic::Code::DeadlineExceeded
        );

        let client = Client::builder(
            "http://127.0.0.1:1",
            ClientSecurity::Plaintext,
            ClientTimeout::OpeningOnly(Duration::from_secs(10)),
        )
        .build()
        .unwrap();
        let mut request = Request::new(Body::empty());
        request
            .headers_mut()
            .insert(GRPC_TIMEOUT, http::HeaderValue::from_static("+5S"));
        let prepared = PreparedCall::prepare(&client, request).unwrap();
        assert!(!prepared.watch_stop);
        assert!(!prepared.request.headers().contains_key(GRPC_TIMEOUT));
    }

    #[tokio::test]
    async fn with_context_propagates_cancellation_and_refuses_competing_request_context() {
        let client = Client::builder(
            "http://127.0.0.1:1",
            ClientSecurity::Plaintext,
            Duration::from_secs(5),
        )
        .build()
        .unwrap();
        let cancelled = OperationContext::unbounded();
        cancelled.cancel();
        let status = client
            .with_context(&cancelled)
            .oneshot(Request::new(Body::empty()))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::Cancelled);

        let mut competing = Request::new(Body::empty());
        competing
            .extensions_mut()
            .insert(OperationContext::unbounded());
        let conflict = client
            .with_context(&OperationContext::unbounded())
            .oneshot(competing)
            .await
            .unwrap_err();
        assert_eq!(conflict.code(), tonic::Code::Internal);
        assert_eq!(
            conflict.message(),
            "request operation context conflicts with client context"
        );
    }

    #[tokio::test]
    async fn tls_connector_rejects_peer_without_h2_alpn() {
        let material = TlsMaterial::new("127.0.0.1");
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(material.key));
        let mut server_config = infra_grpc_core::tls::tls13_server_builder()
            .with_no_client_auth()
            .with_single_cert(vec![material.cert.into()], key)
            .unwrap();
        server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(server_config));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let _ = acceptor.accept(tcp).await;
        });

        let mut client = Client::builder(
            &format!("https://{address}"),
            ClientSecurity::Tls(ClientTlsMaterial {
                ca_certificate_pem: Some(&material.root_pem),
                identity: None,
            }),
            Duration::from_secs(5),
        )
        .build()
        .unwrap();
        let request = Request::builder()
            .method("POST")
            .uri("/example.v1.Echo/Unary")
            .header("content-type", "application/grpc")
            .body(Body::empty())
            .unwrap();
        let status = client.call(request).await.unwrap_err();
        assert_eq!(status.code(), tonic::Code::Unavailable);
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn response_future_resolves_immediate_and_pending_outcomes() {
        let ready = ResponseFuture::from_prepared(
            Err(tonic::Status::permission_denied("refused")),
            PreparedCall::send,
        );
        assert_eq!(format!("{ready:?}"), "ResponseFuture { .. }");
        assert_eq!(
            ready.await.unwrap_err().code(),
            tonic::Code::PermissionDenied
        );

        let client = Client::builder(
            "http://127.0.0.1:1",
            ClientSecurity::Plaintext,
            Duration::from_secs(1),
        )
        .build()
        .unwrap();
        let prepared = PreparedCall::prepare(&client, Request::new(Body::empty())).unwrap();
        let opening = prepared.opening_context().clone();
        assert!(!opening.cancellation().is_cancelled());
        let pending = ResponseFuture::from_prepared(Ok(prepared), |prepared| async move {
            drop(prepared);
            Ok(Response::new(Body::empty()))
        });
        assert_eq!(format!("{pending:?}"), "ResponseFuture { .. }");
        assert!(pending.await.is_ok());
        assert!(opening.cancellation().is_cancelled());
    }
}
