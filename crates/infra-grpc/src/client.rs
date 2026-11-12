use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use http::{Request, Response};
use hyper_util::client::legacy::connect::HttpConnector;
use secrecy::{ExposeSecret as _, SecretString};
use tonic::body::Body;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tower::{Service as _, ServiceExt as _};
use tracing::Instrument as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;

use crate::Error;
use crate::call::{Held, ResponseCustody, ResponseStatusObserver, Side};
use operation_context::{Deadline, OperationContext};
use shared_rules::error_chain::DisplayErrorChain;
use tokio::time::Instant;
use tokio_util::sync::DropGuard;

/// Explicit security selected for one trusted operator destination.
///
/// The destination scheme must agree: `http` for plaintext, `https` for TLS.
/// Tonic applies TLS only to `https`, so a mismatch is rejected rather than
/// silently connecting in plaintext.
#[derive(Clone, Debug)]
pub enum ClientSecurity {
    Plaintext,
    /// TLS-verified `https` connection with the given trust material.
    Tls(ClientTlsMaterial),
}

/// Trust material for normal TLS verification. Without a CA the native roots
/// are trusted. This adapter never loads a path or disables hostname
/// verification.
#[derive(Clone, Debug, Default)]
pub struct ClientTlsMaterial {
    /// PEM CA certificate to trust; `None` trusts the native roots.
    pub ca_certificate_pem: Option<String>,
    /// Client certificate and key for mTLS; `None` presents no client identity.
    pub identity: Option<ClientIdentity>,
}

/// Client certificate and key for mTLS.
#[derive(Clone, Debug)]
pub struct ClientIdentity {
    pub certificate_pem: String,
    pub private_key_pem: SecretString,
}

/// Bounds both the endpoint's dial and the connector's TCP connect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The interval promised by this adapter's required local budget.
#[derive(Clone, Copy, Debug)]
pub enum ClientTimeout {
    /// Bounds readiness, queueing, response headers, DATA and terminal trailers.
    FullRpc(Duration),
    /// Bounds opening only. A supplied caller deadline still bounds the whole RPC.
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

/// One lazy, shared channel for a configured dependency.
///
/// Construction performs neither DNS nor network I/O. Each call injects the
/// current trace context and records the client span and metrics when the
/// call's status arrives: in the response headers of a Trailers-Only answer,
/// otherwise in the trailers that end the response stream.
///
/// An [`OperationContext`] in the request extensions, such as the one a
/// handler received, also bounds the call: the earliest of its deadline and
/// this client's wins for opening and lifetime, and its cancellation cancels
/// the call.
#[derive(Clone, Debug)]
pub struct Client {
    channel: Channel,
    timeout: ClientTimeout,
    series: Arc<crate::observe::Series>,
}

impl Client {
    /// Parses an explicit trusted destination and constructs a lazy channel.
    ///
    /// `timeout` bounds the entire RPC from adapter entry through readiness,
    /// queueing, headers, DATA and trailers. A shorter caller `grpc-timeout`
    /// wins; the local policy itself adds no wire metadata. Use
    /// [`Self::with_timeout_policy`] with [`ClientTimeout::OpeningOnly`] for
    /// intentionally long-lived streams. Cancellation does not imply rollback
    /// or that a request already handed to tonic was never dispatched.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDestination`] for a destination that is not a URI,
    /// including tonic's `unix:` socket form,
    /// [`Error::DestinationSecurityMismatch`] when its scheme disagrees with
    /// `security`, a certificate or key variant for unusable PEM material, and
    /// [`Error::InvalidClientTls`] when tonic cannot build the TLS connector.
    pub fn try_new(
        destination: &str,
        security: ClientSecurity,
        timeout: Duration,
    ) -> Result<Self, Error> {
        Self::with_timeout_policy(destination, security, ClientTimeout::FullRpc(timeout))
    }

