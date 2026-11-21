use std::{error::Error as StdError, fmt, mem::DropGuard, sync::Arc, time::Instant};

use http::{HeaderMap, Method, Request, StatusCode};
use metrics::{Label, SharedString, Unit};
use operation_context::{OperationContext, Stopped};
use tracing::Span;

use shared_rules::{
    error_chain::{self, DisplayErrorChain},
    otel_http::{HttpMethod, response_status_code},
};

use crate::{Error, TransportErrorKind, policy::TargetOrigin};

pub(crate) const REQUEST_DURATION_METRIC: &str = "http_client_request_duration_seconds";

pub(crate) const REQUEST_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

pub const HISTOGRAMS: &[(&str, &[f64])] = &[(REQUEST_DURATION_METRIC, REQUEST_DURATION_BUCKETS)];

impl TransportErrorKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Tls => "tls",
            Self::Connect => "connect",
            Self::Protocol => "protocol",
            Self::Other => "transport",
        }
    }

    const fn diagnostic(self) -> &'static str {
        match self {
            Self::Tls => "HTTP transport failed during the TLS exchange",
            Self::Connect => "HTTP transport failed during connection setup",
            Self::Protocol => "HTTP transport failed while parsing the response",
            Self::Other => "HTTP transport failed",
        }
    }
}

impl Error {
    #[must_use]
    pub fn transport_kind(&self) -> Option<TransportErrorKind> {
        match self {
            Self::Transport { source } => Some(classify_transport_error(&**source)),
            Self::InvalidTarget | Self::Timeout | Self::Cancelled | Self::ResponseBodyTooLarge => {
                None
            }
        }
    }

