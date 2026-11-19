mod observe;
mod policy;
mod transport;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

use std::{convert::Infallible, time::Duration};

use http_body_util::{BodyExt as _, Full, Limited};
use operation_context::{Deadline, Stopped};
use tracing::Instrument as _;

pub use bytes::Bytes;
pub use http::{
    HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Uri, header, response::Parts,
};
pub use observe::{HISTOGRAMS, UrlTemplate};
pub use operation_context::OperationContext;
pub use policy::{InvalidOrigin, Origin};
pub use transport::prepare_tls;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportErrorKind {
    Tls,
    Connect,
    Protocol,
    Other,
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

impl From<InvalidOrigin> for BuildError {
    fn from(_: InvalidOrigin) -> Self {
        Self::InvalidOrigin
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("outbound HTTP target is invalid")]
    InvalidTarget,
    #[error("outbound HTTP operation timed out")]
    Timeout,
    #[error("outbound HTTP operation was cancelled")]
    Cancelled,
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

impl From<ExchangeError> for Error {
    fn from(error: ExchangeError) -> Self {
        error.into_error()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError<E> {
    #[error("outbound HTTP dispatch was refused")]
    Refused(#[source] E),
    #[error(transparent)]
    Exchange(#[from] ExchangeError),
}

#[derive(Clone, derive_more::Debug)]
pub struct Client {
    origin: Origin,
    #[debug(skip)]
    server: observe::Server,
    limits: Limits,
    propagate_trace_context: bool,
    #[debug(skip)]
    transport: transport::Transport,
}

impl Client {
    #[must_use]
    pub const fn origin(&self) -> &Origin {
        &self.origin
    }

    #[must_use]
    pub fn matches_origin(&self, origin: &Uri) -> bool {
        origin
            .path_and_query()
            .is_none_or(|pq| pq.query().is_none() && matches!(pq.path(), "" | "/"))
            && self.origin.admits(origin)
    }

    pub fn try_new(origin: Origin, limits: Limits) -> Result<Self, BuildError> {
        limits.validate()?;
        if !origin.is_https() {
            return Err(BuildError::InvalidOrigin);
        }
        Self::try_with_default_transport(origin, limits)
    }

    fn try_with_default_transport(origin: Origin, limits: Limits) -> Result<Self, BuildError> {
        Ok(Self::with_transport(
            origin,
            limits,
            transport::tls_config()?.clone(),
            transport::Resolver::new(),
        ))
    }

    fn with_transport(
        origin: Origin,
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
    pub fn try_new_for_test_http(origin: &str, limits: Limits) -> Result<Self, BuildError> {
        limits.validate()?;
        Self::try_with_default_transport(policy::admit_test_http_origin(origin)?, limits)
    }

    #[cfg(feature = "test-support")]
    pub fn try_new_for_test_https(
        origin: Origin,
        limits: Limits,
        root_der: &[u8],
        address: std::net::SocketAddr,
    ) -> Result<Self, BuildError> {
        limits.validate()?;
        if !origin.is_https() || !address.ip().is_loopback() {
            return Err(BuildError::InvalidOrigin);
        }
        let tls = transport::tls_config_with_root(root_der)?;
        Ok(Self::with_transport(
            origin,
            limits,
            tls,
            transport::Resolver::Fixture(transport::FixtureResolver::with_addresses(vec![address])),
        ))
    }

    pub async fn execute(
        &self,
        request: Request<Bytes>,
        context: &OperationContext,
    ) -> Result<Response<Bytes>, ExchangeError> {
        self.prepare(request, context)
            .map_err(ExchangeError::BeforeResponse)?
            .send_with_response(|| Ok::<(), Infallible>(()), |_| {}, buffer_response)
            .await
            .map_err(into_exchange_error)
    }

    pub async fn execute_for_headers(
        &self,
        request: Request<Bytes>,
        context: &OperationContext,
    ) -> Result<Parts, Error> {
        self.prepare(request, context)?
            .send_with_response(
                || Ok::<(), Infallible>(()),
                |_| {},
                async |response, _, _| Ok(response.into_parts().0),
            )
            .await
            .map_err(|error| into_exchange_error(error).into_error())
    }

    pub fn prepare(
        &self,
        request: Request<Bytes>,
        context: &OperationContext,
    ) -> Result<PreparedRequest<'_>, Error> {
        let context = context.child_until(Deadline::after(self.limits.operation_timeout));
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
    context: OperationContext,
}

impl PreparedRequest<'_> {
    #[must_use]
    pub const fn context(&self) -> &OperationContext {
        &self.context
    }

    pub fn set_authorization(&mut self, value: HeaderValue) {
        self.request
            .headers_mut()
            .insert(header::AUTHORIZATION, value);
    }

    pub async fn send_checked<E>(
        self,
        admit_dispatch: impl FnOnce() -> Result<(), E> + Send,
        on_status: impl FnOnce(StatusCode) + Send,
    ) -> Result<Response<Bytes>, DispatchError<E>> {
        self.send_with_response(admit_dispatch, on_status, buffer_response)
            .await
    }

    async fn send_with_response<T, E>(
        self,
        admit_dispatch: impl FnOnce() -> Result<(), E> + Send,
        on_status: impl FnOnce(StatusCode) + Send,
        consume: impl AsyncFnOnce(
            Response<hyper::body::Incoming>,
            usize,
            &OperationContext,
        ) -> Result<T, Error>,
    ) -> Result<T, DispatchError<E>> {
        enum SendError<E> {
            Refused(E),
            Error(Error),
        }

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
            context.check().map_err(|s| SendError::Error(s.into()))?;
            let admitted = admit_dispatch();
            context.check().map_err(|s| SendError::Error(s.into()))?;
            if let Err(refused) = admitted {
                return Err(SendError::Refused(refused));
            }
            let response = client
                .transport
                .request(request.map(Full::new))
                .await
                .map_err(|source| {
                    SendError::Error(Error::Transport {
                        source: Box::new(source),
                    })
                })?;
            attempt.record_status(response.status());
            on_status(response.status());
            context.check().map_err(|s| SendError::Error(s.into()))?;
            consume(response, client.limits.response_body_bytes, &context)
                .await
                .map_err(SendError::Error)
        };
        let result = tokio::select! {
            biased;
            reason = context.wait_stopped() => Err(SendError::Error(Error::from(reason))),
            result = exchange.instrument(span) => result,
        }
        .and_then(|response| {
            context.check().map_err(|s| SendError::Error(s.into()))?;
            Ok(response)
        });
        let status = attempt.status();
        match result {
            Ok(value) => {
                std::mem::DropGuard::dismiss(attempt).finish(None);
                Ok(value)
            }
            Err(SendError::Refused(refused)) => {
                std::mem::DropGuard::dismiss(attempt).finish_dispatch_refused();
                Err(DispatchError::Refused(refused))
            }
            Err(SendError::Error(error)) => {
                std::mem::DropGuard::dismiss(attempt).finish(Some(&error));
                Err(DispatchError::Exchange(match status {
                    Some(status) => ExchangeError::AfterResponse {
                        status,
                        source: error,
                    },
                    None => ExchangeError::BeforeResponse(error),
                }))
            }
        }
    }
}

fn into_exchange_error(error: DispatchError<Infallible>) -> ExchangeError {
    match error {
        DispatchError::Refused(never) => match never {},
        DispatchError::Exchange(exchange) => exchange,
    }
}

async fn buffer_response(
    response: Response<hyper::body::Incoming>,
    limit: usize,
    context: &OperationContext,
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
    context: &OperationContext,
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
