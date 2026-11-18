use std::{error::Error as StdError, fmt, sync::Arc, time::Instant};

use http::{HeaderMap, Method, StatusCode};
use metrics::{Label, SharedString, Unit};
use tracing::Span;

use shared_rules::{
    error_chain::{self, DisplayErrorChain},
    otel_http::{OTHER_METHOD, request_method, response_status_code},
};

use crate::{Error, RequestContext, policy::Origin};

pub const REQUEST_DURATION_METRIC: &str = "http_client_request_duration_seconds";

pub const REQUEST_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

pub const HISTOGRAMS: &[(&str, &[f64])] = &[(REQUEST_DURATION_METRIC, REQUEST_DURATION_BUCKETS)];

impl Error {
    #[must_use]
    pub fn diagnostic_reason(&self) -> &'static str {
        match self {
            Self::InvalidTarget => "target is invalid",
            Self::Timeout => "operation timed out",
            Self::Cancelled => "operation was cancelled",
            Self::DispatchRefused => "dispatch was refused",
            Self::ResponseBodyTooLarge => "response body is too large",
            Self::Transport { source } => describe_transport_error(&**source).diagnostic,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UrlTemplate(pub &'static str);

#[derive(Clone, Debug)]
pub(crate) struct Server {
    address: SharedString,
    port: u16,
    port_label: SharedString,
}

impl Server {
    pub(crate) fn new(origin: &Origin) -> Self {
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
            Error::DispatchRefused => Self::Failed("dispatch_refused"),
            Error::Timeout => Self::Failed("timeout"),
            Error::ResponseBodyTooLarge => Self::Failed("response_body_too_large"),
            Error::Transport { source } => Self::Failed(describe_transport_error(&**source).label),
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

pub(crate) struct Attempt {
    started: Instant,
    method: &'static str,
    template: Option<&'static str>,
    server: Server,
    status: Option<StatusCode>,
    span: Span,
}

impl Attempt {
    pub(crate) fn start<'a>(
        method: &Method,
        template: Option<UrlTemplate>,
        server: &Server,
        stops: &'a RequestContext,
    ) -> std::mem::DropGuard<Self, impl FnOnce(Self) + use<'a>> {
        let method = request_method(method.as_str());
        let template = template.map(|UrlTemplate(template)| template);
        let span = tracing::info_span!(
            "outbound_http",
            otel.name = %span_name(method, template),
            otel.kind = "client",
            http.request.method = method,
            url.template = template,
            server.address = &*server.address,
            server.port = server.port,
            http.response.status_code = tracing::field::Empty,
            error.type = tracing::field::Empty,
            outbound.outcome = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        std::mem::DropGuard::new(
            Self {
                started: Instant::now(),
                method,
                template,
                server: server.clone(),
                status: None,
                span,
            },
            move |attempt| {
                let outcome = match stops.check() {
                    Ok(()) => Outcome::Cancelled,
                    Err(reason) => Outcome::from_error(&Error::from(reason)),
                };
                attempt.emit(outcome);
            },
        )
    }

    pub(crate) fn span(&self) -> Span {
        self.span.clone()
    }

    pub(crate) fn inject_trace_context(&self, headers: &mut HeaderMap) {
        tracing_opentelemetry_instrumentation_sdk::http::inject_context(
            &tracing_opentelemetry_instrumentation_sdk::find_context_from_tracing(&self.span),
            headers,
        );
    }

    pub(crate) fn record_status(&mut self, status: StatusCode) {
        self.status = Some(status);
    }

    pub(crate) fn status(&self) -> Option<StatusCode> {
        self.status
    }

    pub(crate) fn finish(self, error: Option<&Error>) {
        self.emit(error.map_or(Outcome::Response, Outcome::from_error));
        if let Some(Error::Transport { source }) = error {
            self.span.in_scope(|| {
                tracing::warn!(
                    error = %DisplayErrorChain(&**source),
                    "outbound_http_transport_failed"
                );
            });
        }
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
        describe_histogram();
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
            Label::new("http.request.method", self.method),
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

fn describe_histogram() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        metrics::describe_histogram!(
            REQUEST_DURATION_METRIC,
            Unit::Seconds,
            "Outbound HTTP attempt duration in seconds"
        );
    });
}

fn span_name(method: &'static str, template: Option<&'static str>) -> impl fmt::Display {
    fmt::from_fn(move |formatter| {
        formatter.write_str(if method == OTHER_METHOD {
            "HTTP"
        } else {
            method
        })?;
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

struct TransportErrorDescription {
    label: &'static str,
    diagnostic: &'static str,
}

fn describe_transport_error(source: &(dyn StdError + 'static)) -> TransportErrorDescription {
    let causes = || error_chain::causes(source);
    if error_chain::contains::<rustls::Error>(source) {
        TransportErrorDescription {
            label: "tls",
            diagnostic: "HTTP transport failed during the TLS exchange",
        }
    } else if causes()
        .filter_map(<dyn StdError>::downcast_ref::<hyper_util::client::legacy::Error>)
        .any(hyper_util::client::legacy::Error::is_connect)
    {
        TransportErrorDescription {
            label: "connect",
            diagnostic: "HTTP transport failed during connection setup",
        }
    } else if causes()
        .filter_map(<dyn StdError>::downcast_ref::<hyper::Error>)
        .any(hyper::Error::is_parse)
    {
        TransportErrorDescription {
            label: "protocol",
            diagnostic: "HTTP transport failed while parsing the response",
        }
    } else {
        TransportErrorDescription {
            label: "transport",
            diagnostic: "HTTP transport failed",
        }
    }
}

#[cfg(test)]
mod tests;
