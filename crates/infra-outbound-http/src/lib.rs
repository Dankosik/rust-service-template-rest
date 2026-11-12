//! A fixed-origin HTTPS client with finite exchange limits.
//!
//! The provider owns request content and interpretation. This crate owns
//! origin admission, bounded transport work, and static error semantics.

mod observe;
mod policy;
mod transport;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "../../../test/fixtures/tls.rs"]
mod tls;

use std::{fmt, time::Duration};

use http_body_util::{BodyExt as _, Full, Limited};
use operation_context::{Deadline, OperationContext, Stopped};
use tokio::time::Instant;
use tracing::Instrument as _;

pub use bytes::Bytes;
pub use http::{HeaderMap, Method, Request, Response, StatusCode, Version, header};
pub use observe::{HISTOGRAMS, REQUEST_DURATION_BUCKETS, REQUEST_DURATION_METRIC, UrlTemplate};
pub use url::Url;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The largest [`Limits::response_header_count`]. Hyper reserves a response
/// `HeaderMap` for the parsed header count, and `http` 1.5 panics when that
/// reservation, plus a third and rounded up to a power of two, exceeds its
/// 32,768-slot index table.
pub const MAX_RESPONSE_HEADER_COUNT: usize = 24_576;

/// Fixed client ceilings. Every field is required and positive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// The longest an exchange may take, from entry to the end of the response body.
    pub operation_timeout: Duration,
    /// At most [`MAX_RESPONSE_HEADER_COUNT`].
    pub response_header_count: usize,
    pub response_body_bytes: usize,
}

