//! Observation of every request in one middleware, as the gRPC transport's
//! `observe` does it: the OpenTelemetry server span, the HTTP server metrics,
//! and one structured access-log line.
//!
//! The span carries the attributes `axum-tracing-opentelemetry`'s
//! `OtelAxumLayer` sets, from the same `tracing-opentelemetry-instrumentation-sdk`
//! pieces. Only the operation name, the kind, and the request id are `tracing`
//! fields, because the JSON log layer serializes every span field and repeats
//! it on each record inside the request; the HTTP attributes go to the
//! OpenTelemetry span alone. This runs after routing and after the request id
//! is set, so all of them are known at creation, and nothing is `Span::record`ed
//! afterwards, since every record re-serializes the span's fields.
//!
//! The metrics follow the OpenTelemetry HTTP semantic conventions as
//! Prometheus renders them: the response-head duration by method, route
//! template, and status, whose `_count` series is the request count, and the
//! active requests by method, held until the response body is dropped
//! (including after consumption or cancellation).
//!
//! The access-log line is written inside the span, so it carries the trace
//! and span ids through the subscriber, and outside the shedder and timeout,
//! so a rejected or timed-out request is still recorded with its real status.
//! Matched health probe routes are skipped by route template, not raw path,
//! so an unmatched request that merely looks like a probe is still logged.

use std::borrow::Cow;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{MatchedPath, Request, State};
use axum::http::{Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::Response;
use http_body::{Frame, SizeHint};
use metrics::{Label, SharedString};
use opentelemetry::context::FutureExt as _;
use tower_http::request_id::RequestId;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::{otel_trace_span, set_parent_or_fallback};

use crate::harden::{HTTP_ACTIVE_REQUESTS, HTTP_REQUESTS_DURATION_SECONDS, complete_problem};
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
    // Own the accepted header while the request moves into the next service;
    // the borrowed string also remains available for response completion.
    let request_id = request.extensions().get::<RequestId>().cloned();
    let request_id = request_id
        .as_ref()
        .and_then(|id| id.header_value().to_str().ok());
    let span = make_span(&request, matched.as_ref(), request_id);
    let fallback = set_parent_or_fallback(&span, otel_http::extract_context(request.headers()));
    let route = matched
        .as_ref()
        .map_or(UNMATCHED_ROUTE, MatchedPath::as_str);
    let active = Active::start(method_label(&method));

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

    // Complete the Problem before metrics and logging read the response head.
    // These observations do not wait for a streaming body; only the active
    // gauge below follows the body's lifetime.
    let response = complete_problem(response, request_id);
    let status = response.status();
    record(method_label(&method), route, status, started.elapsed());
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
    update_span_from_response(&span, status);
    active.until_body_ends(response)
}

/// The server span. Only what every log record inside the request should
/// carry is a `tracing` field; the other OpenTelemetry HTTP attributes go to
/// the span alone, so the JSON log layer neither serializes nor repeats them.
fn make_span(
    request: &Request,
    matched: Option<&MatchedPath>,
    request_id: Option<&str>,
) -> tracing::Span {
    let method = request.method();
    let route = matched.map(MatchedPath::as_str);
    let span = otel_trace_span!(
        "HTTP request",
        otel.name = format!("{method} {}", route.unwrap_or_default()).trim(),
        otel.kind = ?opentelemetry::trace::SpanKind::Server,
        request_id,
    );
    let (server_address, server_port) = otel_http::http_host_port(request);
    span.set_attribute("http.request.method", method.as_str().to_owned());
    // The conventions leave `http.route` and `user_agent.original` off a span
    // that has none; an empty string would read as a value.
    if let Some(route) = route {
        span.set_attribute("http.route", route.to_owned());
    }
    span.set_attribute(
        "network.protocol.version",
        otel_http::http_flavor(request.version()).into_owned(),
    );
    span.set_attribute("server.address", server_address.to_owned());
    if let Some(port) = server_port {
        span.set_attribute("server.port", port);
    }
    let user_agent = otel_http::user_agent(request);
    if !user_agent.is_empty() {
        span.set_attribute("user_agent.original", user_agent.to_owned());
    }
    span.set_attribute("url.path", request.uri().path().to_owned());
    if let Some(query) = request.uri().query() {
        span.set_attribute("url.query", redact_query(query).into_owned());
    }
    span.set_attribute("url.scheme", url_scheme(request.uri()));
    span.set_attribute("span.type", "web");
    span
}

