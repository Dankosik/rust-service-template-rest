use std::num::NonZeroUsize;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::future::BoxFuture;
use http::{Request, Response};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioIo;
use operation_context::{Deadline, OperationContext};
use secrecy::SecretString;
use shared_rules::error_chain::{DisplayErrorChain, causes};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_util::either::Either;
use tokio_util::sync::DropGuard;
use tonic::body::Body;
use tonic::client::{ResponseStatusObserver as DecodedStatusObserver, TrackResponseStatus};
use tonic::transport::{Channel, Endpoint};
use tower::{Service, ServiceExt as _};
use tracing::Instrument as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;

use crate::ClientError;
use crate::call::{
    ClientCustody, ClientGuards, ClientObservation, StatusHook, attach_client_response,
    client_deadline, client_stopped, upload,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HTTP2_CONNECTION_WINDOW: u32 = 8 * 1024 * 1024;
const HTTP2_STREAM_WINDOW: u32 = 2 * 1024 * 1024;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type TlsTarget = (TlsConnector, rustls::pki_types::ServerName<'static>);
type TransportStream = Either<TcpStream, TlsStream<TcpStream>>;

#[derive(Clone, Debug)]
pub enum ClientSecurity {
    Plaintext,
    Tls(ClientTlsMaterial),
}

#[derive(Clone, derive_more::Debug, Default)]
pub struct ClientTlsMaterial {
    #[debug(skip)]
    pub ca_certificate_pem: Option<String>,
    pub identity: Option<ClientIdentity>,
}

#[derive(Clone, derive_more::Debug)]
pub struct ClientIdentity {
    #[debug(skip)]
    pub certificate_pem: String,
    #[debug(skip)]
    pub private_key_pem: SecretString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientTimeout {
    FullRpc(Duration),
    OpeningOnly(Duration),
}

impl ClientTimeout {
    fn deadlines(self, origin: Instant, caller: Option<Duration>) -> (Deadline, Option<Deadline>) {
        let (Self::FullRpc(local) | Self::OpeningOnly(local)) = self;
        let opening = Deadline::new(origin, caller.map_or(local, |budget| budget.min(local)));
        let lifetime = match self {
            Self::FullRpc(_) => Some(opening),
            Self::OpeningOnly(_) => caller.map(|budget| Deadline::new(origin, budget)),
        };
        (opening, lifetime)
    }
}

#[derive(Clone, derive_more::Debug)]
pub struct Client {
    channel: Channel,
    uses_tls: bool,
    #[debug(skip)]
    origin: http::Uri,
    #[debug(skip)]
    destination: crate::observe::Destination,
    timeout: ClientTimeout,
    openings: Option<Arc<Semaphore>>,
    series: &'static crate::observe::Series,
}

impl Client {
    pub fn try_new(
        destination: &str,
        security: ClientSecurity,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        Self::with_timeout_policy(destination, security, ClientTimeout::FullRpc(timeout))
    }

    pub fn with_timeout_policy(
        destination: &str,
        security: ClientSecurity,
        timeout: ClientTimeout,
    ) -> Result<Self, ClientError> {
        Self::with_opening_limit(destination, security, timeout, None)
    }

    pub fn with_opening_limit(
        destination: &str,
        security: ClientSecurity,
        timeout: ClientTimeout,
        opening_limit: Option<NonZeroUsize>,
    ) -> Result<Self, ClientError> {
        if opening_limit.is_some_and(|limit| limit.get() > Semaphore::MAX_PERMITS) {
            return Err(ClientError::InvalidOpeningLimit);
        }
        let (uri, tls) = parse_destination(destination, security)?;
        let endpoint = Endpoint::from(uri.clone())
            .connect_timeout(CONNECT_TIMEOUT)
            .http2_keep_alive_interval(Duration::from_mins(1))
            .keep_alive_timeout(Duration::from_secs(20))
            .initial_connection_window_size(HTTP2_CONNECTION_WINDOW)
            .initial_stream_window_size(HTTP2_STREAM_WINDOW);
        let destination = crate::observe::Destination::new(endpoint.uri());
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        http.set_nodelay(true);
        http.set_connect_timeout(Some(CONNECT_TIMEOUT));
        http.set_keepalive(Some(Duration::from_mins(1)));
        http.set_keepalive_interval(Some(Duration::from_secs(15)));
        http.set_keepalive_retries(Some(3));
        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        http.set_tcp_user_timeout(Some(Duration::from_secs(30)));
        Ok(Self {
            uses_tls: tls.is_some(),
            origin: uri,
            channel: Channel::new(GrpcConnector { http, tls }, endpoint),
            destination,
            timeout,
            openings: opening_limit.map(|limit| Arc::new(Semaphore::new(limit.get()))),
            series: crate::observe::Series::client(),
        })
    }

    #[must_use]
    pub const fn uses_tls(&self) -> bool {
        self.uses_tls
    }

    #[must_use]
    pub fn matches_origin(&self, origin: &http::Uri) -> bool {
        let (Some((scheme, host, port)), Some((other_scheme, other_host, other_port))) =
            (origin_parts(&self.origin), origin_parts(origin))
        else {
            return false;
        };
        scheme == other_scheme && host.eq_ignore_ascii_case(other_host) && port == other_port
    }

    pub fn prepare_call(&self, mut request: Request<Body>) -> Result<PreparedCall, tonic::Status> {
        let origin = Instant::now();
        let caller = crate::grpc_timeout(request.headers());
        if caller.is_none() {
            request.headers_mut().remove(crate::protocol::GRPC_TIMEOUT);
        }
        let (local_opening, local_lifetime) = self.timeout.deadlines(origin, caller);
        let parent = request.extensions().get::<OperationContext>();
        let has_parent = parent.is_some();
        let parent_deadline = parent.and_then(OperationContext::deadline);
        let cancellation = parent.map_or_else(tokio_util::sync::CancellationToken::new, |parent| {
            parent.cancellation().child_token()
        });
        let opening = OperationContext::new(
            earliest(Some(local_opening), parent_deadline),
            cancellation.clone(),
        );
        let lifetime = OperationContext::new(
            earliest(local_lifetime, parent_deadline),
            cancellation.clone(),
        );
        let watch_stop = has_parent || lifetime.deadline().is_some();
        let call = crate::observe::Call::start(self.series, &request, &self.destination);
        otel_http::inject_context(
            &tracing_opentelemetry_instrumentation_sdk::find_context_from_tracing(call.span()),
            request.headers_mut(),
        );
        if let Err(reason) = opening.check() {
            let status = client_stopped(reason);
            call.finish(status.code());
            return Err(status);
        }
        let permit = match &self.openings {
            Some(openings) => match Arc::clone(openings).try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    let status = opening.stopped().map_or_else(
                        || tonic::Status::resource_exhausted("gRPC client openings at capacity"),
                        client_stopped,
                    );
                    call.finish(status.code());
                    return Err(status);
                }
            },
            None => None,
        };
        Ok(PreparedCall {
            channel: self.channel.clone(),
            request,
            opening,
            lifetime,
            watch_stop,
            call,
            guard: cancellation.drop_guard(),
            permit,
        })
    }

    #[must_use]
    pub fn refuse_call(&self, request: &Request<Body>, status: tonic::Status) -> tonic::Status {
        crate::observe::Call::start(self.series, request, &self.destination).finish(status.code());
        status
    }
}

impl Service<Request<Body>> for Client {
    type Response = Response<Body>;
    type Error = tonic::Status;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        match self.prepare_call(request) {
            Ok(prepared) => Box::pin(prepared.send()),
            Err(status) => Box::pin(std::future::ready(Err(status))),
        }
    }
}

