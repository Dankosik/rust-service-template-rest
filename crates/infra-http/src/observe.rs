//! Observation of every request in one middleware, as the gRPC transport's
//! `observe` does it: the OpenTelemetry server span, the HTTP server metrics,
//! and one structured access-log line.
//!
//! The span admits finite HTTP diagnostics and validated correlation using
//! `tracing-opentelemetry-instrumentation-sdk` pieces. Raw URI, caller authority
//! and User-Agent are never recorded. Only the operation name, the kind, and
//! the request id are `tracing` fields, because the JSON log layer serializes
//! every span field and repeats
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
                method = method_label(&method),
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
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn make_span(
    request: &Request,
    matched: Option<&MatchedPath>,
    request_id: Option<&str>,
) -> tracing::Span {
    let method = method_label(request.method());
    let route = matched.map(MatchedPath::as_str);
    let span = otel_trace_span!(
        "HTTP request",
        otel.name = format!("{method} {}", route.unwrap_or_default()).trim(),
        otel.kind = ?opentelemetry::trace::SpanKind::Server,
        request_id,
    );
    span.set_attribute("http.request.method", method);
    // An unmatched request has no route template; an empty string would read
    // as a value.
    if let Some(route) = route {
        span.set_attribute("http.route", route.to_owned());
    }
    span.set_attribute(
        "network.protocol.version",
        otel_http::http_flavor(request.version()),
    );
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

#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn update_span_from_response(span: &tracing::Span, status: StatusCode) {
    span.set_attribute("http.response.status_code", i64::from(status.as_u16()));
    if status.is_server_error() {
        // The conventions name a failed response by its status code.
        span.set_attribute("error.type", status.as_str().to_owned());
        span.set_status(opentelemetry::trace::Status::error(""));
    }
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

#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn record(method: &'static str, route: &str, status: StatusCode, elapsed: Duration) {
    let labels = vec![
        Label::new("http_request_method", method),
        Label::new("http_response_status_code", status_label(status)),
        Label::new("http_route", SharedString::from(Arc::<str>::from(route))),
    ];
    metrics::histogram!(HTTP_REQUESTS_DURATION_SECONDS, labels).record(elapsed.as_secs_f64());
}