/// The scheme of the request as this server received it. An HTTP/2 request
/// carries it in `:scheme`; an HTTP/1 request line has none, and the listener
/// the hardened chain is bound to is plaintext (TLS ends at the platform edge),
/// so that request is `http`.
fn url_scheme(uri: &Uri) -> &'static str {
    match uri.scheme_str() {
        Some("https") => "https",
        _ => "http",
    }
}

fn update_span_from_response(span: &tracing::Span, status: StatusCode) {
    span.set_attribute("http.response.status_code", i64::from(status.as_u16()));
    if status.is_server_error() {
        // The conventions name a failed response by its status code.
        span.set_attribute("error.type", status.as_str().to_owned());
        span.set_status(opentelemetry::trace::Status::error(""));
    }
}

/// Query parameters whose values the OpenTelemetry HTTP conventions redact by
/// default: they carry request signatures and access key ids.
const REDACTED_QUERY_KEYS: [&str; 4] = ["AWSAccessKeyId", "Signature", "sig", "X-Goog-Signature"];

/// The query string for the span's `url.query`, with the values of
/// [`REDACTED_QUERY_KEYS`] replaced by `REDACTED`.
fn redact_query(query: &str) -> Cow<'_, str> {
    let sensitive = |pair: &str| {
        let key = pair.split_once('=').map_or(pair, |(key, _)| key);
        REDACTED_QUERY_KEYS.contains(&key)
    };
    if !query.split('&').any(sensitive) {
        return Cow::Borrowed(query);
    }
    let mut redacted = String::with_capacity(query.len());
    for (index, pair) in query.split('&').enumerate() {
        if index > 0 {
            redacted.push('&');
        }
        match pair.split_once('=') {
            Some((key, _)) if sensitive(pair) => {
                redacted.push_str(key);
                redacted.push_str("=REDACTED");
            }
            _ => redacted.push_str(pair),
        }
    }
    Cow::Owned(redacted)
}

/// The active-requests gauge of one request, held until its response body is
/// dropped.
struct Active(metrics::Gauge);

impl Active {
    fn start(method: &'static str) -> Self {
        let gauge = metrics::gauge!(HTTP_ACTIVE_REQUESTS, "http_request_method" => method);
        gauge.increment(1);
        Self(gauge)
    }

    fn until_body_ends(self, response: Response) -> Response {
        response.map(|body| {
            Body::new(ActiveBody {
                inner: body,
                _active: self,
            })
        })
    }
}

/// A response body that holds its request's [`Active`] gauge and otherwise
/// passes everything through, the size hint included, so a known length
/// still becomes `Content-Length`.
struct ActiveBody {
    inner: Body,
    _active: Active,
}

impl http_body::Body for ActiveBody {
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

impl Drop for Active {
    fn drop(&mut self) {
        self.0.decrement(1);
    }
}

fn record(method: &'static str, route: &str, status: StatusCode, elapsed: Duration) {
    let labels = vec![
        Label::new("http_request_method", method),
        Label::new(
            "http_response_status_code",
            Arc::<str>::from(status.as_str()),
        ),
        Label::new("http_route", SharedString::from(Arc::<str>::from(route))),
    ];
    metrics::histogram!(HTTP_REQUESTS_DURATION_SECONDS, labels).record(elapsed.as_secs_f64());
}

/// The standard method name, or the conventions' `_OTHER` for an extension
/// method, so a caller cannot create label values.
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
        _ => "_OTHER",
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

