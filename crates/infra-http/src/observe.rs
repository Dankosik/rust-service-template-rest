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
//! (including after consumption or cancellation). A drop before the response
//! head is recorded as 499 for client abandonment or 503 for a forced server
//! shutdown. Neither status was sent to the client.
//!
//! The access-log line is written inside the span, so it carries the trace
//! and span ids through the subscriber, and outside admission and the opening
//! deadline, so a shed or timed-out request is still recorded with its real
//! status.
//! Matched health probe routes are skipped by route template, not raw path,
//! so an unmatched request that merely looks like a probe is still logged.

use std::pin::Pin;
use std::sync::{Arc, PoisonError, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{MatchedPath, Request, State};
use axum::http::header::{CONTENT_ENCODING, CONTENT_LENGTH, TRAILER, TRANSFER_ENCODING};
use axum::http::{Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use foldhash::HashMap;
use http_body::{Frame, SizeHint};
use metrics::{Gauge, Histogram, Label, SharedString};
use opentelemetry::context::FutureExt as _;
use shared_rules::otel_http::request_method;
use tower_http::request_id::RequestId;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::{otel_trace_span, set_parent_or_fallback};

use crate::probes::is_health_probe_route;
use crate::problem::{Problem, body_too_large};
use crate::server::ConnectionInfo;

/// The response-head duration, labelled by `http_request_method`,
/// `http_route` (the matched route template or `<unmatched>`), and
/// `http_response_status_code`; its `_count` series is the request count.
pub(crate) const REQUEST_DURATION_METRIC: &str = "http_server_request_duration_seconds";

/// Requests whose response body has not ended, labelled by
/// `http_request_method`.
pub(crate) const ACTIVE_REQUESTS_METRIC: &str = "http_server_active_requests";

/// Histogram buckets for request latencies, in seconds.
pub(crate) const LATENCY_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Time a request waited for an in-flight permit, a histogram without labels.
/// It is recorded only for a request that had to wait, whether the wait ended
/// in a permit or in a shed.
pub(crate) const ADMISSION_WAIT_METRIC: &str = "http_server_admission_wait_seconds";

/// Requests shed because no in-flight permit freed within the request timeout.
pub(crate) const SHED_REQUESTS_METRIC: &str = "http_server_shed_requests_total";

/// Every histogram this crate records. A composition root hands it to the
/// Prometheus recorder; a metric left out renders the default buckets.
pub const HISTOGRAMS: &[(&str, &[f64])] = &[
    (REQUEST_DURATION_METRIC, LATENCY_BUCKETS),
    (ADMISSION_WAIT_METRIC, LATENCY_BUCKETS),
];

pub(crate) fn describe() {
    metrics::describe_gauge!(
        ACTIVE_REQUESTS_METRIC,
        metrics::Unit::Count,
        "Number of active HTTP server requests."
    );
    metrics::describe_histogram!(
        REQUEST_DURATION_METRIC,
        metrics::Unit::Seconds,
        "Duration of HTTP server requests."
    );
    metrics::describe_histogram!(
        ADMISSION_WAIT_METRIC,
        metrics::Unit::Seconds,
        "Time requests waited for an in-flight permit, recorded only for a request that had to wait."
    );
    metrics::describe_counter!(
        SHED_REQUESTS_METRIC,
        metrics::Unit::Count,
        "Requests rejected without running a handler because no in-flight permit freed within the request timeout."
    );
}

/// Label for requests the router did not match.
pub(crate) const UNMATCHED_ROUTE: &str = "<unmatched>";

/// The status recorded for a request its caller abandoned before the
/// response head; no response was sent.
const CLIENT_CLOSED_REQUEST: StatusCode = match StatusCode::from_u16(499) {
    Ok(status) => status,
    Err(_) => panic!("499 is a valid status code"),
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct AccessLogOptions {
    pub(crate) log_health_probes: bool,
}

/// The middleware state: what the access log is told and the metric handles
/// shared by every request through one [`harden`](crate::harden())ed router.
#[derive(Clone, Debug)]
pub(crate) struct Observer {
    options: AccessLogOptions,
    series: Arc<Series>,
}

impl Observer {
    pub(crate) fn new(options: AccessLogOptions) -> Self {
        Self {
            options,
            series: Arc::default(),
        }
    }
}

pub(crate) async fn observe(
    State(Observer { options, series }): State<Observer>,
    matched: Option<MatchedPath>,
    mut request: Request,
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
    let connection = request.extensions_mut().remove::<ConnectionInfo>();
    let span = make_span(&request, matched.as_ref(), request_id, connection.as_ref());
    let fallback = set_parent_or_fallback(&span, otel_http::extract_context(request.headers()));
    let route = matched
        .as_ref()
        .map_or(UNMATCHED_ROUTE, MatchedPath::as_str);
    let active = Active::start(&series, request_method(method.as_str()));
    let mut pending = Pending {
        options,
        series: &series,
        method: &method,
        route,
        request_id,
        span: &span,
        started,
        answered: false,
        connection,
    };

    let traced = next.run(request).instrument(span.clone());
    let response = match fallback {
        None => traced.await,
        // The span is disabled: keep the caller's context current instead,
        // so it still reaches the response header and outbound calls.
        Some(context) => traced.with_context(context).await,
    };

    // Complete the Problem before metrics and logging read the response head.
    // These observations do not wait for a streaming body; only the active
    // gauge below follows the body's lifetime.
    let response = complete_problem(response, request_id);
    let status = response.status();
    // problem_code separates the failures that share a status: a 503 is load
    // shedding, a saturated pool, or a draining instance, and during an
    // incident that distinction is the whole question. Bounded by the
    // catalog.
    let problem_code = response
        .extensions()
        .get::<Problem>()
        .map(|problem| problem.code().as_str());
    pending.answer(status, problem_code);
    update_span_from_response(&span, status);
    active.until_body_ends(response)
}

struct Pending<'a> {
    options: AccessLogOptions,
    series: &'a Series,
    method: &'a Method,
    route: &'a str,
    request_id: Option<&'a str>,
    span: &'a tracing::Span,
    started: Instant,
    answered: bool,
    connection: Option<ConnectionInfo>,
}

impl Pending<'_> {
    fn answer(&mut self, status: StatusCode, problem_code: Option<&str>) {
        self.answered = true;
        self.record(status, problem_code, None);
    }

    fn record(&self, status: StatusCode, problem_code: Option<&str>, termination: Option<&str>) {
        let elapsed = self.started.elapsed();
        self.series.record_duration(
            request_method(self.method.as_str()),
            self.route,
            status,
            elapsed,
        );
        if suppresses_access_log(self.options, self.method, self.route) {
            return;
        }
        let duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        self.span.in_scope(|| {
            tracing::info!(
                method = request_method(self.method.as_str()),
                route = %self.route,
                status = status.as_u16(),
                duration_ms,
                problem_code,
                termination,
                request_id = self.request_id,
                "http_request"
            );
        });
    }
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if self.answered {
            return;
        }
        if self
            .connection
            .as_ref()
            .is_some_and(ConnectionInfo::was_forced)
        {
            self.record(
                StatusCode::SERVICE_UNAVAILABLE,
                None,
                Some("server_shutdown"),
            );
        } else {
            self.record(CLIENT_CLOSED_REQUEST, None, None);
        }
    }
}

