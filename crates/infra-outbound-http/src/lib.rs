//! A fixed-origin HTTPS client with finite exchange limits.
//!
//! The provider owns request content and interpretation. This crate owns
//! origin admission, bounded transport work, and static error semantics.

mod observe;
mod policy;

#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "../../../test/fixtures/tls.rs"]
mod tls;

use std::{fmt, sync::OnceLock, time::Duration};

use http_body_util::{BodyExt as _, Full, Limited};
use hyper_util::client::legacy::connect::HttpConnector;
use operation_context::{Deadline, OperationContext};
use tokio::time::Instant;
use tracing::Instrument as _;

pub use bytes::Bytes;
pub use http::{HeaderMap, Method, Request, Response, StatusCode, Version, header};
pub use observe::{REQUEST_DURATION_BUCKETS, REQUEST_DURATION_METRIC, UrlTemplate};
pub use url::Url;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The longest TCP connect budget, whatever the operation timeout.
const CONNECT_TIMEOUT_CAP: Duration = Duration::from_secs(10);
type Transport = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<HttpConnector<Resolver>>,
    Full<Bytes>,
>;

// System DNS; tests route fixture names to loopback listeners instead.
#[cfg(not(test))]
type Resolver = hyper_util::client::legacy::connect::dns::GaiResolver;
#[cfg(test)]
type Resolver = tests::FixtureResolver;

/// Fixed client ceilings. Every field is required and positive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub operation_timeout: Duration,
    pub response_header_count: usize,
    pub response_body_bytes: usize,
}

/// Client construction failures.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("outbound HTTP configuration is invalid")]
    InvalidConfiguration,
    #[error("outbound HTTP TLS configuration failed")]
    Tls {
        #[source]
        source: BoxError,
    },
}

/// Outbound exchange failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("outbound HTTP target is invalid")]
    InvalidTarget,
    #[error("outbound HTTP operation timed out")]
    Timeout,
    #[error("outbound HTTP response body is too large")]
    ResponseBodyTooLarge,
    #[error("outbound HTTP transport failed")]
    Transport {
        #[source]
        source: BoxError,
    },
}