// Keep typed values at static addresses so their numeric text can be borrowed
// for every supported status, including extension codes.
#[allow(clippy::panic, reason = "the literal range is valid at compile time")]
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

    #[derive(Default)]
    struct Histograms(std::sync::Mutex<Vec<metrics::Key>>);

    impl metrics::Recorder for Histograms {
        fn describe_counter(&self, _: metrics::KeyName, _: Option<metrics::Unit>, _: SharedString) {
        }

        fn describe_gauge(&self, _: metrics::KeyName, _: Option<metrics::Unit>, _: SharedString) {}

        fn describe_histogram(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: SharedString,
        ) {
        }

        fn register_counter(
            &self,
            _: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Counter {
            metrics::Counter::noop()
        }

        fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
            metrics::Gauge::noop()
        }

        fn register_histogram(
            &self,
            key: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Histogram {
            self.0.lock().unwrap().push(key.clone());
            metrics::Histogram::noop()
        }
    }

    #[test]
    fn duration_metric_keeps_every_numeric_status_and_its_other_labels() {
        let histograms = Histograms::default();
        metrics::with_local_recorder(&histograms, || {
            for code in 100..=999 {
                record(
                    "_OTHER",
                    UNMATCHED_ROUTE,
                    StatusCode::from_u16(code).unwrap(),
                    Duration::from_millis(1),
                );
            }
        });
        let keys = histograms.0.lock().unwrap();
        assert_eq!(keys.len(), 900);
        for (code, key) in (100..=999).zip(keys.iter()) {
            assert_eq!(key.name(), HTTP_REQUESTS_DURATION_SECONDS);
            let labels: Vec<_> = key
                .labels()
                .map(|label| (label.key(), label.value()))
                .collect();
            let numeric = code.to_string();
            assert_eq!(
                labels,
                [
                    ("http_request_method", "_OTHER"),
                    ("http_response_status_code", numeric.as_str()),
                    ("http_route", "<unmatched>"),
                ]
            );
        }
    }

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

    /// The one server span a request through the hardened chain exports.
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
        let handler_1 = || async { StatusCode::INTERNAL_SERVER_ERROR };
        #[allow(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let app = crate::harden(
            Router::new().route("/items/{id}", get(handler_1)),
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

    fn assert_private_values_absent(span: &opentelemetry_sdk::trace::SpanData) {
        for absent in [
            "server.address",
            "server.port",
            "url.path",
            "url.query",
            "user_agent.original",
        ] {
            assert_eq!(attribute(span, absent), None, "{absent}");
        }
        let exported = format!("{span:?}");
        for private in [
            "private-path-value",
            "private-query-value",
            "encoded-secret",
            "signature-secret",
            "private-host-value",
            "private-authority-value",
            "private-agent-value",
        ] {
            assert!(!exported.contains(private), "{private}: {exported}");
        }
    }

    fn assert_access_event(
        span: &opentelemetry_sdk::trace::SpanData,
        method: &str,
        route: &str,
        status: i64,
    ) {
        let access = span
            .events
            .iter()
            .find(|event| event.name == "http_request")
            .expect("the real access event reaches the subscriber/exporter");
        for expected in [
            opentelemetry::KeyValue::new("method", method.to_owned()),
            opentelemetry::KeyValue::new("route", route.to_owned()),
            // The OTel event visitor represents this unsigned tracing field as text.
            opentelemetry::KeyValue::new("status", status.to_string()),
        ] {
            assert!(access.attributes.contains(&expected), "{access:?}");
        }
    }

    #[tokio::test]
    async fn exported_server_span_keeps_only_admitted_http_diagnostics() {
        use opentelemetry::KeyValue;
        use opentelemetry::trace::Status;

        let request = Request::builder()
            .uri("/items/private-path-value?arbitrary=private-query-value&%73ig=encoded-secret&Signature=signature-secret")
            .header("host", "private-host-value.test:8443")
            .header("user-agent", "private-agent-value")
            .header("x-request-id", "admitted-correlation")
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
            KeyValue::new("url.scheme", "http"),
            KeyValue::new("error.type", "500"),
            KeyValue::new("span.type", "web"),
        ] {
            assert_eq!(attribute(&span, key.as_str()), Some(value), "{key}");
        }
        assert_eq!(
            attribute(&span, "request_id"),
            Some("admitted-correlation".into())
        );
        assert_private_values_absent(&span);
        assert_access_event(&span, "GET", "/items/{id}", 500);
    }

    #[tokio::test]
    async fn unmatched_request_withholds_raw_authority_path_and_query() {
        use opentelemetry::trace::Status;

        let request = Request::builder()
            .uri("https://private-authority-value.test:9443/private-path-value?arbitrary=private-query-value")
            .header("host", "private-host-value.test:8443")
            .header("user-agent", "private-agent-value")
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
        assert_private_values_absent(&span);
        assert_access_event(&span, "GET", "<unmatched>", 404);
    }

    #[tokio::test]
    async fn spans_and_access_events_normalize_methods_and_preserve_protocols() {
        use axum::http::Version;

        for (method, normalized, version, protocol) in [
            ("OPTIONS", "OPTIONS", Version::HTTP_09, "0.9"),
            ("GET", "GET", Version::HTTP_10, "1.0"),
            ("POST", "POST", Version::HTTP_11, "1.1"),
            ("PUT", "PUT", Version::HTTP_2, "2.0"),
            ("DELETE", "DELETE", Version::HTTP_3, "3.0"),
            ("HEAD", "HEAD", Version::HTTP_11, "1.1"),
            ("TRACE", "TRACE", Version::HTTP_11, "1.1"),
            ("CONNECT", "CONNECT", Version::HTTP_11, "1.1"),
            ("PATCH", "PATCH", Version::HTTP_11, "1.1"),
            ("PURGE", "_OTHER", Version::HTTP_11, "1.1"),
            ("x-Custom_Method", "_OTHER", Version::HTTP_2, "2.0"),
            ("_OTHER", "_OTHER", Version::HTTP_11, "1.1"),
        ] {
            let request = Request::builder()
                .method(method)
                .version(version)
                .uri("/missing")
                .body(Body::empty())
                .unwrap();
            let span = exported_span(request).await;
            assert_eq!(span.name, normalized);
            assert_eq!(
                attribute(&span, "http.request.method"),
                Some(normalized.into())
            );
            assert_access_event(&span, normalized, "<unmatched>", 404);
            if method != normalized {
                assert!(!format!("{span:?}").contains(method));
            }
            assert_eq!(
                attribute(&span, "network.protocol.version"),
                Some(protocol.into())
            );
            assert_eq!(attribute(&span, "http.route"), None);
        }
    }
}
