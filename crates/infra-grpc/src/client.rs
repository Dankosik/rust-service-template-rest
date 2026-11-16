use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use http::{Request, Response};
use hyper_util::client::legacy::connect::HttpConnector;
use secrecy::{ExposeSecret as _, SecretString};
use tonic::body::Body;
use tonic::client::{ResponseStatusObserver as DecodedStatusObserver, TrackResponseStatus};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tower::{Service as _, ServiceExt as _};
use tracing::Instrument as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;

use crate::Error;
use crate::call::{Held, Observation, ResponseCustody, ResponseStatusObserver, Side};
use operation_context::{Deadline, OperationContext};
use shared_rules::error_chain::DisplayErrorChain;
use tokio::time::Instant;
use tokio_util::sync::DropGuard;

#[derive(Clone, Debug)]
pub enum ClientSecurity {
    Plaintext,
    Tls(ClientTlsMaterial),
}

#[derive(Clone, Debug, Default)]
pub struct ClientTlsMaterial {
    pub ca_certificate_pem: Option<String>,
    pub identity: Option<ClientIdentity>,
}

#[derive(Clone, Debug)]
pub struct ClientIdentity {
    pub certificate_pem: String,
    pub private_key_pem: SecretString,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
pub enum ClientTimeout {
    FullRpc(Duration),
    OpeningOnly(Duration),
}

impl ClientTimeout {
    fn deadlines(self, origin: Instant, caller: Option<Duration>) -> (Deadline, Option<Deadline>) {
        let (Self::FullRpc(local) | Self::OpeningOnly(local)) = self;
        let opening = Deadline::new(origin, caller.map_or(local, |caller| caller.min(local)));
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
    #[debug(skip)]
    destination: http::Uri,
    timeout: ClientTimeout,
    series: Arc<crate::observe::Series>,
}

impl Client {
    pub fn try_new(
        destination: &str,
        security: ClientSecurity,
        timeout: Duration,
    ) -> Result<Self, Error> {
        Self::with_timeout_policy(destination, security, ClientTimeout::FullRpc(timeout))
    }

