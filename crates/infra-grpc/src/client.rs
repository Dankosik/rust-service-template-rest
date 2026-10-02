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

/// One lazy, shared channel for a configured dependency.
///
/// Construction performs neither DNS nor network I/O. Each call injects the
/// current trace context and records the client span and metrics from the
/// response headers.
#[derive(Clone, Debug)]
pub struct Client {
    channel: Channel,
    series: Arc<crate::observe::Series>,
}

impl Client {
    /// Parses an explicit trusted destination and constructs a lazy channel.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDestination`] for a destination tonic rejects,
    /// [`Error::DestinationSecurityMismatch`] when its scheme disagrees with
    /// `security`, a certificate or key variant for unusable PEM material, and
    /// [`Error::InvalidClientTls`] when tonic cannot build the TLS connector.
    pub fn new(destination: &str, security: ClientSecurity) -> Result<Self, Error> {
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
            series: Arc::new(crate::observe::Series::client()),
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
        let call = crate::observe::Call::start(&self.series, &request, SpanKind::Client);
        otel_http::inject_context(
            &tracing_opentelemetry_instrumentation_sdk::find_context_from_tracing(call.span()),
            request.headers_mut(),
        );
        let mut channel = self.channel.clone();
        Box::pin(async move {
            let result = async {
                let ready = channel.ready().await.map_err(transport_status)?;
                ready.call(request).await.map_err(transport_status)
            }
            .instrument(call.span().clone())
            .await;
            call.finish(match &result {
                Ok(response) => crate::observe::code_from_headers(response.headers()),
                Err(status) => status.code(),
            });
            result
        })
    }
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
/// own caller; the cause is logged inside the client span. The call's own
/// `grpc-timeout` running out, which tonic's channel enforces, is the
/// caller's deadline and not a transport fault: it is `DEADLINE_EXCEEDED`,
/// so a caller that retries `UNAVAILABLE` does not repeat a call the server
/// may still be running.
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
