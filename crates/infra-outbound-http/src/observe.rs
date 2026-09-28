use std::{sync::Arc, time::Instant};

use http::{Method, StatusCode};
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
/// identity and outcome state, never caller request data or transport errors.
pub(crate) struct Attempt {
    started: Instant,
    method: &'static str,
    server: Server,
    status: Option<StatusCode>,
    span: Span,
    finalized: bool,
}

impl Attempt {
    pub(crate) fn start(method: &Method, server: &Server) -> Self {
        let method = bounded_method(method);
        let span = tracing::info_span!(
            "outbound_http",
            otel.kind = "client",
            http.request.method = method,
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
            server: server.clone(),
            status: None,
            span,
            finalized: false,
        }
    }

    pub(crate) fn span(&self) -> Span {
        self.span.clone()
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
            Err(error) => self.emit("error", Some(error_type(error).into()), false),
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
            4 + usize::from(self.status.is_some()) + usize::from(error_type.is_some()),
        );
        labels.extend([
            Label::new("http.request.method", self.method),
            Label::new("server.address", self.server.address.clone()),
            Label::new("server.port", self.server.port.clone()),
            Label::new("outbound.outcome", outcome),
        ]);
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

fn http_error_type(status: StatusCode) -> Option<SharedString> {
    if status.is_client_error() || status.is_server_error() || status.as_u16() >= 600 {
        Some(Arc::<str>::from(status.as_str()).into())
    } else {
        None
    }
}

fn error_type(error: &Error) -> &'static str {
    match error {
        Error::InvalidConfiguration => "invalid_configuration",
        Error::InvalidTarget => "invalid_target",
        Error::Timeout => "timeout",
        Error::ResponseBodyTooLarge => "response_body_too_large",
        Error::ClientBuild { .. } | Error::Transport { .. } => "transport",
    }
}