/// The server span. Only what every log record inside the request should
/// carry is a `tracing` field; the other OpenTelemetry HTTP attributes go to
/// the span alone, so the JSON log layer neither serializes nor repeats them.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn make_span(
    request: &Request,
    matched: Option<&MatchedPath>,
    request_id: Option<&str>,
    connection: Option<&ConnectionInfo>,
) -> tracing::Span {
    let method = request_method(request.method().as_str());
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
    span.set_attribute("url.scheme", url_scheme(request.uri(), connection));
    span.set_attribute("span.type", "web");
    span
}

/// HTTP/1 origin-form targets carry no scheme. Use the listener's scheme
/// when the request target omits it.
fn url_scheme(uri: &Uri, connection: Option<&ConnectionInfo>) -> &'static str {
    match uri.scheme_str() {
        Some("https") => "https",
        None => connection.map_or("http", |connection| connection.scheme),
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

struct Active(Gauge);

impl Active {
    fn start(series: &Series, method: &'static str) -> Self {
        let gauge = series.active_gauge(method);
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

/// One route's histograms by normalized method and status code.
type RouteDurations = HashMap<(&'static str, u16), Histogram>;

/// Metric handles by series, resolved on first use so a request neither
/// rebuilds label strings nor looks its series up in the recorder. The keys
/// are the router's route templates, [`UNMATCHED_ROUTE`], the normalized
/// methods and the status codes the service answers with, so the table is
/// bounded by the router and no caller-chosen value adds an entry.
///
/// A handle resolved while no recorder is installed stays a no-op; the
/// composition root installs the recorder before the router serves.
#[derive(Debug, Default)]
struct Series {
    active: RwLock<HashMap<&'static str, Gauge>>,
    durations: RwLock<HashMap<Box<str>, RouteDurations>>,
}

impl Series {
    fn active_gauge(&self, method: &'static str) -> Gauge {
        let known = self
            .active
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(method)
            .cloned();
        known.unwrap_or_else(|| {
            self.active
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(method)
                .or_insert_with(
                    || metrics::gauge!(ACTIVE_REQUESTS_METRIC, "http_request_method" => method),
                )
                .clone()
        })
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn record_duration(
        &self,
        method: &'static str,
        route: &str,
        status: StatusCode,
        elapsed: Duration,
    ) {
        let status = status.as_u16();
        let seconds = elapsed.as_secs_f64();
        if let Some(histogram) = self
            .durations
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(route)
            .and_then(|statuses| statuses.get(&(method, status)))
        {
            histogram.record(seconds);
            return;
        }
        self.durations
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(route.into())
            .or_default()
            .entry((method, status))
            .or_insert_with(|| duration_histogram(method, route, status))
            .record(seconds);
    }
}

fn duration_histogram(method: &'static str, route: &str, status: u16) -> Histogram {
    let labels = vec![
        Label::new("http_request_method", method),
        Label::new("http_response_status_code", status_label(status)),
        Label::new("http_route", SharedString::from(Arc::<str>::from(route))),
    ];
    metrics::histogram!(REQUEST_DURATION_METRIC, labels)
}

// Keep typed values at static addresses so their numeric text can be borrowed
// for every supported status, including extension codes.
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

fn status_label(status: u16) -> &'static str {
    STATUS_CODES[usize::from(status - 100)].as_str()
}

/// Complete every Problem the chain or a handler returns: a bare 413 from
/// tower-http's body limit or an axum extractor takes the Problem envelope,
/// and every Problem body gains the request id.
fn complete_problem(mut response: Response, request_id: Option<&str>) -> Response {
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE
        && response.extensions().get::<Problem>().is_none()
    {
        let mut problem = body_too_large();
        if let Some(id) = request_id {
            problem = problem.with_request_id(id.to_owned());
        }
        let (parts, body) = problem.into_response().into_parts();
        let headers = response.headers_mut();
        for name in [CONTENT_ENCODING, CONTENT_LENGTH, TRAILER, TRANSFER_ENCODING] {
            headers.remove(name);
        }
        for name in ["content-digest", "repr-digest", "digest", "content-md5"] {
            headers.remove(name);
        }
        headers.extend(parts.headers);
        response.extensions_mut().extend(parts.extensions);
        *response.body_mut() = body;
        return response;
    }
    let Some(id) = request_id else {
        return response;
    };
    let Some(problem) = response.extensions_mut().remove::<Problem>() else {
        return response;
    };
    let problem = problem.with_request_id(id.to_owned());
    // Only `Problem::into_response` attaches the extension, so status and
    // headers (Allow, WWW-Authenticate, Retry-After) are already right.
    *response.body_mut() = axum::Json(&problem).into_response().into_body();
    response.headers_mut().remove(CONTENT_LENGTH);
    response.extensions_mut().insert(problem);
    response
}

fn suppresses_access_log(options: AccessLogOptions, method: &Method, route: &str) -> bool {
    !options.log_health_probes && method == Method::GET && is_health_probe_route(route)
}

#[cfg(test)]
mod tests {
    use super::*;

    use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};

    /// The labels of every duration series the snapshot holds, sorted: the
    /// snapshot lists its keys in no fixed order.
    fn duration_series(snapshotter: &Snapshotter) -> Vec<Vec<(String, String)>> {
        let mut series: Vec<_> = snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .filter(|(key, ..)| key.key().name() == REQUEST_DURATION_METRIC)
            .map(|(key, ..)| {
                key.key()
                    .labels()
                    .map(|label| (label.key().to_owned(), label.value().to_owned()))
                    .collect()
            })
            .collect();
        series.sort();
        series
    }

    fn pair(key: &str, value: &str) -> (String, String) {
        (key.to_owned(), value.to_owned())
    }

    #[test]
    fn duration_metric_keeps_every_numeric_status_and_its_other_labels() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let series = Series::default();
        metrics::with_local_recorder(&recorder, || {
            for code in 100..=999 {
                series.record_duration(
                    "_OTHER",
                    UNMATCHED_ROUTE,
                    StatusCode::from_u16(code).unwrap(),
                    Duration::from_millis(1),
                );
            }
        });
        let expected: Vec<_> = (100..=999)
            .map(|code: u16| {
                vec![
                    pair("http_request_method", "_OTHER"),
                    pair("http_response_status_code", &code.to_string()),
                    pair("http_route", "<unmatched>"),
                ]
            })
            .collect();
        let mut expected = expected;
        expected.sort();
        assert_eq!(duration_series(&snapshotter), expected);
    }

    #[test]
    fn repeated_answers_share_one_series_and_a_new_status_adds_its_own() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let series = Series::default();
        metrics::with_local_recorder(&recorder, || {
            for status in [StatusCode::OK, StatusCode::OK, StatusCode::NOT_FOUND] {
                series.record_duration("GET", "/items/{id}", status, Duration::from_millis(1));
            }
        });
        let samples: Vec<_> = snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .filter_map(|(key, _, _, value)| match value {
                DebugValue::Histogram(samples) => Some((
                    key.key()
                        .labels()
                        .find(|label| label.key() == "http_response_status_code")
                        .map(|label| label.value().to_owned()),
                    samples.len(),
                )),
                _ => None,
            })
            .collect();
        let mut samples = samples;
        samples.sort();
        assert_eq!(
            samples,
            [(Some("200".to_owned()), 2), (Some("404".to_owned()), 1)]
        );
    }

    #[test]
    fn an_active_gauge_appears_on_first_use_and_returns_to_zero() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let series = Series::default();
        metrics::with_local_recorder(&recorder, || {
            assert_eq!(snapshotter.snapshot().into_vec().len(), 0);
            let first = Active::start(&series, "GET");
            let second = Active::start(&series, "GET");
            drop((first, second));
            // A snapshot resets the gauge it reads, so this is the only one.
            assert_eq!(gauge_values(&snapshotter), [("GET".to_owned(), 0.0)]);
        });
    }

    fn gauge_values(snapshotter: &Snapshotter) -> Vec<(String, f64)> {
        snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .filter(|(key, ..)| key.key().name() == ACTIVE_REQUESTS_METRIC)
            .filter_map(|(key, _, _, value)| match value {
                DebugValue::Gauge(value) => Some((
                    key.key()
                        .labels()
                        .next()
                        .expect("method label")
                        .value()
                        .to_owned(),
                    value.into_inner(),
                )),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_request_dropped_before_its_response_head_is_recorded_as_499() {
        use tower::ServiceExt as _;

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let handler_1 = || std::future::pending::<&'static str>();
        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let app = crate::harden(
            axum::Router::new().route("/hang", axum::routing::get(handler_1)),
            &crate::HardenOptions {
                max_body_bytes: 64,
                request_timeout: Duration::from_secs(60),
                max_in_flight: std::num::NonZeroU32::new(2),
                log_health_probes: false,
            },
        );
        let request = Request::builder().uri("/hang").body(Body::empty()).unwrap();

        // The caller gives up long before the request budget.
        let abandoned = tokio::time::timeout(Duration::from_millis(10), app.oneshot(request)).await;

        assert!(abandoned.is_err());
        assert_eq!(
            duration_series(&snapshotter),
            [vec![
                pair("http_request_method", "GET"),
                pair("http_response_status_code", "499"),
                pair("http_route", "/hang"),
            ]]
        );
    }

    #[test]
    fn probes_are_skipped_by_route_template_only() {
        let quiet = AccessLogOptions {
            log_health_probes: false,
        };
        assert!(suppresses_access_log(quiet, &Method::GET, "/health/live"));
        assert!(suppresses_access_log(quiet, &Method::GET, "/health/ready"));
        assert!(suppresses_access_log(
            quiet,
            &Method::GET,
            "/api/health/live"
        ));
        assert!(!suppresses_access_log(quiet, &Method::POST, "/health/live"));
        assert!(!suppresses_access_log(quiet, &Method::GET, UNMATCHED_ROUTE));
        let verbose = AccessLogOptions {
            log_health_probes: true,
        };
        assert!(!suppresses_access_log(
            verbose,
            &Method::GET,
            "/health/live"
        ));
    }

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
        #[expect(
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
        std::assert_matches!(span.status, Status::Error { .. });
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
        std::assert_matches!(span.status, Status::Unset);
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
            ("QUERY", "QUERY", Version::HTTP_11, "1.1"),
            ("query", "_OTHER", Version::HTTP_11, "1.1"),
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
