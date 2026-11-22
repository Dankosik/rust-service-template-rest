mod observe;
mod policy;
mod transport;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("outbound HTTP limits are invalid")]
pub struct InvalidLimits;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    operation_timeout: Duration,
    response_header_count: usize,
    response_body_bytes: usize,
}

impl Limits {
    #[must_use]
    pub const fn new(
        operation_timeout: Duration,
        response_header_count: usize,
        response_body_bytes: usize,
    ) -> Self {
        assert!(
            !operation_timeout.is_zero(),
            "outbound HTTP operation_timeout must be non-zero"
        );
        assert!(
            response_header_count > 0 && response_header_count <= MAX_RESPONSE_HEADER_COUNT,
            "outbound HTTP response_header_count must be between 1 and MAX_RESPONSE_HEADER_COUNT"
        );
        assert!(
            response_body_bytes > 0,
            "outbound HTTP response_body_bytes must be non-zero"
        );
        Self {
            operation_timeout,
            response_header_count,
            response_body_bytes,
        }
    }

    pub const fn try_new(
        operation_timeout: Duration,
        response_header_count: usize,
        response_body_bytes: usize,
    ) -> Result<Self, InvalidLimits> {
        if operation_timeout.is_zero()
            || response_header_count == 0
            || response_header_count > MAX_RESPONSE_HEADER_COUNT
            || response_body_bytes == 0
        {
            return Err(InvalidLimits);
        }
        Ok(Self {
            operation_timeout,
            response_header_count,
            response_body_bytes,
        })
    }

    #[must_use]
    pub const fn operation_timeout(self) -> Duration {
        self.operation_timeout
    }

    #[must_use]
    pub const fn response_header_count(self) -> usize {
        self.response_header_count
    }

    #[must_use]
    pub const fn response_body_bytes(self) -> usize {
        self.response_body_bytes
    }

    pub const fn with_operation_timeout(
        self,
        operation_timeout: Duration,
    ) -> Result<Self, InvalidLimits> {
        Self::try_new(
            operation_timeout,
            self.response_header_count,
            self.response_body_bytes,
        )
    }

    pub const fn with_response_header_count(
        self,
        response_header_count: usize,
    ) -> Result<Self, InvalidLimits> {
        Self::try_new(
            self.operation_timeout,
            response_header_count,
            self.response_body_bytes,
        )
    }

