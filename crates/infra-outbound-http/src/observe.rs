use std::{error::Error as StdError, fmt, sync::Arc, time::Instant};

use http::{HeaderMap, Method, StatusCode};
use metrics::{Label, SharedString, Unit};
use tracing::Span;

use shared_rules::{
    error_chain::DisplayErrorChain,
    otel_http::{OTHER_METHOD, request_method},
};

use crate::{Error, policy::Origin};

/// OpenTelemetry `http.client.request.duration`, in the recorder's
/// Prometheus naming.
pub const REQUEST_DURATION_METRIC: &str = "http_client_request_duration_seconds";

/// Buckets in seconds for [`REQUEST_DURATION_METRIC`].
pub const REQUEST_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// Every histogram this crate records. A composition root hands it to the
/// Prometheus recorder; a metric left out renders the default buckets.
pub const HISTOGRAMS: &[(&str, &[f64])] = &[(REQUEST_DURATION_METRIC, REQUEST_DURATION_BUCKETS)];

/// The path template of a request, such as `/v1/items/{id}`: OpenTelemetry
/// `url.template`. Insert it as a request extension to tell one provider
/// operation from another; the attempt's span is then named
/// `{method} {template}` and both the span and the duration metric carry the
/// template. Supply a static template with placeholders for variable path
/// segments so requests for the same operation share a metric label.
///
/// ```
/// use infra_outbound_http::{Bytes, Request, UrlTemplate};
///
/// let mut request = Request::get("https://provider.example/v1/items/42")
///     .body(Bytes::new())
///     .expect("request");
/// request.extensions_mut().insert(UrlTemplate("/v1/items/{id}"));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UrlTemplate(pub &'static str);

/// Configured server identity, formatted once per client. Clones share it.
#[derive(Clone, Debug)]
pub(crate) struct Server {
    address: SharedString,
    port: u16,
    port_label: SharedString,
}

impl Server {
    pub(crate) fn new(origin: &Origin) -> Self {
        Self {
            address: Arc::<str>::from(origin.host()).into(),
            port: origin.port(),
            port_label: Arc::<str>::from(origin.port().to_string()).into(),
        }
    }
}

/// How an attempt ended, as its `outbound.outcome` label names it.
#[derive(Clone, Copy)]
enum Outcome {
    /// A response was buffered to its end, whatever its status.
    Response,
    /// The exchange failed with this `error.type`.
    Failed(&'static str),
    /// The caller cancelled or dropped the attempt before it ended.
    Cancelled,
}

impl Outcome {
    /// A cancelled caller is not a provider failure, as a dropped attempt.
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

/// One polled outbound attempt. The guard retains only bounded configured
/// identity, the caller's static template, and outcome state, never caller
/// request data or transport errors.
///
/// [`Attempt::finish`] borrows the result and logs a transport failure's
/// causes once; the guard itself keeps none.
pub(crate) struct Attempt {
    started: Instant,
    method: &'static str,
    template: Option<&'static str>,
    server: Server,
    status: Option<StatusCode>,
    span: Span,
    finalized: bool,
}

impl Attempt {
    pub(crate) fn start(method: &Method, template: Option<UrlTemplate>, server: &Server) -> Self {
        let method = request_method(method.as_str());
        let template = template.map(|UrlTemplate(template)| template);
        let span = tracing::info_span!(
            "outbound_http",
            otel.name = %SpanName { method, template },
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
        Self {
            started: Instant::now(),
            method,
            template,
            server: server.clone(),
            status: None,
            span,
            finalized: false,
        }
    }

    pub(crate) fn span(&self) -> Span {
        self.span.clone()
    }

    /// Writes this attempt's context with the process text-map propagator, so
    /// the provider's server span becomes a child of this client span.
    pub(crate) fn inject_trace_context(&self, headers: &mut HeaderMap) {
        tracing_opentelemetry_instrumentation_sdk::http::inject_context(
            &tracing_opentelemetry_instrumentation_sdk::find_context_from_tracing(&self.span),
            headers,
        );
    }

    pub(crate) fn record_status(&mut self, status: StatusCode) {
        self.status = Some(status);
    }

    pub(crate) fn finish<T>(mut self, result: &Result<T, Error>) {
        self.finalized = true;
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
        // An HTTP error status is its own `error.type`, sharing the status label.
        let error_type = match outcome {
            Outcome::Response => self
                .status
                .filter(|status| is_error_status(*status))
                .and(status_label.clone()),
            Outcome::Failed(class) => Some(SharedString::from(class)),
            Outcome::Cancelled => None,
        };
        // One call: a formatting layer may re-serialize every field per record.
        tracing::record_all!(
            self.span,
            http.response.status_code = self.status.map(|status| status.as_u16()),
            error.type = error_type.as_deref(),
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
        status_label: Option<SharedString>,
        error_type: Option<SharedString>,
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

impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.finalized {
            self.emit(Outcome::Cancelled);
        }
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

/// The OpenTelemetry HTTP client span name: `{method} {url.template}`, or the
/// method alone without a template; `HTTP` stands for a method that is not a
/// known one.
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

/// Every code `StatusCode` admits, 100 through 999, as three ASCII digits each.
/// `StatusCode::as_str` borrows a table like this one only for `&self`, so a
/// `'static` label would otherwise need an allocation per response.
const STATUS_DIGITS: &str = {
    const BYTES: [u8; 2700] = {
        let mut digits = [0; 2700];
        let mut offset = 0;
        let mut hundreds = b'1';
        while hundreds <= b'9' {
            let mut tens = b'0';
            while tens <= b'9' {
                let mut ones = b'0';
                while ones <= b'9' {
                    digits[offset] = hundreds;
                    digits[offset + 1] = tens;
                    digits[offset + 2] = ones;
                    offset += 3;
                    ones += 1;
                }
                tens += 1;
            }
            hundreds += 1;
        }
        digits
    };
    match std::str::from_utf8(&BYTES) {
        Ok(digits) => digits,
        Err(_) => "",
    }
};

fn status_label(status: StatusCode) -> SharedString {
    let offset = usize::from(status.as_u16() - 100) * 3;
    STATUS_DIGITS.get(offset..offset + 3).map_or_else(
        || SharedString::from(Arc::<str>::from(status.as_str())),
        SharedString::from,
    )
}

/// The class of a transport failure, read from the typed causes the libraries
/// retain: `tls` for a certificate or handshake refusal, `connect` when no
/// connection was established (name resolution included), `protocol` for a
/// response head hyper refused to parse, and `transport` for the rest, such
/// as a connection lost during the exchange.
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