    /// Constructs the shared channel with an explicit RPC interval policy.
    ///
    /// Both policies have a finite opening budget. Supplied caller metadata
    /// always bounds the whole RPC and is reduced by adapter readiness waiting
    /// before handoff to tonic; its internal queue/transit remain unobservable.
    ///
    /// # Errors
    ///
    /// Returns the same destination and TLS errors as [`Self::try_new`].
    pub fn with_timeout_policy(
        destination: &str,
        security: ClientSecurity,
        timeout: ClientTimeout,
    ) -> Result<Self, Error> {
        // Parsed as a URI rather than through `Endpoint::from_shared`, which
        // turns a `unix:` destination into a socket endpoint whose `uri()` is
        // the fixed `http://tonic` and would pass the scheme check below.
        let destination =
            http::Uri::try_from(destination).map_err(|_| Error::InvalidDestination)?;
        let mut endpoint = Endpoint::from(destination)
            .connect_timeout(CONNECT_TIMEOUT)
            // gRPC's keepalive guide asks clients not to ping much more
            // often than once a minute, and only while a call is open.
            .http2_keep_alive_interval(Duration::from_secs(60))
            .keep_alive_timeout(Duration::from_secs(20))
            // The listener's fixed windows; see `infra_http::HTTP2_CONNECTION_WINDOW`.
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
        connector.set_keepalive(Some(Duration::from_secs(60)));
        connector.set_connect_timeout(Some(CONNECT_TIMEOUT));
        Ok(Self {
            // This path places tonic's connect timer outside TLS as well as
            // DNS/TCP, so the shared channel can release a stalled full dial.
            channel: endpoint.connect_with_connector_lazy(connector),
            timeout,
            series: crate::observe::Series::client(),
        })
    }
}

impl tower::Service<Request<Body>> for Client {
    type Response = Response<Body>;
    type Error = tonic::Status;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    /// Always ready: each call waits for the channel itself, so a wrapper may
    /// clone this client and call it without driving readiness first.
    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        Box::pin(self.prepare_call(request).send())
    }
}

/// One concrete client's request and fixed cutoffs, captured before credentials.
/// Preparation performs no I/O and cannot be moved to another client's policy.
/// The call is already observed: dropped without [`PreparedCall::send`] or
/// [`PreparedCall::refuse`], it is recorded as `Canceled`.
pub struct PreparedCall {
    channel: Channel,
    request: Request<Body>,
    opening: OperationContext,
    lifetime: OperationContext,
    forwarded: Option<Deadline>,
    call: crate::observe::Call,
    guard: DropGuard,
}