#[derive(derive_more::Debug)]
pub struct PreparedCall {
    #[debug(skip)]
    channel: Channel,
    #[debug(skip)]
    request: Request<Body>,
    opening: OperationContext,
    lifetime: OperationContext,
    watch_stop: bool,
    #[debug(skip)]
    call: crate::observe::Call,
    #[debug(skip)]
    guard: DropGuard,
    #[debug(skip)]
    permit: Option<OwnedSemaphorePermit>,
}

impl PreparedCall {
    #[must_use]
    pub const fn opening_context(&self) -> &OperationContext {
        &self.opening
    }

    pub fn headers_mut(&mut self) -> &mut http::HeaderMap {
        self.request.headers_mut()
    }

    #[must_use]
    pub fn refuse(self, status: tonic::Status) -> tonic::Status {
        self.call.finish(status.code());
        status
    }

    pub async fn send(self) -> Result<Response<Body>, tonic::Status> {
        self.send_inner(|_| Ok(()), None).await
    }

    pub async fn send_with_dispatch_check(
        self,
        check: impl FnOnce() -> Result<(), tonic::Status> + Send,
        on_status: impl FnOnce(tonic::Code) + Send + 'static,
    ) -> Result<Response<Body>, tonic::Status> {
        self.send_inner(move |_| check(), Some(Box::new(on_status)))
            .await
    }

