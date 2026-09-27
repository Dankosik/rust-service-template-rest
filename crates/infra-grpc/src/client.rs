use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use http::{Request, Response};
use tonic::body::Body;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tower::ServiceExt as _;
use tracing::Instrument as _;

use crate::Error;

/// Explicit security selected for one trusted operator destination.
#[derive(Clone, Debug)]
pub enum ClientSecurity {
    Plaintext,
    Tls(ClientTlsMaterial),
}

/// Trust and optional client identity material for normal TLS verification.
/// The service config owner admits these values; this adapter never loads a
/// path or disables hostname verification.
#[derive(Clone)]
pub struct ClientTlsMaterial {
    pub ca_certificate_pem: Option<Vec<u8>>,
    pub certificate_pem: Option<Vec<u8>>,
    pub private_key_pem: Option<Vec<u8>>,
}

impl fmt::Debug for ClientTlsMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientTlsMaterial")
            .field("has_ca_certificate", &self.ca_certificate_pem.is_some())
            .field("has_certificate", &self.certificate_pem.is_some())
            .field("has_private_key", &self.private_key_pem.is_some())
            .finish()
    }
}

/// One lazy, shared channel for a configured dependency.
///
/// Construction performs neither DNS nor network I/O. Each call injects the
/// current trace context and records the client span and metrics from the
/// response headers.
#[derive(Clone, Debug)]
pub struct Client {
    channel: Channel,
}

impl Client {
    /// Parses an explicit trusted destination and constructs a lazy channel.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] for an invalid destination or
    /// unusable TLS trust or client identity material.
    pub fn new(destination: &str, security: ClientSecurity) -> Result<Self, Error> {
        let mut endpoint = Endpoint::from_shared(destination.to_owned())
            .map_err(|_| Error::InvalidConfiguration)?
            .connect_timeout(Duration::from_secs(5))
            .tcp_keepalive(Some(Duration::from_secs(60)))
            .http2_keep_alive_interval(Duration::from_secs(20))
            .keep_alive_timeout(Duration::from_secs(20));
        if let ClientSecurity::Tls(material) = security {
            endpoint = endpoint
                .tls_config(client_tls(&material)?)
                .map_err(|_| Error::InvalidConfiguration)?;
        }
        Ok(Self {
            channel: endpoint.connect_lazy(),
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

    fn call(&mut self, mut request: Request<Body>) -> Self::Future {
        let mut channel = self.channel.clone();
        Box::pin(async move {
            let span = tracing_opentelemetry_instrumentation_sdk::http::grpc_client::make_span_from_request(
                &request,
            );
            tracing_opentelemetry_instrumentation_sdk::http::inject_context(
                &tracing_opentelemetry_instrumentation_sdk::find_context_from_tracing(&span),
                request.headers_mut(),
            );
            let started = Instant::now();
            let label = request.uri().path().to_owned();
            let result = async {
                let ready = channel.ready().await.map_err(transport_status)?;
                ready.call(request).await.map_err(transport_status)
            }
            .instrument(span.clone())
            .await;
            match &result {
                Ok(response) => {
                    let code = crate::observe::code_from_headers(response.headers());
                    tracing_opentelemetry_instrumentation_sdk::http::grpc::update_span_from_response(
                        &span, response, false,
                    );
                    crate::observe::record(&label, "client", code, started.elapsed());
                }
                Err(status) => {
                    crate::observe::record(&label, "client", status.code(), started.elapsed());
                }
            }
            result
        })
    }
}

fn client_tls(material: &ClientTlsMaterial) -> Result<ClientTlsConfig, Error> {
    let mut config = ClientTlsConfig::new();
    config = match &material.ca_certificate_pem {
        Some(ca) => config.ca_certificate(Certificate::from_pem(ca)),
        None => config.with_native_roots(),
    };
    match (&material.certificate_pem, &material.private_key_pem) {
        (Some(certificate), Some(key)) => {
            config = config.identity(Identity::from_pem(certificate, key));
        }
        (None, None) => {}
        _ => return Err(Error::InvalidConfiguration),
    }
    Ok(config)
}

fn transport_status(_error: tonic::transport::Error) -> tonic::Status {
    tonic::Status::unavailable("transport unavailable")
}