impl std::fmt::Debug for PreparedCall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedCall")
            .field("opening", &self.opening)
            .field("lifetime", &self.lifetime)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Captures the original opening and response bounds before credential work.
    /// It starts the client span and counts the call as started.
    #[must_use]
    pub fn prepare_call(&self, mut request: Request<Body>) -> PreparedCall {
        let origin = Instant::now();
        let caller = crate::grpc_timeout(request.headers());
        if caller.is_none() {
            // Tonic's channel accepts values this grammar rejects, such as
            // `+5S`, and would apply a timeout this call treats as absent.
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
        let call = crate::observe::Call::start(&self.series, &request);
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
    match (left, right) {
        (Some(left), Some(right)) => Some(left.earlier(right)),
        (left, right) => left.or(right),
    }
}

impl PreparedCall {
    /// The fixed budget covering credentials, readiness and response headers.
    #[must_use]
    pub const fn opening_context(&self) -> &OperationContext {
        &self.opening
    }

    /// Metadata for credentials; changing it cannot reset the selected cutoffs.
    pub fn headers_mut(&mut self) -> &mut http::HeaderMap {
        self.request.headers_mut()
    }

    /// Ends this call without dispatch, recording the status its caller
    /// receives instead of the `Canceled` of an abandoned call.
    #[must_use]
    pub fn refuse(self, status: tonic::Status) -> tonic::Status {
        self.call.finish(status.code());
        status
    }

    /// Dispatches this prepared request within its original budget.
    ///
    /// # Errors
    /// Returns the existing deadline, cancellation, or transport status.
    pub async fn send(self) -> Result<Response<Body>, tonic::Status> {
        self.send_inner(None).await
    }

    /// Like [`Self::send`], with a synchronous callback for the peer status.
    /// It runs once when the status is observed, either at response headers
    /// or when the caller polls the stream to its end. Locally generated
    /// failures and abandoned bodies without an observed status skip it.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::send`].
    pub async fn send_with_response_status(
        self,
        on_status: impl FnOnce(tonic::Code) + Send + 'static,
    ) -> Result<Response<Body>, tonic::Status> {
        self.send_inner(Some(Box::new(on_status))).await
    }

    async fn send_inner(
        self,
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
        let (parts, body) = request.into_parts();
        let (body, upload) = crate::call::upload(body);
        let mut request = Request::from_parts(parts, body);
        let stopped = |reason| Side::Client.stopped(reason);
        let result = async {
            // Biased: an opening already stopped answers before any channel work.
            let result = tokio::select! {
                biased;
                reason = opening.wait_stopped() => Err(stopped(reason)),
                result = async {
                    let ready = channel.ready().await.map_err(transport_status)?;
                    opening.check().map_err(stopped)?;
                    if let Some(forwarded) = forwarded {
                        forward_timeout(request.headers_mut(), forwarded)?;
                    }
                    ready.call(request).await.map_err(transport_status)
                } => result,
            };
            // A terminal failure that becomes ready late also loses to the original cutoff.
            opening.check().map_err(stopped)?;
            result
        }
        .instrument(call.span().clone())
        .await;
        match result {
            Ok(response) => Ok(crate::call::attach(
                response,
                call,
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
            .map(Body::new)),
            Err(status) => {
                call.finish(status.code());
                Err(status)
            }
        }
    }
}

fn forward_timeout(headers: &mut http::HeaderMap, caller: Deadline) -> Result<(), tonic::Status> {
    let remaining = caller.remaining();
    if remaining.is_zero() {
        return Err(Side::Client.deadline());
    }
    crate::protocol::set_grpc_timeout(headers, remaining);
    Ok(())
}

/// Checks each PEM input first so a failure names it; tonic's own build error
/// does not say which input it rejected.
fn client_tls(material: &ClientTlsMaterial) -> Result<ClientTlsConfig, Error> {
    let mut config = match &material.ca_certificate_pem {
        Some(ca) => {
            // Parsed as trust anchors, not only as PEM: tonic drops a
            // certificate it cannot parse and still builds the connector.
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

/// The caller sees a fixed status, because a handler may forward it to its
/// own caller; the cause is logged inside the client span. When the call's
/// supplied `grpc-timeout` runs out inside tonic's channel, that is the
/// caller's deadline, not a transport fault: the answer is
/// `DEADLINE_EXCEEDED`, so a caller that retries `UNAVAILABLE` does not
/// repeat a call the server may still be running.
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
    // Tonic's transport error prints only `transport error` and keeps the DNS,
    // TLS, connect or HTTP/2 reason in `source()`.
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
        forward_timeout(&mut headers, Deadline::new(origin, supplied)).unwrap();
        assert_eq!(crate::grpc_timeout(&headers), Some(Duration::from_secs(4)));
        assert_eq!(opening.remaining(), Duration::from_secs(1));
        assert_eq!(full.unwrap().remaining(), Duration::from_secs(1));
        assert_eq!(long.unwrap().remaining(), Duration::from_secs(4));
        tokio::time::advance(Duration::from_secs(4)).await;
        assert_eq!(
            forward_timeout(&mut headers, Deadline::new(origin, supplied))
                .unwrap_err()
                .code(),
            tonic::Code::DeadlineExceeded
        );
        // Even legal wire timeouts larger than a practical Instant horizon
        // must not overflow while a finite local budget remains enforceable.
        let largest_wire = Duration::from_hours(99_999_999);
        let (opening, _) = ClientTimeout::FullRpc(local).deadlines(origin, Some(largest_wire));
        assert!(opening.expired());
        assert!(!Deadline::new(origin, largest_wire).expired());
        // A finite parent budget past the wire maximum is sent as that maximum.
        forward_timeout(&mut headers, Deadline::new(origin, Duration::MAX)).unwrap();
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
