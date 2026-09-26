use std::time::Instant;

use http::{Method, StatusCode};
use metrics::{Label, Unit};
use tracing::Span;

use crate::Error;

const REQUEST_DURATION_METRIC: &str = "http.client.request.duration";

/// One polled outbound attempt. The guard retains only bounded configured
/// identity and outcome state, never caller request data or transport errors.
pub(crate) struct Attempt {
    started: Instant,
    method: &'static str,
    server_address: String,
    server_port: String,
    status: Option<StatusCode>,
    span: Span,
    finalized: bool,
}

impl Attempt {
    pub(crate) fn start(method: &Method, base: &url::Url) -> Self {
        let method = bounded_method(method);
        let server_address = base.host_str().unwrap_or_default().to_owned();
        let server_port = base.port_or_known_default().unwrap_or_default().to_string();
        let span = tracing::info_span!(
            "outbound_http",
            otel.kind = "client",
            http.request.method = method,
            server.address = %server_address,
            server.port = %server_port,
            http.response.status_code = tracing::field::Empty,
            error.type = tracing::field::Empty,
            outbound.outcome = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        Self {
            started: Instant::now(),
            method,
            server_address,
            server_port,
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
        self.span.record(
            "http.response.status_code",
            tracing::field::display(status.as_u16()),
        );
    }

    pub(crate) fn finish<T>(&mut self, result: &Result<T, Error>) {
        if self.finalized {
            return;
        }
        self.finalized = true;
        match result {
            Ok(_) => {
                let error_type = self.status.and_then(http_error_type);
                self.emit("response", error_type.as_deref());
            }
            Err(error) => self.emit("error", Some(error_type(error))),
        }
    }

    fn emit(&self, outcome: &'static str, error_type: Option<&str>) {
        self.span.record("outbound.outcome", outcome);
        if let Some(error_type) = error_type {
            self.span.record("error.type", error_type);
            self.span.record("otel.status_code", "ERROR");
        }

        metrics::describe_histogram!(
            REQUEST_DURATION_METRIC,
            Unit::Seconds,
            "Outbound HTTP attempt duration in seconds"
        );
        let mut labels = vec![
            Label::new("http.request.method", self.method),
            Label::new("server.address", self.server_address.clone()),
            Label::new("server.port", self.server_port.clone()),
            Label::new("outbound.outcome", outcome),
        ];
        if let Some(status) = self.status {
            labels.push(Label::new(
                "http.response.status_code",
                status.as_u16().to_string(),
            ));
        }
        if let Some(error_type) = error_type {
            labels.push(Label::new("error.type", error_type.to_owned()));
        }
        metrics::histogram!(REQUEST_DURATION_METRIC, labels)
            .record(self.started.elapsed().as_secs_f64());
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.finalized {
            self.emit("cancelled", None);
        }
    }
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

fn http_error_type(status: StatusCode) -> Option<String> {
    if status.is_client_error() || status.is_server_error() || status.as_u16() >= 600 {
        Some(status.as_u16().to_string())
    } else {
        None
    }
}

fn error_type(error: &Error) -> &'static str {
    match error {
        Error::InvalidConfiguration => "invalid_configuration",
        Error::InvalidTarget => "invalid_target",
        Error::AtCapacity => "at_capacity",
        Error::Timeout { .. } => "timeout",
        Error::ResponseBodyTooLarge => "response_body_too_large",
        Error::ClientBuild { .. } | Error::Transport { .. } => "transport",
    }
}