    pub fn with_timeout_policy(
        destination: &str,
        security: ClientSecurity,
        timeout: ClientTimeout,
    ) -> Result<Self, Error> {
        let destination =
            http::Uri::try_from(destination).map_err(|_| Error::InvalidDestination)?;
        let mut endpoint = Endpoint::from(destination)
            .connect_timeout(CONNECT_TIMEOUT)
            .http2_keep_alive_interval(Duration::from_mins(1))
            .keep_alive_timeout(Duration::from_secs(20))
            .initial_connection_window_size(infra_http::HTTP2_CONNECTION_WINDOW)
            .initial_stream_window_size(infra_http::HTTP2_STREAM_WINDOW);
        let expected_scheme = match security {
            ClientSecurity::Plaintext => "http",
            ClientSecurity::Tls(_) => "https",
        };
        if endpoint.uri().scheme_str() != Some(expected_scheme) {
            return Err(Error::DestinationSecurityMismatch);
        }
        if let ClientSecurity::Tls(material) = security {
            endpoint = endpoint
                .tls_config(client_tls(&material)?)
                .map_err(|_| Error::InvalidClientTls)?;
        }
        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        connector.set_nodelay(true);
        connector.set_keepalive(Some(Duration::from_mins(1)));
        connector.set_connect_timeout(Some(CONNECT_TIMEOUT));
        Ok(Self {
            channel: endpoint.connect_with_connector_lazy(connector),
            destination: endpoint.uri().clone(),
            timeout,
            series: crate::observe::Series::client(),
        })
    }
}

impl tower::Service<Request<Body>> for Client {
    type Response = Response<Body>;
    type Error = tonic::Status;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        Box::pin(self.prepare_call(request).send())
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
    #[debug(skip)]
    forwarded: Option<Deadline>,
    #[debug(skip)]
    call: crate::observe::Call,
    #[debug(skip)]
    guard: DropGuard,
}

impl Client {
    #[must_use]
    pub fn prepare_call(&self, mut request: Request<Body>) -> PreparedCall {
        let origin = Instant::now();
        let caller = crate::grpc_timeout(request.headers());
        if caller.is_none() {
            request.headers_mut().remove(crate::protocol::GRPC_TIMEOUT);
        }
        let (local_opening, local_lifetime) = self.timeout.deadlines(origin, caller);
        let parent = request.extensions().get::<OperationContext>();
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
        let forwarded = earliest(
            caller.map(|budget| Deadline::new(origin, budget)),
            parent_deadline,
        );
        let call = crate::observe::Call::start(&self.series, &request, Some(&self.destination));
        otel_http::inject_context(
            &tracing_opentelemetry_instrumentation_sdk::find_context_from_tracing(call.span()),
            request.headers_mut(),
        );
        PreparedCall {
            channel: self.channel.clone(),
            request,
            opening,
            lifetime,
            forwarded,
            call,
            guard: cancellation.drop_guard(),
        }
    }
}

fn earliest(left: Option<Deadline>, right: Option<Deadline>) -> Option<Deadline> {
    left.into_iter().chain(right).reduce(Deadline::earlier)
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

    pub async fn send_with_response_status(
        self,
        on_status: impl FnOnce(tonic::Code) + Send + 'static,
    ) -> Result<Response<Body>, tonic::Status> {
        self.send_with_dispatch_check(|| Ok(()), on_status).await
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
        on_status: Option<ResponseStatusObserver>,
    ) -> Result<Response<Body>, tonic::Status> {
        let Self {
            mut channel,
            request,
            opening,
            lifetime,
            forwarded,
            call,
            guard,
        } = self;
        let (mut parts, body) = request.into_parts();
        let track_decoded_status = parts.extensions.remove::<TrackResponseStatus>().is_some();
        let (body, upload) = crate::call::upload(body);
        let mut request = Request::from_parts(parts, body);
        let stopped = |reason| Side::Client.stopped(reason);
        let result = async {
            let result = tokio::select! {
                biased;
                reason = opening.wait_stopped() => Err(stopped(reason)),
                result = async {
                    let ready = channel.ready().await.map_err(transport_status)?;
                    opening.check().map_err(stopped)?;
                    prepare_dispatch(request.headers_mut())?;
                    opening.check().map_err(stopped)?;
                    apply_forwarded_timeout(request.headers_mut(), forwarded)?;
                    ready.call(request).await.map_err(transport_status)
                } => result,
            };
            opening.check().map_err(stopped)?;
            result
        }
        .instrument(call.span().clone())
        .await;
        match result {
            Ok(mut response) => {
                let observation = if track_decoded_status {
                    let span = call.span().clone();
                    let observer = DecodedStatusObserver::new(move |code| call.finish(code));
                    response.extensions_mut().insert(observer.clone());
                    Observation::Decoded { span, observer }
                } else {
                    Observation::Transport(call)
                };
                Ok(crate::call::attach(
                    response,
                    observation,
                    None,
                    ResponseCustody {
                        on_status,
                        context: Some(lifetime),
                        held: Held {
                            permit: None,
                            upload: Some(upload),
                            cancel_on_drop: Some(guard),
                        },
                    },
                    tonic::Status::code,
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
        return Err(Side::Client.deadline());
    }
    crate::protocol::set_grpc_timeout(headers, remaining);
    Ok(())
}

fn client_tls(material: &ClientTlsMaterial) -> Result<ClientTlsConfig, Error> {
    let mut config = match &material.ca_certificate_pem {
        Some(ca) => {
            crate::tls::roots(ca)?;
            ClientTlsConfig::new().ca_certificate(Certificate::from_pem(ca))
        }
        None => ClientTlsConfig::new().with_native_roots(),
    };
    if let Some(identity) = &material.identity {
        crate::tls::chain(&identity.certificate_pem)?;
        crate::tls::private_key(&identity.private_key_pem)?;
        config = config.identity(Identity::from_pem(
            &identity.certificate_pem,
            identity.private_key_pem.expose_secret(),
        ));
    }
    Ok(config)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "used as a `map_err` callback, which hands over the owned error"
)]
fn transport_status(error: tonic::transport::Error) -> tonic::Status {
    let error = &error as &(dyn std::error::Error + 'static);
    if shared_rules::error_chain::causes(error)
        .any(<dyn std::error::Error>::is::<tonic::TimeoutExpired>)
    {
        return Side::Client.deadline();
    }
    tracing::warn!(error = %DisplayErrorChain(error), "grpc_client_transport_failed");
    tonic::Status::unavailable("transport unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn budgets_and_forwarded_metadata_keep_the_adapter_entry_origin() {
        let origin = Instant::now();
        let local = Duration::from_secs(2);
        let supplied = Duration::from_secs(5);
        let (opening, full) = ClientTimeout::FullRpc(local).deadlines(origin, Some(supplied));
        let (_, long) = ClientTimeout::OpeningOnly(local).deadlines(origin, Some(supplied));
        assert!(
            ClientTimeout::OpeningOnly(local)
                .deadlines(origin, None)
                .1
                .is_none()
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        let mut headers = http::HeaderMap::new();
        apply_forwarded_timeout(&mut headers, Some(Deadline::new(origin, supplied))).unwrap();
        assert_eq!(crate::grpc_timeout(&headers), Some(Duration::from_secs(4)));
        assert_eq!(opening.remaining(), Duration::from_secs(1));
        assert_eq!(full.unwrap().remaining(), Duration::from_secs(1));
        assert_eq!(long.unwrap().remaining(), Duration::from_secs(4));
        tokio::time::advance(Duration::from_secs(4)).await;
        assert_eq!(
            apply_forwarded_timeout(&mut headers, Some(Deadline::new(origin, supplied)))
                .unwrap_err()
                .code(),
            tonic::Code::DeadlineExceeded
        );
        let largest_wire = Duration::from_hours(99_999_999);
        let (opening, _) = ClientTimeout::FullRpc(local).deadlines(origin, Some(largest_wire));
        assert!(opening.expired());
        assert!(!Deadline::new(origin, largest_wire).expired());
        apply_forwarded_timeout(&mut headers, Some(Deadline::new(origin, Duration::MAX))).unwrap();
        assert_eq!(headers[crate::protocol::GRPC_TIMEOUT], "99999999H");
    }

    #[test]
    fn a_ca_that_is_not_a_certificate_fails_construction() {
        let garbage =
            "-----BEGIN CERTIFICATE-----\nbm90IGEgY2VydGlmaWNhdGU=\n-----END CERTIFICATE-----\n";
        let security = ClientSecurity::Tls(ClientTlsMaterial {
            ca_certificate_pem: Some(garbage.to_owned()),
            identity: None,
        });
        assert_eq!(
            Client::try_new("https://example.test", security, Duration::from_secs(1)).unwrap_err(),
            Error::Tls(crate::TlsMaterialError::InvalidCaCertificate)
        );
    }

    #[test]
    fn a_socket_destination_is_refused_in_either_security_mode() {
        for destination in ["unix:/tmp/grpc.sock", "unix:///tmp/grpc.sock"] {
            for security in [
                ClientSecurity::Plaintext,
                ClientSecurity::Tls(ClientTlsMaterial::default()),
            ] {
                assert_eq!(
                    Client::try_new(destination, security, Duration::from_secs(1)).unwrap_err(),
                    Error::InvalidDestination,
                    "{destination}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_transport_failure_is_logged_with_its_cause() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut channel = Endpoint::from(http::Uri::try_from(format!("http://{address}")).unwrap())
            .connect_lazy();
        let request = Request::builder()
            .uri(format!("http://{address}/example.v1.Echo/Unary"))
            .body(Body::empty())
            .unwrap();
        let error = channel
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "transport error");
        let logged = DisplayErrorChain(&error).to_string();
        assert!(logged.contains("Connection refused"), "{logged}");
    }

    #[tokio::test]
    async fn a_timeout_this_grammar_rejects_does_not_reach_the_channel() {
        let client = Client::try_new(
            "http://127.0.0.1:1",
            ClientSecurity::Plaintext,
            Duration::from_secs(10),
        )
        .unwrap();
        let mut request = Request::new(Body::empty());
        request.headers_mut().insert(
            crate::protocol::GRPC_TIMEOUT,
            http::HeaderValue::from_static("+5S"),
        );
        let mut prepared = client.prepare_call(request);
        assert!(
            !prepared
                .headers_mut()
                .contains_key(crate::protocol::GRPC_TIMEOUT)
        );
    }
}