    #[must_use]
    pub fn diagnostic_reason(&self) -> &'static str {
        match self {
            Self::InvalidTarget => "target is invalid",
            Self::Timeout => "operation timed out",
            Self::Cancelled => "operation was cancelled",
            Self::ResponseBodyTooLarge => "response body is too large",
            Self::Transport { source } => classify_transport_error(&**source).diagnostic(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UrlTemplate(pub &'static str);

impl UrlTemplate {
    #[must_use]
    pub fn attach<B>(self, mut request: Request<B>) -> Request<B> {
        request.extensions_mut().insert(self);
        request
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Server {
    address: SharedString,
    port: u16,
    port_label: SharedString,
}

impl Server {
    pub(crate) fn new(origin: &TargetOrigin) -> Self {
        let port = origin.port();
        let mut buffer = fmt::NumBuffer::new();
        Self {
            address: SharedString::from(origin.host_arc()),
            port,
            port_label: SharedString::from(Arc::<str>::from(port.format_into(&mut buffer))),
        }
    }
}

#[derive(Clone, Copy)]
enum Outcome {
    Response,
    Failed(&'static str),
    Cancelled,
}

impl Outcome {
    fn from_error(error: &Error) -> Self {
        match error {
            Error::Cancelled => Self::Cancelled,
            Error::InvalidTarget => Self::Failed("invalid_target"),
            Error::Timeout => Self::Failed("timeout"),
            Error::ResponseBodyTooLarge => Self::Failed("response_body_too_large"),
            Error::Transport { source } => {
                Self::Failed(classify_transport_error(&**source).label())
            }
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Response => "response",
            Self::Failed(_) => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

struct AttemptInner<'a> {
    started: Instant,
    method: HttpMethod,
    template: Option<&'static str>,
    server: &'a Server,
    context: &'a OperationContext,
    status: Option<StatusCode>,
    span: Span,
}

impl AttemptInner<'_> {
    fn abandon(self) {
        let outcome = match self.context.check() {
            Ok(()) | Err(Stopped::Cancelled) => Outcome::Cancelled,
            Err(Stopped::Deadline) => Outcome::Failed("timeout"),
        };
        self.emit(outcome);
    }

    fn emit(&self, outcome: Outcome) {
        let status_label = self.status.map(status_label);
        let error_type = match outcome {
            Outcome::Response if self.status.is_some_and(is_error_status) => status_label,
            Outcome::Failed(class) => Some(class),
            Outcome::Response | Outcome::Cancelled => None,
        };
        tracing::record_all!(
            self.span,
            http.response.status_code = self.status.map(|status| status.as_u16()),
            error.type = error_type,
            outbound.outcome = outcome.as_str(),
            otel.status_code = error_type.is_some().then_some("ERROR"),
        );
        metrics::histogram!(
            REQUEST_DURATION_METRIC,
            self.labels(outcome, status_label, error_type)
        )
        .record(self.started.elapsed());
    }

    fn labels(
        &self,
        outcome: Outcome,
        status_label: Option<&'static str>,
        error_type: Option<&'static str>,
    ) -> Vec<Label> {
        let mut labels = Vec::with_capacity(
            4 + usize::from(self.template.is_some())
                + usize::from(status_label.is_some())
                + usize::from(error_type.is_some()),
        );
        labels.extend([
            Label::new("http.request.method", self.method.as_str()),
            Label::new("server.address", self.server.address.clone()),
            Label::new("server.port", self.server.port_label.clone()),
            Label::new("outbound.outcome", outcome.as_str()),
        ]);
        if let Some(template) = self.template {
            labels.push(Label::new("url.template", template));
        }
        if let Some(status_label) = status_label {
            labels.push(Label::new("http.response.status_code", status_label));
        }
        if let Some(error_type) = error_type {
            labels.push(Label::new("error.type", error_type));
        }
        labels
    }
}

pub(crate) struct Attempt<'a>(DropGuard<AttemptInner<'a>, fn(AttemptInner<'a>)>);

impl<'a> Attempt<'a> {
    pub(crate) fn start(
        method: &Method,
        template: Option<UrlTemplate>,
        server: &'a Server,
        context: &'a OperationContext,
    ) -> Self {
        let method = HttpMethod::parse(method.as_str());
        let template = template.map(|UrlTemplate(template)| template);
        let span = tracing::info_span!(
            "outbound_http",
            otel.name = %span_name(method, template),
            otel.kind = "client",
            http.request.method = method.as_str(),
            url.template = template,
            server.address = &*server.address,
            server.port = server.port,
            http.response.status_code = tracing::field::Empty,
            error.type = tracing::field::Empty,
            outbound.outcome = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        Self(DropGuard::new(
            AttemptInner {
                started: Instant::now(),
                method,
                template,
                server,
                context,
                status: None,
                span,
            },
            AttemptInner::abandon,
        ))
    }

    pub(crate) fn span(&self) -> Span {
        self.0.span.clone()
    }

    pub(crate) fn inject_trace_context(&self, headers: &mut HeaderMap) {
        tracing_opentelemetry_instrumentation_sdk::http::inject_context(
            &tracing_opentelemetry_instrumentation_sdk::find_context_from_tracing(&self.0.span),
            headers,
        );
    }

    pub(crate) fn record_status(&mut self, status: StatusCode) {
        self.0.status = Some(status);
    }

    pub(crate) fn finish_response(self) {
        DropGuard::dismiss(self.0).emit(Outcome::Response);
    }

    pub(crate) fn finish_error(self, error: &Error) -> Option<StatusCode> {
        let inner = DropGuard::dismiss(self.0);
        inner.emit(Outcome::from_error(error));
        if let Error::Transport { source } = error {
            inner.span.in_scope(|| {
                tracing::warn!(
                    error = %DisplayErrorChain(&**source),
                    "outbound_http_transport_failed"
                );
            });
        }
        inner.status
    }

    pub(crate) fn finish_dispatch_refused(self) {
        let inner = DropGuard::dismiss(self.0);
        inner.emit(Outcome::Failed("dispatch_refused"));
    }
}

pub(crate) fn describe() {
    metrics::describe_histogram!(
        REQUEST_DURATION_METRIC,
        Unit::Seconds,
        "Outbound HTTP attempt duration in seconds"
    );
}

fn span_name(method: HttpMethod, template: Option<&'static str>) -> impl fmt::Display {
    fmt::from_fn(move |formatter| {
        formatter.write_str(method.span_name_prefix())?;
        match template {
            Some(template) => write!(formatter, " {template}"),
            None => Ok(()),
        }
    })
}

fn is_error_status(status: StatusCode) -> bool {
    status.as_u16() >= 400
}

fn status_label(status: StatusCode) -> &'static str {
    response_status_code(status.as_u16()).unwrap_or_default()
}

fn classify_transport_error(source: &(dyn StdError + 'static)) -> TransportErrorKind {
    let causes = || error_chain::causes(source);
    if error_chain::contains::<rustls::Error>(source) {
        TransportErrorKind::Tls
    } else if causes()
        .filter_map(<dyn StdError>::downcast_ref::<hyper_util::client::legacy::Error>)
        .any(hyper_util::client::legacy::Error::is_connect)
    {
        TransportErrorKind::Connect
    } else if causes()
        .filter_map(<dyn StdError>::downcast_ref::<hyper::Error>)
        .any(hyper::Error::is_parse)
    {
        TransportErrorKind::Protocol
    } else {
        TransportErrorKind::Other
    }
}

#[cfg(test)]
mod tests;
