use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use http::{Request, Response};
use opentelemetry::trace::SpanKind;
use secrecy::{ExposeSecret as _, SecretString};
use tonic::body::Body;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tower::ServiceExt as _;
use tracing::Instrument as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;

use crate::Error;
use crate::call::{Deadline, Lifetime, Side};
use tokio::time::Instant;

/// Explicit security selected for one trusted operator destination.
///
/// The destination scheme must agree: `http` for plaintext, `https` for TLS.
/// Tonic applies TLS only to `https`, so a mismatch is rejected rather than
/// silently connecting in plaintext.
#[derive(Clone, Debug)]
pub enum ClientSecurity {
    Plaintext,
    Tls(ClientTlsMaterial),
}

/// Trust material for normal TLS verification. Without a CA the native roots
/// are trusted. This adapter never loads a path or disables hostname
/// verification.
#[derive(Clone, Debug, Default)]
pub struct ClientTlsMaterial {
    pub ca_certificate_pem: Option<String>,
    pub identity: Option<ClientIdentity>,
}

/// Client certificate and key for mTLS.
#[derive(Clone, Debug)]
pub struct ClientIdentity {
    pub certificate_pem: String,
    pub private_key_pem: SecretString,
}

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
    /// Returns [`Error::InvalidDestination`] for a destination tonic rejects,
    /// [`Error::DestinationSecurityMismatch`] when its scheme disagrees with
    /// `security`, a certificate or key variant for unusable PEM material, and
    /// [`Error::InvalidClientTls`] when tonic cannot build the TLS connector.
    pub fn new(
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
    /// Returns the same destination and TLS errors as [`Self::new`].
    pub fn with_timeout_policy(
        destination: &str,
        security: ClientSecurity,
        timeout: ClientTimeout,
    ) -> Result<Self, Error> {
        let mut endpoint = Endpoint::from_shared(destination.to_owned())
            .map_err(|_| Error::InvalidDestination)?
            .connect_timeout(Duration::from_secs(5))
            .tcp_keepalive(Some(Duration::from_secs(60)))
            // gRPC's keepalive guide asks clients not to ping much more
            // often than once a minute, and only while a call is open.
            .http2_keep_alive_interval(Duration::from_secs(60))
            .keep_alive_timeout(Duration::from_secs(20))
            // Receive windows follow the bandwidth-delay product, as the server's do.
            .http2_adaptive_window(true);
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
        Ok(Self {
            channel: endpoint.connect_lazy(),
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

    fn call(&mut self, mut request: Request<Body>) -> Self::Future {
        let origin = Instant::now();
        let caller = crate::grpc_timeout(request.headers());
        let (opening, deadline) = self.timeout.deadlines(origin, caller);
        let caller = caller.map(|budget| Deadline::new(origin, budget));
        let call = crate::observe::Call::start(&self.series, &request, SpanKind::Client);
        otel_http::inject_context(
            &tracing_opentelemetry_instrumentation_sdk::find_context_from_tracing(call.span()),
            request.headers_mut(),
        );
        let (parts, body) = request.into_parts();
        let (body, upload) = crate::call::upload(body);
        let mut request = Request::from_parts(parts, body);
        let mut channel = self.channel.clone();
        Box::pin(async move {
            let result = async {
                tokio::select! {
                    biased;
                    () = opening.wait() => Err(deadline_status()),
                    result = async {
                        let ready = channel.ready().await.map_err(transport_status)?;
                        // A ready channel may have consumed the last of the budget.
                        if opening.expired() { return Err(deadline_status()); }
                        if let Some(caller) = caller {
                            forward_timeout(request.headers_mut(), caller)?;
                        }
                        let response = ready.call(request).await.map_err(transport_status)?;
                        if opening.expired() { return Err(deadline_status()); }
                        Ok(response)
                    } => result,
                }
            }
            .instrument(call.span().clone())
            .await;
            match result {
                Ok(response) => Ok(crate::call::attach(
                    response,
                    call,
                    None,
                    Lifetime {
                        deadline,
                        permit: None,
                        upload: Some(upload),
                    },
                    Side::Client,
                    tonic::Status::code,
                )
                .map(Body::new)),
                Err(status) => {
                    call.finish(status.code());
                    Err(status)
                }
            }
        })
    }
}

fn deadline_status() -> tonic::Status {
    tonic::Status::deadline_exceeded("request deadline exceeded")
}

fn forward_timeout(headers: &mut http::HeaderMap, caller: Deadline) -> Result<(), tonic::Status> {
    let remaining = caller.remaining();
    if remaining.is_zero() {
        return Err(deadline_status());
    }
    let mut request = tonic::Request::new(());
    request.set_timeout(remaining);
    let encoded = request.into_parts().0.into_headers();
    if let Some(value) = encoded.get("grpc-timeout") {
        headers.insert("grpc-timeout", value.clone());
    }
    Ok(())
}

/// Checks each PEM input first so a failure names it; tonic's own build error
/// does not say which input it rejected.
fn client_tls(material: &ClientTlsMaterial) -> Result<ClientTlsConfig, Error> {
    let mut config = match &material.ca_certificate_pem {
        Some(ca) => {
            crate::tls::certificates(ca, Error::InvalidCaCertificate)?;
            ClientTlsConfig::new().ca_certificate(Certificate::from_pem(ca))
        }
        None => ClientTlsConfig::new().with_native_roots(),
    };
    if let Some(identity) = &material.identity {
        crate::tls::certificates(&identity.certificate_pem, Error::InvalidCertificate)?;
        crate::tls::private_key(&identity.private_key_pem)?;
        config = config.identity(Identity::from_pem(
            &identity.certificate_pem,
            identity.private_key_pem.expose_secret(),
        ));
    }
    Ok(config)
}

/// The caller sees a fixed status, because a handler may forward it to its
/// own caller; the cause is logged inside the client span. The call's
/// supplied `grpc-timeout` running out inside tonic's channel, is the caller's deadline and not a transport fault: it
/// is `DEADLINE_EXCEEDED`, so a caller that retries `UNAVAILABLE` does not
/// repeat a call the server may still be running.
#[allow(
    clippy::needless_pass_by_value,
    reason = "used as a `map_err` callback, which hands over the owned error"
)]
fn transport_status(error: tonic::transport::Error) -> tonic::Status {
    let error = &error as &(dyn std::error::Error + 'static);
    if std::iter::successors(Some(error), |error| error.source())
        .any(<dyn std::error::Error>::is::<tonic::TimeoutExpired>)
    {
        return tonic::Status::deadline_exceeded("request deadline exceeded");
    }
    tracing::warn!(error, "grpc_client_transport_failed");
    tonic::Status::unavailable("transport unavailable")
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "budget fixtures fail with their setup context"
)]
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
    }
}