    pub(crate) async fn send_inner(
        self,
        prepare_dispatch: impl FnOnce(&mut http::HeaderMap) -> Result<(), tonic::Status> + Send,
        on_status: Option<StatusHook>,
    ) -> Result<Response<Body>, tonic::Status> {
        let Self {
            mut channel,
            request,
            opening,
            lifetime,
            watch_stop,
            call,
            guard,
            permit,
        } = self;
        let (mut parts, body) = request.into_parts();
        let track_decoded_status = parts.extensions.remove::<TrackResponseStatus>().is_some();
        let (body, upload) = upload(body);
        let mut request = Request::from_parts(parts, body);
        let result = async {
            let outcome = tokio::select! {
                biased;
                reason = opening.wait_stopped() => Err(client_stopped(reason)),
                dispatched = async {
                    let ready = channel.ready().await.map_err(|err| transport_status(&err))?;
                    opening.check().map_err(client_stopped)?;
                    prepare_dispatch(request.headers_mut())?;
                    opening.check().map_err(client_stopped)?;
                    apply_forwarded_timeout(request.headers_mut(), lifetime.deadline())?;
                    ready.call(request).await.map_err(|err| transport_status(&err))
                } => dispatched,
            };
            opening.check().map_err(client_stopped)?;
            outcome
        }
        .instrument(call.span().clone())
        .await;
        drop(permit);
        match result {
            Ok(mut response) => {
                let observation = if track_decoded_status {
                    let span = call.span().clone();
                    let observer = DecodedStatusObserver::new(move |code| call.finish(code));
                    response.extensions_mut().insert(observer.clone());
                    ClientObservation::Decoded { span, observer }
                } else {
                    ClientObservation::Transport(call)
                };
                Ok(attach_client_response(
                    response,
                    observation,
                    ClientCustody {
                        lifetime,
                        watch_stop,
                        guards: ClientGuards {
                            upload,
                            cancel_on_drop: Some(guard),
                        },
                        on_status,
                    },
                )
                .map(Body::new))
            }
            Err(status) => {
                call.finish(status.code());
                Err(status)
            }
        }
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
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.http
            .poll_ready(context)
            .map_err(|error| Box::new(error) as BoxError)
    }

    fn call(&mut self, uri: http::Uri) -> Self::Future {
        let mut http = self.http.clone();
        let tls = self.tls.clone();
        Box::pin(async move {
            let dial = async move {
                let tcp = http
                    .call(uri)
                    .await
                    .map_err(|error| Box::new(error) as BoxError)?;
                let tcp = tcp.into_inner();
                match tls {
                    None => Ok(TokioIo::new(Either::Left(tcp))),
                    Some((connector, server_name)) => {
                        let tls_stream = connector
                            .connect(server_name, tcp)
                            .await
                            .map_err(|error| Box::new(error) as BoxError)?;
                        if tls_stream.get_ref().1.alpn_protocol() != Some(b"h2") {
                            return Err(Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "TLS peer did not negotiate h2 ALPN",
                            )) as BoxError);
                        }
                        Ok(TokioIo::new(Either::Right(tls_stream)))
                    }
                }
            };
            tokio::time::timeout(CONNECT_TIMEOUT, dial)
                .await
                .unwrap_or_else(|elapsed| {
                    Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        elapsed,
                    )))
                })
        })
    }
}

