//! Observation of every request in one middleware, as the gRPC transport's
//! `observe` does it: the OpenTelemetry server span, the HTTP server metrics,
//! and one structured access-log line.
//!
//! The span is the one `axum-tracing-opentelemetry`'s `OtelAxumLayer` opens,
//! built from the same `tracing-opentelemetry-instrumentation-sdk` pieces,
//! except that the route, the operation name, and the request id are fields of
//! the new span instead of `Span::record` calls after it. This runs after
//! routing and after the request id is set, so all three are known, and every
//! record re-serializes all of the span's fields in the JSON log layer. Only
//! the response status is recorded afterwards.
//!
//! The metrics keep the names and labels `axum-prometheus` defined: requests
//! and their duration by method, route template, and status, and in-flight
//! requests by method and route template, held until the response body ends.
//!
//! The access-log line is written inside the span, so it carries the trace
//! and span ids through the subscriber, and outside the shedder and timeout,
//! so a rejected or timed-out request is still recorded with its real status.
//! Matched health probe routes are skipped by route template, not raw path,
//! so an unmatched request that merely looks like a probe is still logged.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{MatchedPath, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use http_body::{Frame, SizeHint};
use metrics::{Label, SharedString};
use opentelemetry::context::FutureExt as _;
use tower_http::request_id::RequestId;
use tracing::Instrument as _;
use tracing::field::Empty;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::{otel_trace_span, set_parent_or_fallback};

use crate::harden::{
    HTTP_REQUESTS_DURATION_SECONDS, HTTP_REQUESTS_PENDING, HTTP_REQUESTS_TOTAL, complete_problem,
};
use crate::problem::Problem;
use crate::router::HEALTH_PROBE_ROUTES;

/// Label for requests the router did not match.
pub(crate) const UNMATCHED_ROUTE: &str = "<unmatched>";

#[derive(Clone, Copy, Debug)]
pub(crate) struct AccessLogOptions {
    pub(crate) log_health_probes: bool,
}

pub(crate) async fn observe(
    State(options): State<AccessLogOptions>,
    matched: Option<MatchedPath>,
    request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    let request_id = request.extensions().get::<RequestId>().cloned();
    let request_id = request_id
        .as_ref()
        .and_then(|id| id.header_value().to_str().ok());
    let span = make_span(&request, matched.as_ref(), request_id);
    let fallback = set_parent_or_fallback(&span, otel_http::extract_context(request.headers()));
    let route = matched
        .as_ref()
        .map_or(UNMATCHED_ROUTE, MatchedPath::as_str);
    let endpoint = SharedString::from(Arc::<str>::from(route));
    let pending = Pending::start(method_label(&method), endpoint.clone());

    let response = match fallback {
        None => next.run(request).instrument(span.clone()).await,
        // The span is disabled: keep the caller's context current instead,
        // so it still reaches the response header and outbound calls.
        Some(context) => {
            next.run(request)
                .instrument(span.clone())
                .with_context(context)
                .await
        }
    };

    // Completed here, before the metrics and the line below read it.
    let response = complete_problem(response, request_id);
    let status = response.status();
    record(method_label(&method), endpoint, status, started.elapsed());
    if !skip_probe(options, &method, route) {
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        // problem_code separates the failures that share a status: a 503 is
        // load shedding, a saturated pool, or a draining instance, and during
        // an incident that distinction is the whole question. Bounded by the
        // catalog.
        let problem_code = response
            .extensions()
            .get::<Problem>()
            .map(|problem| problem.code().as_str());
        span.in_scope(|| {
            tracing::info!(
                method = %method,
                route = %route,
                status = status.as_u16(),
                duration_ms,
                problem_code,
                request_id,
                "http_request"
            );
        });
    }
    otel_http::http_server::update_span_from_response(&span, &response);
    pending.until_body_ends(response)
}

