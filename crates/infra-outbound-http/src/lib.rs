mod context;
mod observe;
mod policy;
mod transport;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

use std::time::Duration;

use http_body_util::{BodyExt as _, Full, Limited};
use operation_context::{Deadline, OperationContext, Stopped};
use tokio::time::Instant;
use tracing::Instrument as _;

pub use bytes::Bytes;
pub use context::RequestContext;
pub use http::{HeaderMap, Method, Request, Response, StatusCode, Version, header};
pub use observe::{HISTOGRAMS, REQUEST_DURATION_BUCKETS, REQUEST_DURATION_METRIC, UrlTemplate};
pub use transport::prepare_tls;
pub use url::Url;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub const MAX_RESPONSE_HEADER_COUNT: usize = 24_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub operation_timeout: Duration,
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
    #[error("outbound HTTP origin is invalid")]
    InvalidOrigin,
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
    #[error("outbound HTTP target is invalid")]
    InvalidTarget,
    #[error("outbound HTTP operation timed out")]
    Timeout,
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

#[derive(Debug, thiserror::Error)]
pub enum ExchangeError {
    #[error("outbound HTTP exchange failed before response headers")]
    BeforeResponse(#[source] Error),
    #[error("outbound HTTP exchange failed after HTTP {status}")]
    AfterResponse {
        status: StatusCode,
        #[source]
        source: Error,
    },
}

impl ExchangeError {
    #[must_use]
    pub fn into_error(self) -> Error {
        match self {
            Self::BeforeResponse(source) | Self::AfterResponse { source, .. } => source,
        }
    }
}

#[derive(Clone, derive_more::Debug)]
pub struct Client {
    origin: policy::Origin,
    #[debug(skip)]
    server: observe::Server,
    limits: Limits,
    propagate_trace_context: bool,
    #[debug(skip)]
    transport: transport::Transport,
}

impl Client {
    #[must_use]
    pub fn matches_origin(&self, origin: &http::Uri) -> bool {
        origin
            .path_and_query()
            .is_none_or(|pq| pq.query().is_none() && matches!(pq.path(), "" | "/"))
            && self.origin.admits(origin)
    }

    pub fn try_new(origin: &Url, limits: Limits) -> Result<Self, BuildError> {
        limits.validate()?;
        Self::with_default_transport(policy::admit_origin(origin)?, limits)
    }

    fn with_default_transport(origin: policy::Origin, limits: Limits) -> Result<Self, BuildError> {
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

    #[must_use]
    pub fn with_trace_context(mut self) -> Self {
        self.propagate_trace_context = true;
        self
    }

    #[cfg(feature = "test-support")]
    pub fn try_new_for_test_http(origin: &Url, limits: Limits) -> Result<Self, BuildError> {
        limits.validate()?;
        Self::with_default_transport(policy::admit_test_http_origin(origin)?, limits)
    }

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

    pub async fn execute_with_context(
        &self,
        request: Request<Bytes>,
        context: &OperationContext,
    ) -> Result<Response<Bytes>, Error> {
        self.execute_buffered(request, context)
            .await
            .map_err(ExchangeError::into_error)
    }

    pub async fn execute_buffered(
        &self,
        request: Request<Bytes>,
        context: impl Into<RequestContext>,
    ) -> Result<Response<Bytes>, ExchangeError> {
        self.prepare_scoped(request, context.into())
            .map_err(ExchangeError::BeforeResponse)?
            .send_with_response(|| true, |_| {}, buffer_response)
            .await
    }

    pub async fn execute_with_response_status(
        &self,
        request: Request<Bytes>,
        context: &OperationContext,
        on_status: impl FnOnce(StatusCode) + Send,
    ) -> Result<Response<Bytes>, Error> {
        self.prepare(request, context)?
            .send(|| true, on_status)
            .await
    }

    pub async fn execute_for_headers(
        &self,
        request: Request<Bytes>,
        context: &OperationContext,
    ) -> Result<http::response::Parts, Error> {
        self.prepare(request, context)?
            .send_with_response(
                || true,
                |_| {},
                async |response, _, _| Ok(response.into_parts().0),
            )
            .await
            .map_err(ExchangeError::into_error)
    }

    pub fn prepare(
        &self,
        request: Request<Bytes>,
        context: &OperationContext,
    ) -> Result<PreparedRequest<'_>, Error> {
        self.prepare_scoped(request, context.into())
    }