fn origin_parts(uri: &http::Uri) -> Option<(&str, &str, u16)> {
    if !matches!(uri.path(), "" | "/") || uri.query().is_some() {
        return None;
    }
    let scheme = uri.scheme_str()?;
    let authority = uri.authority()?;
    let host = authority.host();
    let port = shared_rules::uri::http_origin_port(scheme, authority.as_str(), host)?;
    Some((scheme, host, port))
}

fn parse_destination(
    raw: &str,
    security: ClientSecurity,
) -> Result<(http::Uri, Option<TlsTarget>), ClientError> {
    if raw.contains('#')
        || shared_rules::uri::has_whitespace_or_control(raw)
        || shared_rules::uri::authority_has_userinfo(raw)
    {
        return Err(ClientError::InvalidDestination);
    }
    let uri = http::Uri::try_from(raw).map_err(|_| ClientError::InvalidDestination)?;
    let (scheme, host, _) = origin_parts(&uri).ok_or(ClientError::InvalidDestination)?;
    match security {
        ClientSecurity::Plaintext => {
            if scheme != "http" {
                return Err(ClientError::DestinationSecurityMismatch);
            }
            Ok((uri, None))
        }
        ClientSecurity::Tls(material) => {
            if scheme != "https" {
                return Err(ClientError::DestinationSecurityMismatch);
            }
            let trimmed_host = host
                .strip_prefix('[')
                .and_then(|inner| inner.strip_suffix(']'))
                .unwrap_or(host);
            let server_name = rustls::pki_types::ServerName::try_from(trimmed_host)
                .map_err(|_| ClientError::InvalidDestination)?
                .to_owned();
            let config = crate::tls::client_tls_config(&material)?;
            Ok((uri, Some((TlsConnector::from(config), server_name))))
        }
    }
}

fn earliest(left: Option<Deadline>, right: Option<Deadline>) -> Option<Deadline> {
    left.into_iter().chain(right).reduce(Deadline::earlier)
}

fn apply_forwarded_timeout(
    headers: &mut http::HeaderMap,
    forwarded: Option<Deadline>,
) -> Result<(), tonic::Status> {
    let Some(deadline) = forwarded else {
        headers.remove(crate::protocol::GRPC_TIMEOUT);
        return Ok(());
    };
    let remaining = deadline.remaining();
    if remaining.is_zero() {
        return Err(client_deadline());
    }
    crate::protocol::set_grpc_timeout(headers, remaining);
    Ok(())
}