/// `otel_http::http_server::make_span_from_request` with `http.route`,
/// `otel.name`, and `request_id` filled in.
fn make_span(
    request: &Request,
    matched: Option<&MatchedPath>,
    request_id: Option<&str>,
) -> tracing::Span {
    let method = request.method();
    let route = matched.map_or("", MatchedPath::as_str);
    let (server_address, server_port) = otel_http::http_host_port(request);
    otel_trace_span!(
        "HTTP request",
        http.request.method = %method,
        http.route = route,
        network.protocol.version = %otel_http::http_flavor(request.version()),
        server.address = server_address,
        server.port = server_port,
        http.client.address = Empty,
        user_agent.original = otel_http::user_agent(request),
        http.response.status_code = Empty,
        url.path = request.uri().path(),
        url.query = request.uri().query(),
        url.scheme = otel_http::url_scheme(request.uri()),
        otel.name = format!("{method} {route}").trim(),
        otel.kind = ?opentelemetry::trace::SpanKind::Server,
        otel.status_code = Empty,
        trace_id = Empty,
        request_id,
        exception.message = Empty,
        // Datadog's span category, as the SDK's `SpanType::Web` renders it.
        "span.type" = "web",
    )
}

/// The in-flight gauge of one request, held until its response body ends or
/// is dropped.
struct Pending(metrics::Gauge);

impl Pending {
    fn start(method: &'static str, endpoint: SharedString) -> Self {
        let gauge = metrics::gauge!(
            HTTP_REQUESTS_PENDING,
            vec![
                Label::new("method", method),
                Label::new("endpoint", endpoint)
            ]
        );
        gauge.increment(1);
        Self(gauge)
    }

    fn until_body_ends(self, response: Response) -> Response {
        response.map(|body| {
            Body::new(PendingBody {
                inner: body,
                _pending: self,
            })
        })
    }
}

/// A response body that holds its request's [`Pending`] gauge and otherwise
/// passes everything through, the size hint included, so a known length
/// still becomes `Content-Length`.
struct PendingBody {
    inner: Body,
    _pending: Pending,
}

impl http_body::Body for PendingBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        http_body::Body::poll_frame(Pin::new(&mut self.inner), cx)
    }

    fn is_end_stream(&self) -> bool {
        http_body::Body::is_end_stream(&self.inner)
    }

    fn size_hint(&self) -> SizeHint {
        http_body::Body::size_hint(&self.inner)
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.0.decrement(1);
    }
}

fn record(method: &'static str, endpoint: SharedString, status: StatusCode, elapsed: Duration) {
    let labels = vec![
        Label::new("method", method),
        Label::new("status", Arc::<str>::from(status.as_str())),
        Label::new("endpoint", endpoint),
    ];
    metrics::counter!(HTTP_REQUESTS_TOTAL, labels.clone()).increment(1);
    metrics::histogram!(HTTP_REQUESTS_DURATION_SECONDS, labels).record(elapsed.as_secs_f64());
}

/// The standard method name, or empty for an extension method, so a caller
/// cannot create label values.
const fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::OPTIONS => "OPTIONS",
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::HEAD => "HEAD",
        Method::TRACE => "TRACE",
        Method::CONNECT => "CONNECT",
        Method::PATCH => "PATCH",
        _ => "",
    }
}

fn skip_probe(options: AccessLogOptions, method: &Method, route: &str) -> bool {
    !options.log_health_probes && method == Method::GET && HEALTH_PROBE_ROUTES.contains(&route)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_are_skipped_by_route_template_only() {
        let quiet = AccessLogOptions {
            log_health_probes: false,
        };
        assert!(skip_probe(quiet, &Method::GET, "/health/live"));
        assert!(skip_probe(quiet, &Method::GET, "/health/ready"));
        assert!(!skip_probe(quiet, &Method::POST, "/health/live"));
        assert!(!skip_probe(quiet, &Method::GET, UNMATCHED_ROUTE));
        let verbose = AccessLogOptions {
            log_health_probes: true,
        };
        assert!(!skip_probe(verbose, &Method::GET, "/health/live"));
    }
}
