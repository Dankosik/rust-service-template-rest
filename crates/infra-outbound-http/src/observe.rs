use std::{error::Error as StdError, fmt, sync::Arc, time::Instant};

use http::{HeaderMap, Method, StatusCode};
use metrics::{Label, SharedString, Unit};
use tracing::Span;

use shared_rules::{
    error_chain::DisplayErrorChain,
    otel_http::{OTHER_METHOD, request_method},
};

use crate::{Error, StopScopes, policy::Origin};

pub const REQUEST_DURATION_METRIC: &str = "http_client_request_duration_seconds";

pub const REQUEST_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// A metric left out of this list renders the default buckets.
pub const HISTOGRAMS: &[(&str, &[f64])] = &[(REQUEST_DURATION_METRIC, REQUEST_DURATION_BUCKETS)];

/// OpenTelemetry `url.template`: insert as a request extension to name the operation. Use a
/// static template with placeholders for variable segments so requests for one operation
/// share a metric label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UrlTemplate(pub &'static str);

#[derive(Clone, Debug)]
pub(crate) struct Server {
    address: SharedString,
    port_label: SharedString,
}

impl Server {
    pub(crate) fn new(origin: &Origin) -> Self {
        Self {
            address: SharedString::from(origin.host_arc()),
            port_label: SharedString::from(Arc::<str>::from(origin.port().to_string())),
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
            Error::Transport { source } => Self::Failed(transport_error_type(&**source)),
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

/// [`Attempt::start`] returns a drop guard that borrows the stop scopes so it can record the
/// cause when its parent drops the exchange.
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
        port: u16,
        stops: &'a StopScopes<'a>,
    ) -> std::mem::DropGuard<Self, impl FnOnce(Self) + use<'a>> {
        let method = request_method(method.as_str());
        let template = template.map(|UrlTemplate(template)| template);
        let span = tracing::info_span!(
            "outbound_http",
            otel.name = %SpanName { method, template },
            otel.kind = "client",
            http.request.method = method,
            url.template = template,
            server.address = &*server.address,
            server.port = port,
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
                    Err(error) => Outcome::from_error(&error),
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

    pub(crate) fn finish<T>(self, result: &Result<T, Error>) {
        self.emit(
            result
                .as_ref()
                .map_or_else(Outcome::from_error, |_| Outcome::Response),
        );
        // Adapters map this error to their own closed outcome and drop its
        // source, so the client span is where it is told.
        if let Err(Error::Transport { source }) = result {
            // Hyper and hyper-util name only the failed step in their own
            // text and keep the reason in `source()`.
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
        // One call: a formatting layer may re-serialize every field per record.
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
        .record(self.started.elapsed().as_secs_f64());
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

/// `HTTP` stands for a method that is not a known one.
struct SpanName {
    method: &'static str,
    template: Option<&'static str>,
}

impl fmt::Display for SpanName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(if self.method == OTHER_METHOD {
            "HTTP"
        } else {
            self.method
        })?;
        match self.template {
            Some(template) => write!(formatter, " {template}"),
            None => Ok(()),
        }
    }
}

fn is_error_status(status: StatusCode) -> bool {
    status.as_u16() >= 400
}

// Static addresses let metrics retain labels for extension codes too; `StatusCode::as_str`
// borrows its receiver.
static STATUS_CODES: [StatusCode; 900] = {
    let mut codes = [StatusCode::CONTINUE; 900];
    let mut code = 100_u16;
    while code <= 999 {
        codes[(code - 100) as usize] = match StatusCode::from_u16(code) {
            Ok(status) => status,
            Err(_) => panic!("status table contains an invalid code"),
        };
        code += 1;
    }
    codes
};

fn status_label(status: StatusCode) -> &'static str {
    STATUS_CODES[usize::from(status.as_u16() - 100)].as_str()
}

/// Read from the typed causes the libraries retain: `tls` for certificate or handshake
/// refusal, `connect` when no connection was established (name resolution included),
/// `protocol` for a response head hyper refused to parse, `transport` for the rest.
fn transport_error_type(source: &(dyn StdError + 'static)) -> &'static str {
    let causes = || shared_rules::error_chain::causes(source);
    if causes().any(<dyn StdError>::is::<rustls::Error>) {
        "tls"
    } else if causes().any(|cause| {
        cause
            .downcast_ref::<hyper_util::client::legacy::Error>()
            .is_some_and(hyper_util::client::legacy::Error::is_connect)
    }) {
        "connect"
    } else if causes().any(|cause| {
        cause
            .downcast_ref::<hyper::Error>()
            .is_some_and(hyper::Error::is_parse)
    }) {
        "protocol"
    } else {
        "transport"
    }
}

#[cfg(test)]
mod tests;
