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
use tracing::Instrument as _;

pub use bytes::Bytes;
pub use context::RequestContext;
pub use http::{HeaderMap, Method, Request, Response, StatusCode, Version, header};
pub use observe::{HISTOGRAMS, REQUEST_DURATION_METRIC, UrlTemplate};
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
        Self::try_with_default_transport(policy::admit_origin(origin)?, limits)
    }

    fn try_with_default_transport(
        origin: policy::Origin,
        limits: Limits,
    ) -> Result<Self, BuildError> {
        Ok(Self::with_transport(
            origin,
            limits,
            transport::tls_config()?.clone(),
            transport::Resolver::new(),
        ))
    }

    fn with_transport(
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
        Self::try_with_default_transport(policy::admit_test_http_origin(origin)?, limits)
    }

    pub async fn execute(
        &self,
        context: &OperationContext,
        request: Request<Bytes>,
    ) -> Result<Response<Bytes>, Error> {
        self.execute_buffered(context, request)
            .await
            .map_err(ExchangeError::into_error)
    }

    pub async fn execute_buffered(
        &self,
        context: impl Into<RequestContext>,
        request: Request<Bytes>,
    ) -> Result<Response<Bytes>, ExchangeError> {
        self.prepare_scoped(context.into(), request)
            .map_err(ExchangeError::BeforeResponse)?
            .send_with_response(|| true, |_| {}, buffer_response)
            .await
    }

    pub async fn execute_with_response_status(
        &self,
        context: &OperationContext,
        request: Request<Bytes>,
        on_status: impl FnOnce(StatusCode) + Send,
    ) -> Result<Response<Bytes>, Error> {
        self.prepare(context, request)?
            .send(|| true, on_status)
            .await
    }

    pub async fn execute_for_headers(
        &self,
        context: &OperationContext,
        request: Request<Bytes>,
    ) -> Result<http::response::Parts, Error> {
        self.prepare(context, request)?
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
        context: &OperationContext,
        request: Request<Bytes>,
    ) -> Result<PreparedRequest<'_>, Error> {
        self.prepare_scoped(context.into(), request)
    }

    fn prepare_scoped(
        &self,
        context: RequestContext,
        mut request: Request<Bytes>,
    ) -> Result<PreparedRequest<'_>, Error> {
        let context = context.for_request(
            Deadline::after(self.limits.operation_timeout),
            request.extensions_mut().remove::<OperationContext>(),
        );
        context.check()?;
        let request = policy::admit_request(&self.origin, request)?;
        Ok(PreparedRequest {
            client: self,
            request,
            context,
        })
    }
}

#[derive(derive_more::Debug)]
#[debug("PreparedRequest([REDACTED])")]
pub struct PreparedRequest<'a> {
    client: &'a Client,
    request: Request<Bytes>,
    context: RequestContext,
}

impl PreparedRequest<'_> {
    delegate::delegate! {
        to self.context {
            #[must_use]
            pub const fn deadline(&self) -> Option<Deadline>;

            #[must_use]
            pub fn stopped(&self) -> Option<Stopped>;

            pub async fn wait_stopped(&self) -> Stopped;
        }
    }

    #[must_use]
    pub const fn context(&self) -> &RequestContext {
        &self.context
    }

    pub fn set_authorization(&mut self, value: http::HeaderValue) {
        self.request
            .headers_mut()
            .insert(header::AUTHORIZATION, value);
    }

    pub async fn send(
        self,
        admit_dispatch: impl FnOnce() -> bool + Send,
        on_status: impl FnOnce(StatusCode) + Send,
    ) -> Result<Response<Bytes>, Error> {
        self.send_with_response(admit_dispatch, on_status, buffer_response)
            .await
            .map_err(ExchangeError::into_error)
    }

    async fn send_with_response<T>(
        self,
        admit_dispatch: impl FnOnce() -> bool + Send,
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
            context,
        } = self;
        context
            .check()
            .map_err(Error::from)
            .map_err(ExchangeError::BeforeResponse)?;
        let template = request.extensions_mut().remove::<UrlTemplate>();
        let mut attempt =
            observe::Attempt::start(request.method(), template, &client.server, &context);
        if client.propagate_trace_context {
            attempt.inject_trace_context(request.headers_mut());
        }
        let span = attempt.span();
        let exchange = async {
            context.check()?;
            let admitted = admit_dispatch();
            context.check()?;
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
            context.check()?;
            consume(response, client.limits.response_body_bytes, &context).await
        };
        let result = tokio::select! {
            biased;
            reason = context.wait_stopped() => Err(Error::from(reason)),
            result = exchange.instrument(span) => result,
        }
        .and_then(|response| {
            context.check()?;
            Ok(response)
        });
        let status = attempt.finish(result.as_ref().err());
        result.map_err(|source| match status {
            Some(status) => ExchangeError::AfterResponse { status, source },
            None => ExchangeError::BeforeResponse(source),
        })
    }
}

async fn buffer_response(
    response: Response<hyper::body::Incoming>,
    limit: usize,
    context: &RequestContext,
) -> Result<Response<Bytes>, Error> {
    if hyper::body::Body::size_hint(response.body())
        .exact()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(Error::ResponseBodyTooLarge);
    }

    let (parts, body) = response.into_parts();
    let buffered = collect_bounded_body(body, limit, context).await?;
    Ok(Response::from_parts(parts, buffered))
}

async fn collect_bounded_body<B>(
    body: B,
    limit: usize,
    context: &RequestContext,
) -> Result<Bytes, Error>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    enum Buffered {
        Empty,
        Single(Bytes),
        Multi(Vec<u8>),
    }

    let mut body = Limited::new(body, limit);
    let mut buffered = Buffered::Empty;
    loop {
        context.check()?;
        let Some(frame) = body.frame().await else {
            break;
        };
        if let Ok(data) = frame.map_err(map_body_error)?.into_data()
            && !data.is_empty()
        {
            match &mut buffered {
                Buffered::Empty => buffered = Buffered::Single(data),
                Buffered::Single(first) => {
                    let mut collected = Vec::new();
                    let target = first
                        .len()
                        .saturating_add(data.len())
                        .max(first.len().saturating_mul(2).min(limit));
                    collected.reserve_exact(target);
                    collected.extend_from_slice(first);
                    collected.extend_from_slice(&data);
                    buffered = Buffered::Multi(collected);
                }
                Buffered::Multi(collected) => {
                    append_with_capacity_cap(collected, &data, limit);
                }
            }
        }
    }
    Ok(match buffered {
        Buffered::Empty => Bytes::new(),
        Buffered::Single(single) => single,
        Buffered::Multi(collected) => Bytes::from(collected),
    })
}

impl From<Stopped> for Error {
    fn from(stopped: Stopped) -> Self {
        match stopped {
            Stopped::Deadline => Self::Timeout,
            Stopped::Cancelled => Self::Cancelled,
        }
    }
}

fn append_with_capacity_cap(collected: &mut Vec<u8>, data: &[u8], capacity_cap: usize) {
    let required = collected.len().saturating_add(data.len());
    if required > collected.capacity() {
        let target = required.max(collected.capacity().saturating_mul(2).min(capacity_cap));
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