    #[test]
    fn signature_query_values_are_redacted_and_the_rest_kept() {
        for (query, expected) in [
            ("", ""),
            ("full=1&page=2", "full=1&page=2"),
            ("sig=abc", "sig=REDACTED"),
            ("a=1&Signature=abc%3D&b", "a=1&Signature=REDACTED&b"),
            (
                "AWSAccessKeyId=AKIA&X-Goog-Signature=ff&x=1",
                "AWSAccessKeyId=REDACTED&X-Goog-Signature=REDACTED&x=1",
            ),
            // Keys are matched exactly, as the conventions list them.
            ("signature=abc&sig", "signature=abc&sig"),
        ] {
            assert_eq!(redact_query(query), expected, "{query}");
        }
    }

    /// The one server span a request through the hardened chain exports.
    #[allow(
        clippy::disallowed_methods,
        reason = "transport fixture exercises the middleware independently of contract finalization"
    )]
    async fn exported_span(request: Request) -> opentelemetry_sdk::trace::SpanData {
        use axum::Router;
        use axum::routing::get;
        use opentelemetry::trace::TracerProvider as _;
        use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
        use tower::ServiceExt as _;
        use tracing_subscriber::layer::SubscriberExt as _;

        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        let _guard = tracing::subscriber::set_default(subscriber);
        let app = crate::harden(
            Router::new().route(
                "/items/{id}",
                get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
            ),
            &crate::HardenOptions {
                max_body_bytes: 64,
                request_timeout: Duration::from_secs(5),
                max_in_flight: std::num::NonZeroU32::new(2),
                log_health_probes: false,
            },
        );
        drop(app.oneshot(request).await.unwrap());

        let mut spans = exporter.get_finished_spans().unwrap();
        assert_eq!(spans.len(), 1, "one server span, got {spans:?}");
        spans.remove(0)
    }

    fn attribute(
        span: &opentelemetry_sdk::trace::SpanData,
        key: &str,
    ) -> Option<opentelemetry::Value> {
        span.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.clone())
    }

    #[tokio::test]
    async fn exported_server_span_keeps_the_http_attributes_and_the_error_status() {
        use opentelemetry::trace::Status;
        use opentelemetry::{KeyValue, Value};

        let request = Request::builder()
            .uri("/items/7?full=1")
            .header("host", "api.test:8443")
            .header("user-agent", "probe/1")
            .body(Body::empty())
            .unwrap();
        let span = exported_span(request).await;

        assert_eq!(span.name, "GET /items/{id}");
        assert_eq!(span.span_kind, opentelemetry::trace::SpanKind::Server);
        assert!(matches!(span.status, Status::Error { .. }));
        for KeyValue { key, value, .. } in [
            KeyValue::new("http.request.method", "GET"),
            KeyValue::new("http.route", "/items/{id}"),
            KeyValue::new("http.response.status_code", 500),
            KeyValue::new("network.protocol.version", "1.1"),
            KeyValue::new("server.address", "api.test"),
            KeyValue::new("server.port", 8443),
            KeyValue::new("url.path", "/items/7"),
            KeyValue::new("url.query", "full=1"),
            KeyValue::new("url.scheme", "http"),
            KeyValue::new("error.type", "500"),
            KeyValue::new("user_agent.original", "probe/1"),
            KeyValue::new("span.type", "web"),
        ] {
            assert_eq!(attribute(&span, key.as_str()), Some(value), "{key}");
        }
        assert!(matches!(
            attribute(&span, "request_id"),
            Some(Value::String(_))
        ));
    }

    #[tokio::test]
    async fn an_unmatched_request_span_omits_what_the_request_does_not_have() {
        use opentelemetry::trace::Status;

        let request = Request::builder()
            .uri("https://api.test/missing")
            .body(Body::empty())
            .unwrap();
        let span = exported_span(request).await;

        // No route template and a client error: the method alone names the
        // span, and a 404 is not a server failure.
        assert_eq!(span.name, "GET");
        assert!(matches!(span.status, Status::Unset));
        for absent in ["http.route", "user_agent.original", "error.type"] {
            assert_eq!(attribute(&span, absent), None, "{absent}");
        }
        assert_eq!(
            attribute(&span, "http.response.status_code"),
            Some(404.into())
        );
        // An absolute-form target (HTTP/2 `:scheme`) names its own scheme.
        assert_eq!(attribute(&span, "url.scheme"), Some("https".into()));
    }
}