    fn prepare_scoped(
        &self,
        mut request: Request<Bytes>,
        context: RequestContext,
    ) -> Result<PreparedRequest<'_>, Error> {
        let stops = context.for_request(
            Deadline::after(self.limits.operation_timeout),
            request.extensions_mut().remove::<OperationContext>(),
        );
        stops.check()?;
        let request = policy::admit_request(&self.origin, request)?;
        Ok(PreparedRequest {
            client: self,
            request,
            stops,
        })
    }
}

#[derive(derive_more::Debug)]
#[debug("PreparedRequest([REDACTED])")]
pub struct PreparedRequest<'a> {
    client: &'a Client,
    request: Request<Bytes>,
    stops: RequestContext,
}

impl PreparedRequest<'_> {
    #[must_use]
    pub const fn deadline(&self) -> Option<Deadline> {
        self.stops.deadline()
    }

    #[must_use]
    pub fn stopped(&self) -> Option<Stopped> {
        self.stops.stopped()
    }

    pub async fn wait_stopped(&self) -> Stopped {
        self.stops.wait_stopped().await
    }

    #[must_use]
    pub const fn context(&self) -> &RequestContext {
        &self.stops
    }

    pub fn set_authorization(&mut self, value: http::HeaderValue) {
        self.request
            .headers_mut()
            .insert(header::AUTHORIZATION, value);
    }

    pub async fn send(
        self,
        check: impl FnOnce() -> bool + Send,
        on_status: impl FnOnce(StatusCode) + Send,
    ) -> Result<Response<Bytes>, Error> {
        self.send_with_response(check, on_status, buffer_response)
            .await
            .map_err(ExchangeError::into_error)
    }

    async fn send_with_response<T>(
        self,
        check: impl FnOnce() -> bool + Send,
        on_status: impl FnOnce(StatusCode) + Send,
        consume: impl AsyncFnOnce(
            Response<hyper::body::Incoming>,
            usize,
            &RequestContext,
        ) -> Result<T, Error>,
    ) -> Result<T, ExchangeError> {
        let Self {
            client,
            mut request,
            stops,
        } = self;
        stops
            .check()
            .map_err(Error::from)
            .map_err(ExchangeError::BeforeResponse)?;
        let template = request.extensions_mut().remove::<UrlTemplate>();
        let mut attempt = observe::Attempt::start(
            request.method(),
            template,
            &client.server,
            client.origin.port(),
            &stops,
        );
        if client.propagate_trace_context {
            attempt.inject_trace_context(request.headers_mut());
        }
        let span = attempt.span();
        let exchange = async {
            stops.check()?;
            let admitted = check();
            stops.check()?;
            if !admitted {
                return Err(Error::DispatchRefused);
            }
            let response = client
                .transport
                .request(request.map(Full::new))
                .await
                .map_err(|source| Error::Transport {
                    source: Box::new(source),
                })?;
            attempt.record_status(response.status());
            on_status(response.status());
            stops.check()?;
            consume(response, client.limits.response_body_bytes, &stops).await
        };
        let result = tokio::select! {
            biased;
            reason = stops.wait_stopped() => Err(Error::from(reason)),
            result = exchange.instrument(span) => result,
        }
        .and_then(|response| {
            stops.check()?;
            Ok(response)
        });
        let status = attempt.status();
        std::mem::DropGuard::dismiss(attempt).finish(result.as_ref().err());
        result.map_err(|source| match status {
            Some(status) => ExchangeError::AfterResponse { status, source },
            None => ExchangeError::BeforeResponse(source),
        })
    }
}

async fn buffer_response(
    response: Response<hyper::body::Incoming>,
    limit: usize,
    stops: &RequestContext,
) -> Result<Response<Bytes>, Error> {
    if hyper::body::Body::size_hint(response.body())
        .exact()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(Error::ResponseBodyTooLarge);
    }

    let (parts, body) = response.into_parts();
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
    }
    let buffered = Bytes::from(collected);
    drop(body);
    Ok(Response::from_parts(parts, buffered))
}

impl From<Stopped> for Error {
    fn from(stopped: Stopped) -> Self {
        match stopped {
            Stopped::Deadline => Self::Timeout,
            Stopped::Cancelled => Self::Cancelled,
        }
    }
}

fn append_within(collected: &mut Vec<u8>, data: &[u8], limit: usize) {
    let required = collected.len().saturating_add(data.len());
    if required > collected.capacity() {
        let target = required.max(collected.capacity().saturating_mul(2).min(limit));
        collected.reserve_exact(target - collected.len());
    }
    collected.extend_from_slice(data);
}

fn map_body_error(error: BoxError) -> Error {
    if error.is::<http_body_util::LengthLimitError>() {
        Error::ResponseBodyTooLarge
    } else {
        Error::Transport { source: error }
    }
}
