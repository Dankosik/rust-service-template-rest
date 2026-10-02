use std::{error::Error as StdError, fmt, sync::Arc, time::Instant};

use http::{HeaderMap, Method, StatusCode};
use metrics::{Label, SharedString, Unit};
use tracing::Span;

use crate::{Error, policy::Target};

/// OpenTelemetry `http.client.request.duration`, in the recorder's
/// Prometheus naming.
pub const REQUEST_DURATION_METRIC: &str = "http_client_request_duration_seconds";

/// Buckets in seconds for [`REQUEST_DURATION_METRIC`]; the composition root
/// passes both to the Prometheus recorder.
pub const REQUEST_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// The path template of a request, such as `/v1/items/{id}`: OpenTelemetry
/// `url.template`. Insert it as a request extension to tell one provider
/// operation from another; the attempt's span is then named
/// `{method} {template}` and both the span and the duration metric carry the
/// template. It is a metric label, so it is a literal with placeholders and
/// never a formatted path.
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
    port: SharedString,
    port_number: u16,
}

impl Server {
    pub(crate) fn new(target: &Target) -> Self {
        Self {
            address: Arc::<str>::from(target.host()).into(),
            port: Arc::<str>::from(target.port().to_string()).into(),
            port_number: target.port(),
        }
    }
}

/// One polled outbound attempt. The guard retains only bounded configured
/// identity, the caller's static template, and outcome state, never caller
/// request data or transport errors.
/// A transport failure's cause is logged once, when the attempt finishes.
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
        let method = bounded_method(method);
        let template = template.map(|UrlTemplate(template)| template);
        let span = tracing::info_span!(
            "outbound_http",
            otel.name = %SpanName { method, template },
            otel.kind = "client",
            http.request.method = method,
            url.template = template,
            server.address = &*server.address,
            server.port = server.port_number,
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

    pub(crate) fn response_headers(&mut self, status: StatusCode) {
        self.status = Some(status);
    }

    pub(crate) fn finish<T>(&mut self, result: &Result<T, Error>) {
        if self.finalized {
            return;
        }
        self.finalized = true;
        match result {
            Ok(_) => {
                let error_type = self.status.and_then(http_error_type);
                let is_http_error = error_type.is_some();
                self.emit("response", error_type, is_http_error);
            }
            Err(error) => {
                self.emit("error", Some(error_type(error).into()), false);
                // Adapters map this error to their own closed outcome and
                // drop its source, so the client span is where it is told.
                if let Error::Transport { source } = error {
                    self.span.in_scope(|| {
                        tracing::warn!(
                            error = %Causes(&**source),
                            "outbound_http_transport_failed"
                        );
                    });
                }
            }
        }
    }

    fn emit(&self, outcome: &'static str, error_type: Option<SharedString>, is_http_error: bool) {
        // One call: a formatting layer may re-serialize every field per record.
        tracing::record_all!(
            self.span,
            http.response.status_code = self.status.map(|status| status.as_u16()),
            error.type = error_type.as_deref(),
            outbound.outcome = outcome,
            otel.status_code = error_type.is_some().then_some("ERROR"),
        );

        describe_histogram();
        let mut labels = Vec::with_capacity(
            4 + usize::from(self.template.is_some())
                + usize::from(self.status.is_some())
                + usize::from(error_type.is_some()),
        );
        labels.extend([
            Label::new("http.request.method", self.method),
            Label::new("server.address", self.server.address.clone()),
            Label::new("server.port", self.server.port.clone()),
            Label::new("outbound.outcome", outcome),
        ]);
        if let Some(template) = self.template {
            labels.push(Label::new("url.template", template));
        }
        let error_type = match (self.status, is_http_error, error_type) {
            (Some(_), true, Some(error_type)) => {
                labels.push(Label::new("http.response.status_code", error_type.clone()));
                labels.push(Label::new("error.type", error_type));
                None
            }
            (Some(status), _, error_type) => {
                labels.push(Label::new(
                    "http.response.status_code",
                    status.as_u16().to_string(),
                ));
                error_type
            }
            (None, _, error_type) => error_type,
        };
        if let Some(error_type) = error_type {
            labels.push(Label::new("error.type", error_type));
        }
        metrics::histogram!(REQUEST_DURATION_METRIC, labels)
            .record(self.started.elapsed().as_secs_f64());
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.finalized {
            self.emit("cancelled", None, false);
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

fn bounded_method(method: &Method) -> &'static str {
    match method.as_str() {
        "GET" => "GET",
        "HEAD" => "HEAD",
        "POST" => "POST",
        "PUT" => "PUT",
        "DELETE" => "DELETE",
        "CONNECT" => "CONNECT",
        "OPTIONS" => "OPTIONS",
        "TRACE" => "TRACE",
        "PATCH" => "PATCH",
        "QUERY" => "QUERY",
        _ => "_OTHER",
    }
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
        formatter.write_str(if self.method == "_OTHER" {
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

fn http_error_type(status: StatusCode) -> Option<SharedString> {
    if status.is_client_error() || status.is_server_error() || status.as_u16() >= 600 {
        Some(Arc::<str>::from(status.as_str()).into())
    } else {
        None
    }
}

fn error_type(error: &Error) -> &'static str {
    match error {
        Error::InvalidTarget => "invalid_target",
        Error::Timeout => "timeout",
        Error::ResponseBodyTooLarge => "response_body_too_large",
        Error::Transport { source } => transport_error_type(&**source),
    }
}

/// The class of a transport failure, read from the typed causes the libraries
/// retain: `tls` for a certificate or handshake refusal, `connect` when no
/// connection was established (name resolution included), `protocol` for a
/// response head hyper refused to parse, and `transport` for the rest, such
/// as a connection lost during the exchange.
fn transport_error_type(source: &(dyn StdError + 'static)) -> &'static str {
    let mut class = "transport";
    for cause in std::iter::successors(Some(source), |cause| next_cause(*cause)) {
        if cause.is::<rustls::Error>() {
            return "tls";
        }
        if cause
            .downcast_ref::<hyper_util::client::legacy::Error>()
            .is_some_and(hyper_util::client::legacy::Error::is_connect)
        {
            class = "connect";
        }
        if cause
            .downcast_ref::<hyper::Error>()
            .is_some_and(hyper::Error::is_parse)
        {
            class = "protocol";
        }
    }
    class
}

/// The cause one level down. An I/O error holds its custom error as a payload
/// that `source()` skips, and the TLS connector hands over a rustls error
/// that way.
fn next_cause<'a>(error: &'a (dyn StdError + 'static)) -> Option<&'a (dyn StdError + 'static)> {
    match error
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::get_ref)
    {
        Some(payload) => Some(payload),
        None => error.source(),
    }
}

/// An error and its sources on one line. Hyper and hyper-util name only the
/// failed step in their own text and keep the reason in `source()`.
struct Causes<'a>(&'a (dyn StdError + 'static));

impl fmt::Display for Causes<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)?;
        for cause in std::iter::successors(self.0.source(), |cause| (*cause).source()) {
            write!(formatter, ": {cause}")?;
        }
        Ok(())
    }
}