/// A reusable bounded client for one configured origin. Requests name
/// absolute URLs on that origin; any other origin is refused before I/O.
///
/// ```no_run
/// use std::time::Duration;
///
/// use infra_outbound_http::{Bytes, Client, Limits, Url};
/// use tokio::time::Instant;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = Client::new(
///     &Url::parse("https://provider.example")?,
///     Limits {
///         operation_timeout: Duration::from_secs(2),
///         response_header_count: 100,
///         response_body_bytes: 1024 * 1024,
///     },
/// )?;
/// let request = http::Request::get("https://provider.example/v1/items?q=a%2Fb")
///     .body(Bytes::new())?;
/// let response = client
///     .execute(request, Instant::now() + Duration::from_secs(3))
///     .await?;
/// assert!(response.status().is_success());
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Client {
    target: policy::Target,
    server: observe::Server,
    limits: Limits,
    propagate_trace_context: bool,
    transport: Transport,
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Client")
            .field("origin", &self.target)
            .field("limits", &self.limits)
            .field("propagate_trace_context", &self.propagate_trace_context)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Creates a client bound to the origin (scheme, host, and port) of
    /// `origin`. Its path, query, and fragment are ignored. Construction
    /// performs no DNS or network I/O.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidConfiguration`] for invalid limits or a
    /// non-HTTPS URL, a URL without a host, or a URL with userinfo, and
    /// [`BuildError::Tls`] when the platform verifier cannot be built.
    pub fn new(origin: &Url, limits: Limits) -> Result<Self, BuildError> {
        policy::validate_limits(&limits)?;
        let target = policy::admit_origin(origin)?;
        Ok(Self {
            server: observe::Server::new(&target),
            target,
            limits,
            propagate_trace_context: false,
            transport: build_transport(&limits, true, tls_config()?.clone(), Resolver::new()),
        })
    }

    /// Sends the trace context of each attempt to the origin through the
    /// process text-map propagator (W3C `traceparent` and `tracestate`),
    /// replacing any caller value of those headers. Off by default: a trace
    /// identifier is data shared with the provider, so the adapter opts in
    /// for an origin that belongs to the same trace domain.
    #[must_use]
    pub fn with_trace_context(mut self) -> Self {
        self.propagate_trace_context = true;
        self
    }

    /// Creates the narrow local-HTTP constructor used by downstream mock
    /// tests. Production code must use [`Client::new`].
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidConfiguration`] unless `origin` is an
    /// `http` URL whose host is a literal loopback IP address.
    #[cfg(feature = "test-support")]
    pub fn new_for_test_http(origin: &Url, limits: Limits) -> Result<Self, BuildError> {
        policy::validate_limits(&limits)?;
        let target = policy::admit_test_http_origin(origin)?;
        Ok(Self {
            server: observe::Server::new(&target),
            target,
            limits,
            propagate_trace_context: false,
            transport: build_transport(&limits, false, tls_config()?.clone(), Resolver::new()),
        })
    }

    /// Executes one complete buffered exchange before `deadline`.
    ///
    /// An [`OperationContext`] request extension additionally bounds cancellation
    /// and cutoff. The end is fixed on execution entry at the earlier of `deadline` and
    /// entry plus [`Limits::operation_timeout`]. Admission, setup, DNS and
    /// complete buffered collection spend that same budget. At or after the
    /// end, no new transport dispatch or successful operation decision is allowed.
    /// The result is fixed after the last await and full buffering, then observed
    /// once and returned unchanged. Synchronous terminal observation callbacks
    /// can delay physical return beyond the end.
    /// Dropping this future ends the exchange; it does not undo a provider-side
    /// effect. A [`UrlTemplate`] request extension names the
    /// operation in the attempt's span and metric.
    ///
    /// # Errors
    ///
    /// Returns target, timeout, response-size, or transport errors.
    pub async fn execute(
        &self,
        request: Request<Bytes>,
        deadline: Instant,
    ) -> Result<Response<Bytes>, Error> {
        self.execute_with_context(
            request,
            &OperationContext::from_deadline(Deadline::at(deadline)),
        )
        .await
    }

    /// Fixes the resource operation allowance before a composition begins
    /// credential acquisition or other preparation. Later execution may only
    /// shorten this context, never restart its deadline.
    #[must_use]
    pub fn operation_context(&self, parent: &OperationContext) -> OperationContext {
        parent.child(self.limits.operation_timeout)
    }

    /// Executes one buffered exchange within the caller context and local ceiling.
    ///
    /// An [`OperationContext`] request extension supplies an additional bound;
    /// neither supplied cancellation scope nor cutoff is discarded. Preparation,
    /// dispatch and confirmed body EOF all spend the same original allowance.
    /// Cancellation maps to [`Error::Timeout`] and does not undo a remote effect.
    ///
    /// # Errors
    ///
    /// Returns target, timeout, response-size, or transport errors.
    pub async fn execute_with_context(
        &self,
        mut request: Request<Bytes>,
        context: &OperationContext,
    ) -> Result<Response<Bytes>, Error> {
        let context = self.operation_context(context);
        let request_context = request
            .extensions_mut()
            .remove::<OperationContext>()
            .unwrap_or_else(OperationContext::unbounded);
        check_contexts(&context, &request_context)?;
        let mut request = policy::admit_request(&self.target, request)?;

        let template = request.extensions_mut().remove::<UrlTemplate>();
        let mut attempt = observe::Attempt::start(request.method(), template, &self.server);
        if self.propagate_trace_context {
            attempt.inject_trace_context(request.headers_mut());
        }
        let result = self
            .exchange(request, &context, &request_context, &mut attempt)
            .await;
        // Fix the successful result after buffering and the last await.
        // Terminal observation can delay return but cannot reopen this decision.
        let result = if result.is_ok() {
            check_contexts(&context, &request_context).and(result)
        } else {
            result
        };
        attempt.finish(&result);
        result
    }

    async fn exchange(
        &self,
        request: Request<Bytes>,
        context: &OperationContext,
        request_context: &OperationContext,
        attempt: &mut observe::Attempt,
    ) -> Result<Response<Bytes>, Error> {
        let span = attempt.span();
        let exchange = async {
            check_contexts(context, request_context)?;
            let response = self
                .transport
                .request(request.map(Full::new))
                .await
                .map_err(|source| Error::Transport {
                    source: Box::new(source),
                })?;
            attempt.response_headers(response.status());
            check_contexts(context, request_context)?;
            let body_limit = self.limits.response_body_bytes;
            if hyper::body::Body::size_hint(response.body())
                .exact()
                .is_some_and(|length| length > body_limit as u64)
            {
                return Err(Error::ResponseBodyTooLarge);
            }

            let (parts, body) = response.into_parts();
            let mut body = Limited::new(body, body_limit);
            let mut collected = Vec::new();
            loop {
                check_contexts(context, request_context)?;
                let Some(frame) = body.frame().await else {
                    break;
                };
                if let Ok(data) = frame.map_err(map_body_error)?.into_data() {
                    // Limited admits at most body_limit bytes in total.
                    let required = collected.len().saturating_add(data.len());
                    if required > collected.capacity() {
                        let target =
                            required.max(collected.capacity().saturating_mul(2).min(body_limit));
                        collected.reserve_exact(target - collected.len());
                    }
                    collected.extend_from_slice(&data);
                }
                // Release this frame's backing allocation before polling again.
            }
            check_contexts(context, request_context)?;
            Ok(Response::from_parts(parts, Bytes::from(collected)))
        };
        tokio::select! {
            biased;
            _ = context.wait_stopped() => Err(Error::Timeout),
            _ = request_context.wait_stopped() => Err(Error::Timeout),
            result = exchange.instrument(span) => result,
        }
    }
}

fn check_contexts(
    context: &OperationContext,
    request_context: &OperationContext,
) -> Result<(), Error> {
    context
        .check()
        .and_then(|()| request_context.check())
        .map_err(|_| Error::Timeout)
}

/// The process-wide TLS client configuration. Its platform verifier loads the
/// system root store once, and every client shares its session cache, which
/// rustls keys by server name.
fn tls_config() -> Result<&'static rustls::ClientConfig, BuildError> {
    use rustls_platform_verifier::BuilderVerifierExt as _;

    static CONFIG: OnceLock<rustls::ClientConfig> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(config);
    }
    let build_error = |source: rustls::Error| BuildError::Tls {
        source: Box::new(source),
    };
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(build_error)?
    .with_platform_verifier()
    .map_err(build_error)?
    .with_no_client_auth();
    Ok(CONFIG.get_or_init(|| config))
}

fn build_transport(
    limits: &Limits,
    https_only: bool,
    tls: rustls::ClientConfig,
    resolver: Resolver,
) -> Transport {
    let mut http = HttpConnector::new_with_resolver(resolver);
    http.enforce_http(false);
    http.set_nodelay(true);
    // Hyper-util divides this among the resolved addresses of one family, so
    // an address that never answers leaves time for the next one.
    http.set_connect_timeout(Some(
        (limits.operation_timeout / 2).min(CONNECT_TIMEOUT_CAP),
    ));
    http.set_keepalive(Some(Duration::from_secs(15)));
    http.set_keepalive_interval(Some(Duration::from_secs(15)));
    http.set_keepalive_retries(Some(3));
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    http.set_tcp_user_timeout(Some(Duration::from_secs(30)));
    let https = hyper_rustls::HttpsConnectorBuilder::new().with_tls_config(tls);
    let https = if https_only {
        https.https_only()
    } else {
        https.https_or_http()
    };
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .timer(hyper_util::rt::TokioTimer::new())
        .pool_timer(hyper_util::rt::TokioTimer::new())
        .pool_idle_timeout(Duration::from_secs(30))
        .http1_max_headers(limits.response_header_count)
        .build(https.enable_http1().wrap_connector(http))
}

// `Limited` yields either the hyper body error or its own length error.
fn map_body_error(error: BoxError) -> Error {
    if error.is::<http_body_util::LengthLimitError>() {
        Error::ResponseBodyTooLarge
    } else {
        Error::Transport { source: error }
    }
}