fn transport_status(error: &tonic::transport::Error) -> tonic::Status {
    let error = error as &(dyn std::error::Error + 'static);
    if causes(error).any(<dyn std::error::Error>::is::<tonic::TimeoutExpired>) {
        return client_deadline();
    }
    tracing::warn!(error = %DisplayErrorChain(error), "grpc_client_transport_failed");
    tonic::Status::unavailable("transport unavailable")
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::sync::Arc;

    use rcgen::{
        BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa,
        KeyPair, KeyUsagePurpose,
    };
    use rustls::ServerConfig;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    use super::*;

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
            for security in [
                ClientSecurity::Plaintext,
                ClientSecurity::Tls(ClientTlsMaterial::default()),
            ] {
                assert_matches!(
                    Client::try_new(invalid, security, Duration::from_secs(1)).unwrap_err(),
                    ClientError::InvalidDestination,
                    "{invalid:?}"
                );
            }
        }

        assert_matches!(
            Client::try_new(
                "https://example.test",
                ClientSecurity::Plaintext,
                Duration::from_secs(1),
            )
            .unwrap_err(),
            ClientError::DestinationSecurityMismatch
        );
        assert_matches!(
            Client::try_new(
                "http://example.test",
                ClientSecurity::Tls(ClientTlsMaterial::default()),
                Duration::from_secs(1),
            )
            .unwrap_err(),
            ClientError::DestinationSecurityMismatch
        );
        assert_matches!(
            Client::with_opening_limit(
                "http://example.test",
                ClientSecurity::Plaintext,
                ClientTimeout::FullRpc(Duration::from_secs(1)),
                NonZeroUsize::new(Semaphore::MAX_PERMITS + 1),
            ),
            Err(ClientError::InvalidOpeningLimit)
        );

        let bad_ca = ClientSecurity::Tls(ClientTlsMaterial {
            ca_certificate_pem: Some(
                "-----BEGIN CERTIFICATE-----\nbm90LWEtY2VydA==\n-----END CERTIFICATE-----\n"
                    .to_owned(),
            ),
            identity: None,
        });
        assert_matches!(
            Client::try_new("https://example.test", bad_ca, Duration::from_secs(1)).unwrap_err(),
            ClientError::Tls(crate::TlsMaterialError::InvalidCaCertificate(Some(_)))
        );

        for valid_http in [
            "http://example.test",
            "http://example.test/",
            "http://[::1]:50051",
        ] {
            Client::try_new(
                valid_http,
                ClientSecurity::Plaintext,
                Duration::from_secs(1),
            )
            .unwrap();
        }
        for valid_https in [
            "https://example.test",
            "https://example.test/",
            "https://[::1]:50051",
            "https://127.0.0.1:50051",
        ] {
            Client::try_new(
                valid_https,
                ClientSecurity::Tls(ClientTlsMaterial::default()),
                Duration::from_secs(1),
            )
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
        assert_eq!(crate::grpc_timeout(&headers), Some(Duration::from_secs(4)));
        assert_eq!(opening.remaining(), Duration::from_secs(1));
        assert_eq!(full_life.unwrap().remaining(), Duration::from_secs(1));
        assert_eq!(open_only_life.unwrap().remaining(), Duration::from_secs(4));

        apply_forwarded_timeout(&mut headers, Some(Deadline::new(origin, Duration::MAX))).unwrap();
        assert_eq!(headers[crate::protocol::GRPC_TIMEOUT], "99999999H");

        tokio::time::advance(Duration::from_secs(4)).await;
        assert_eq!(
            apply_forwarded_timeout(&mut headers, Some(Deadline::new(origin, caller_budget)))
                .unwrap_err()
                .code(),
            tonic::Code::DeadlineExceeded
        );

        let client = Client::with_timeout_policy(
            "http://127.0.0.1:1",
            ClientSecurity::Plaintext,
            ClientTimeout::OpeningOnly(Duration::from_secs(10)),
        )
        .unwrap();
        let mut request = Request::new(Body::empty());
        request.headers_mut().insert(
            crate::protocol::GRPC_TIMEOUT,
            http::HeaderValue::from_static("+5S"),
        );
        let mut prepared = client.prepare_call(request).unwrap();
        assert!(!prepared.watch_stop);
        assert!(
            !prepared
                .headers_mut()
                .contains_key(crate::protocol::GRPC_TIMEOUT)
        );
    }

    #[tokio::test]
    async fn tls_connector_rejects_peer_without_h2_alpn() {
        let mut ca_params = CertificateParams::default();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let issuer = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
        let server_key = KeyPair::generate().unwrap();
        let mut server_params = CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_cert = server_params.signed_by(&server_key, &issuer).unwrap();

        let certs = CertificateDer::pem_slice_iter(server_cert.pem().as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let key = PrivateKeyDer::from_pem_slice(server_key.serialize_pem().as_bytes()).unwrap();
        let mut server_config = ServerConfig::builder_with_provider(
            rustls::crypto::aws_lc_rs::default_provider().into(),
        )
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
        server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(server_config));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let _ = acceptor.accept(tcp).await;
        });

        let mut client = Client::try_new(
            &format!("https://{address}"),
            ClientSecurity::Tls(ClientTlsMaterial {
                ca_certificate_pem: Some(issuer.pem()),
                identity: None,
            }),
            Duration::from_secs(5),
        )
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
}