impl Limits {
    pub(crate) fn validate(&self) -> Result<(), BuildError> {
        if self.operation_timeout.is_zero()
            || self.response_header_count == 0
            || self.response_header_count > MAX_RESPONSE_HEADER_COUNT
            || self.response_body_bytes == 0
        {
            return Err(BuildError::InvalidLimits);
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// The origin URL is not admitted: see [`Client::try_new`].
    #[error("outbound HTTP origin is invalid")]
    InvalidOrigin,
    /// A limit is zero or above its ceiling: see [`Limits`].
    #[error("outbound HTTP limits are invalid")]
    InvalidLimits,
    #[error("outbound HTTP TLS configuration failed")]
    Tls {
        #[source]
        source: BoxError,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The URL is not an absolute URL on the client's origin.
    #[error("outbound HTTP target is invalid")]
    InvalidTarget,
    /// A deadline or [`Limits::operation_timeout`] ended the operation.
    #[error("outbound HTTP operation timed out")]
    Timeout,
    /// A supplied [`OperationContext`] was cancelled before the exchange ended.
    #[error("outbound HTTP operation was cancelled")]
    Cancelled,
    #[error("outbound HTTP dispatch was refused")]
    DispatchRefused,
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
/// # Request preparation
///
/// The client rejects requests with a caller-supplied `Host` header and
/// sets `Host` from the configured origin. It sends HTTP/1.1, removes
/// `Transfer-Encoding`, and sets `Content-Length` to the buffered body length
/// for nonempty bodies and for `POST`, `PUT`, and `PATCH`. For other empty
/// requests it removes `Content-Length`. If `Accept` is absent, it adds `*/*`.
///
/// ```no_run
/// use std::time::Duration;
///
/// use infra_outbound_http::{Bytes, Client, Limits, Request, Url};
/// use tokio::time::Instant;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = Client::try_new(
///     &Url::parse("https://provider.example")?,
///     Limits {
///         operation_timeout: Duration::from_secs(2),
///         response_header_count: 100,
///         response_body_bytes: 1024 * 1024,
///     },
/// )?;
/// let request = Request::get("https://provider.example/v1/items?q=a%2Fb")
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
    origin: policy::Origin,
    server: observe::Server,
    limits: Limits,
    propagate_trace_context: bool,
    transport: transport::Transport,
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Client")
            .field("origin", &self.origin)
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
    /// Returns [`BuildError::InvalidLimits`] for invalid limits,
    /// [`BuildError::InvalidOrigin`] for a non-HTTPS URL, a URL without a
    /// host, or a URL with userinfo, and
    /// [`BuildError::Tls`] when the platform verifier cannot be built.
    pub fn try_new(origin: &Url, limits: Limits) -> Result<Self, BuildError> {
        limits.validate()?;
        let origin = policy::admit_origin(origin)?;
        Ok(Self::from_origin(
            origin,
            limits,
            transport::tls_config()?.clone(),
            transport::Resolver::new(),
        ))
    }

    fn from_origin(
        origin: policy::Origin,
        limits: Limits,
        tls: rustls::ClientConfig,
        resolver: transport::Resolver,
    ) -> Self {
        Self {
            server: observe::Server::new(&origin),
            transport: transport::build_transport(&limits, &origin, tls, resolver),
            origin,
            limits,
            propagate_trace_context: false,
        }
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
    /// tests. Production code must use [`Client::try_new`].
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidLimits`] for invalid limits,
    /// [`BuildError::InvalidOrigin`] unless `origin` is an `http` URL with a
    /// literal loopback IP address and no userinfo, and [`BuildError::Tls`]
    /// when the platform verifier cannot be built.
    #[cfg(feature = "test-support")]
    pub fn try_new_for_test_http(origin: &Url, limits: Limits) -> Result<Self, BuildError> {
        limits.validate()?;
        let origin = policy::admit_test_http_origin(origin)?;
        Ok(Self::from_origin(
            origin,
            limits,
            transport::tls_config()?.clone(),
            transport::Resolver::new(),
        ))
    }

    /// Executes one complete buffered exchange before `deadline`.
    ///
    /// The request follows the preparation rules on [`Client`].
    ///
    /// An [`OperationContext`] request extension additionally bounds cancellation
    /// and cutoff. The end is fixed on execution entry at the earlier of `deadline` and
    /// entry plus [`Limits::operation_timeout`]. Admission, setup, DNS and
    /// complete buffered collection spend that same budget. At or after the
    /// end, no new transport dispatch or successful operation decision is allowed.
    ///
    /// Attempt telemetry starts after the initial stop check and request
    /// admission. Once started, a completed attempt records its result once
    /// after the exchange returns. Success requires a fully buffered body and
    /// a final stop check. Synchronous terminal observation callbacks can delay
    /// return beyond the deadline without changing the result.
    /// Dropping this future ends the exchange; it does not undo a provider-side
    /// effect. A [`UrlTemplate`] request extension names the
    /// operation in the attempt's span and metric.
    ///
    /// # Errors
    ///
    /// Returns target, timeout, cancellation, response-size, or transport errors.
    pub async fn execute(
        &self,
        request: Request<Bytes>,
        deadline: Instant,
    ) -> Result<Response<Bytes>, Error> {
        // Built directly rather than through `begin_operation`: a parent made
        // here could never be cancelled, so its child token is pure cost.
        let operation = OperationContext::from_deadline(
            Deadline::at(deadline)
                .earlier(Deadline::new(Instant::now(), self.limits.operation_timeout)),
        );
        self.execute_within(request, &operation, || true, |_| {})
            .await
    }

    /// Fixes the resource operation allowance before a composition begins
    /// credential acquisition or other preparation. Later execution may only
    /// shorten this context, never restart its deadline.
    #[must_use]
    pub fn begin_operation(&self, parent: &OperationContext) -> OperationContext {
        parent.child(self.limits.operation_timeout)
    }

    /// Executes one buffered exchange within the caller context and local ceiling.
    ///
    /// The request follows the preparation rules on [`Client`].
    ///
    /// An [`OperationContext`] request extension supplies an additional bound;
    /// neither supplied cancellation scope nor cutoff is discarded. Preparation,
    /// dispatch and confirmed body EOF all spend the same original allowance.
    /// A cutoff maps to [`Error::Timeout`] and cancellation to
    /// [`Error::Cancelled`]; neither undoes a remote effect.
    ///
    /// # Errors
    ///
    /// Returns target, timeout, cancellation, response-size, or transport errors.
    pub async fn execute_with_context(
        &self,
        request: Request<Bytes>,
        context: &OperationContext,
    ) -> Result<Response<Bytes>, Error> {
        self.execute_with_response_status(request, context, |_| {})
            .await
    }

    /// Like [`Self::execute_with_context`], with a synchronous callback when
    /// response headers arrive. It runs once before body admission or reading,
    /// so it may observe a status even if the exchange later fails.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::execute_with_context`].
    pub async fn execute_with_response_status(
        &self,
        request: Request<Bytes>,
        context: &OperationContext,
        on_status: impl FnOnce(StatusCode) + Send,
    ) -> Result<Response<Bytes>, Error> {
        self.execute_with_dispatch_check(request, context, || true, on_status)
            .await
    }

    /// Like [`Self::execute_with_response_status`], with a synchronous admission
    /// check after request preparation and immediately before transport handoff.
    ///
    /// # Errors
    /// Returns [`Error::DispatchRefused`] when the check returns `false`, or
    /// the same errors as [`Self::execute_with_response_status`].
    pub async fn execute_with_dispatch_check(
        &self,
        request: Request<Bytes>,
        context: &OperationContext,
        check: impl FnOnce() -> bool + Send,
        on_status: impl FnOnce(StatusCode) + Send,
    ) -> Result<Response<Bytes>, Error> {
        self.execute_within(request, &self.begin_operation(context), check, on_status)
            .await
    }

    async fn execute_within(
        &self,
        mut request: Request<Bytes>,
        operation: &OperationContext,
        check: impl FnOnce() -> bool + Send,
        on_status: impl FnOnce(StatusCode) + Send,
    ) -> Result<Response<Bytes>, Error> {
        let request_context = request.extensions_mut().remove::<OperationContext>();
        let stops = StopScopes {
            operation,
            request: request_context.as_ref(),
        };
        stops.check()?;
        let mut request = policy::admit_request(&self.origin, request)?;

        let template = request.extensions_mut().remove::<UrlTemplate>();
        let mut attempt = observe::Attempt::start(request.method(), template, &self.server);
        if self.propagate_trace_context {
            attempt.inject_trace_context(request.headers_mut());
        }
        // The success decision is made here, after the last await: leaving the
        // exchange span and dropping the body run after the exchange's own last
        // check and spend the same budget. Terminal observation can delay
        // return but cannot reopen this decision.
        let result = self
            .exchange(request, &stops, &mut attempt, check, on_status)
            .await
            .and_then(|response| stops.check().map(|()| response));
        attempt.finish(&result);
        result
    }

    async fn exchange(
        &self,
        request: Request<Bytes>,
        stops: &StopScopes<'_>,
        attempt: &mut observe::Attempt,
        check: impl FnOnce() -> bool + Send,
        on_status: impl FnOnce(StatusCode) + Send,
    ) -> Result<Response<Bytes>, Error> {
        let span = attempt.span();
        let exchange = async {
            stops.check()?;
            let admitted = check();
            stops.check()?;
            if !admitted {
                return Err(Error::DispatchRefused);
            }
            let response = self
                .transport
                .request(request.map(Full::new))
                .await
                .map_err(|source| Error::Transport {
                    source: Box::new(source),
                })?;
            attempt.record_status(response.status());
            on_status(response.status());
            stops.check()?;
            let body_limit = self.limits.response_body_bytes;
            if hyper::body::Body::size_hint(response.body())
                .exact()
                .is_some_and(|length| length > body_limit as u64)
            {
                return Err(Error::ResponseBodyTooLarge);
            }

            let (parts, body) = response.into_parts();
            let body = collect_body(body, body_limit, stops).await?;
            Ok(Response::from_parts(parts, body))
        };
        tokio::select! {
            biased;
            error = stops.stopped() => Err(error),
            result = exchange.instrument(span) => result,
        }
    }
}

/// The scopes that can stop an exchange: the client's operation and the
/// caller's request extension. The operation's stop wins over the request's.
struct StopScopes<'a> {
    operation: &'a OperationContext,
    request: Option<&'a OperationContext>,
}

impl StopScopes<'_> {
    fn check(&self) -> Result<(), Error> {
        self.operation
            .check()
            .and_then(|()| self.request.map_or(Ok(()), OperationContext::check))
            .map_err(stopped_error)
    }

    async fn stopped(&self) -> Error {
        let request = async {
            match self.request {
                Some(request) => request.wait_stopped().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            stopped = self.operation.wait_stopped() => stopped_error(stopped),
            stopped = request => stopped_error(stopped),
        }
    }
}

async fn collect_body(
    body: hyper::body::Incoming,
    limit: usize,
    stops: &StopScopes<'_>,
) -> Result<Bytes, Error> {
    let mut body = Limited::new(body, limit);
    let mut collected = Vec::new();
    loop {
        stops.check()?;
        let Some(frame) = body.frame().await else {
            break;
        };
        if let Ok(data) = frame.map_err(map_body_error)?.into_data() {
            append_within(&mut collected, &data, limit);
        }
        // A frame's `Bytes` can share a large read buffer, so each frame
        // is dropped here, before the next poll.
    }
    Ok(Bytes::from(collected))
}

fn stopped_error(stopped: Stopped) -> Error {
    match stopped {
        Stopped::Deadline => Error::Timeout,
        Stopped::Cancelled => Error::Cancelled,
    }
}

/// Appends `data`, growing `collected` twofold like `Vec` but never past
/// `limit`; [`Limited`] admits at most `limit` bytes in total. The buffer is
/// not presized from `Content-Length`, so a declared length cannot hold memory
/// for bytes the provider never sends.
fn append_within(collected: &mut Vec<u8>, data: &[u8], limit: usize) {
    let required = collected.len().saturating_add(data.len());
    if required > collected.capacity() {
        let target = required.max(collected.capacity().saturating_mul(2).min(limit));
        collected.reserve_exact(target - collected.len());
    }
    collected.extend_from_slice(data);
}

// `Limited` yields either the hyper body error or its own length error.
fn map_body_error(error: BoxError) -> Error {
    if error.is::<http_body_util::LengthLimitError>() {
        Error::ResponseBodyTooLarge
    } else {
        Error::Transport { source: error }
    }
}