    pub const fn with_response_body_bytes(
        self,
        response_body_bytes: usize,
    ) -> Result<Self, InvalidLimits> {
        Self::try_new(
            self.operation_timeout,
            self.response_header_count,
            response_body_bytes,
        )
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
#[error("outbound HTTP TLS configuration failed")]
pub struct TlsError {
    #[source]
    source: BoxError,
}

impl TlsError {
    pub(crate) fn new(source: BoxError) -> Self {
        Self { source }
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
    pub const fn error(&self) -> &Error {
        match self {
            Self::BeforeResponse(source) | Self::AfterResponse { source, .. } => source,
        }
    }

    #[must_use]
    pub const fn status(&self) -> Option<StatusCode> {
        match self {
            Self::BeforeResponse(_) => None,
            Self::AfterResponse { status, .. } => Some(*status),
        }
    }

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
    origin: policy::TargetOrigin,
    #[debug(skip)]
    server: observe::Server,
    limits: Limits,
    propagate_trace_context: bool,
    #[debug(skip)]
    transport: transport::Transport,
}

impl Client {
    #[must_use]
    pub fn host(&self) -> &str {
        self.origin.host()
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.origin.port()
    }

    pub fn uri(&self, path_and_query: &str) -> Result<Uri, http::Error> {
        self.origin.uri(path_and_query)
    }

    #[must_use]
    pub fn matches_origin(&self, origin: &Uri) -> bool {
        origin
            .path_and_query()
            .is_none_or(|pq| pq.query().is_none() && matches!(pq.path(), "" | "/"))
            && self.origin.admits(origin)
    }

    pub fn try_new(origin: Origin, limits: Limits) -> Result<Self, TlsError> {
        Self::try_with_default_transport(policy::TargetOrigin::Https(origin), limits)
    }

    pub(crate) fn try_with_default_transport(
        origin: policy::TargetOrigin,
        limits: Limits,
    ) -> Result<Self, TlsError> {
        Ok(Self::with_transport(
            origin,
            limits,
            transport::tls_config()?.clone(),
            transport::Resolver::new(),
        ))
    }

    pub(crate) fn with_transport(
        origin: policy::TargetOrigin,
        limits: Limits,
        tls: rustls::ClientConfig,
        resolver: transport::Resolver,
    ) -> Self {
        observe::describe();
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

    pub async fn execute(
        &self,
        context: &OperationContext,
        request: Request<Bytes>,
    ) -> Result<Response<Bytes>, ExchangeError> {
        self.prepare(context, request)
            .map_err(ExchangeError::BeforeResponse)?
            .send()
            .await
    }

    pub async fn execute_for_headers(
        &self,
        context: &OperationContext,
        request: Request<Bytes>,
    ) -> Result<Parts, Error> {
        self.prepare(context, request)?
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
        context: &OperationContext,
        request: Request<Bytes>,
    ) -> Result<PreparedRequest<'_>, Error> {
        let context = context.child_until(Deadline::after(self.limits.operation_timeout()));
        context.check()?;
        let request = policy::admit_request(&self.origin, request)?;
        Ok(PreparedRequest {
            client: self,
            request,
            context,
        })
    }
}

#[must_use = "send the prepared request or drop it intentionally"]
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

    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        self.request.headers()
    }

    pub fn set_authorization(&mut self, value: HeaderValue) {
        self.request
            .headers_mut()
            .insert(header::AUTHORIZATION, value);
    }

    pub async fn send(self) -> Result<Response<Bytes>, ExchangeError> {
        self.send_with_response(|| Ok::<(), Infallible>(()), |_| {}, buffer_response)
            .await
            .map_err(into_exchange_error)
    }

    pub async fn send_checked<E>(
        self,
        admit_dispatch: impl FnOnce() -> Result<(), E>,
        on_status: impl FnOnce(StatusCode),
    ) -> Result<Response<Bytes>, DispatchError<E>> {
        self.send_with_response(admit_dispatch, on_status, buffer_response)
            .await
    }

    async fn send_with_response<T, E>(
        self,
        admit_dispatch: impl FnOnce() -> Result<(), E>,
        on_status: impl FnOnce(StatusCode),
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

        impl<E> From<Error> for SendError<E> {
            fn from(error: Error) -> Self {
                Self::Error(error)
            }
        }

        impl<E> From<Stopped> for SendError<E> {
            fn from(stopped: Stopped) -> Self {
                Self::Error(stopped.into())
            }
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
            context.check()?;
            let admitted = admit_dispatch();
            context.check()?;
            if let Err(refused) = admitted {
                return Err(SendError::Refused(refused));
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
            consume(response, client.limits.response_body_bytes(), &context)
                .await
                .map_err(SendError::from)
        };
        let result = tokio::select! {
            biased;
            reason = context.wait_stopped() => Err(SendError::from(reason)),
            result = exchange.instrument(span) => result,
        }
        .and_then(|response| {
            context.check()?;
            Ok(response)
        });
        match result {
            Ok(value) => {
                attempt.finish_response();
                Ok(value)
            }
            Err(SendError::Refused(refused)) => {
                attempt.finish_dispatch_refused();
                Err(DispatchError::Refused(refused))
            }
            Err(SendError::Error(error)) => {
                let status = attempt.finish_error(&error);
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
    let capacity_cap = match body.size_hint().exact() {
        Some(length) => match usize::try_from(length) {
            Ok(exact) if exact <= limit => exact,
            _ => return Err(Error::ResponseBodyTooLarge),
        },
        None => limit,
    };
    let mut body = Limited::new(body, limit);
    let mut collected = Vec::new();
    loop {
        context.check()?;
        let Some(frame) = body.frame().await else {
            break;
        };
        if let Ok(data) = frame.map_err(map_body_error)?.into_data()
            && !data.is_empty()
        {
            append_with_capacity_cap(&mut collected, &data, capacity_cap);
        }
    }
    Ok(Bytes::from(collected))
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
