use std::mem::DropGuard;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::extract::{MatchedPath, Request, State};
use axum::http::header::{CONTENT_LENGTH, TRANSFER_ENCODING};
use axum::http::{Method, StatusCode, Uri, Version};
use axum::middleware::Next;
use axum::response::Response;
use compact_str::{CompactString, format_compact};
use enum_map::{Enum, EnumMap};
use foldhash::HashMap;
use http_body::Body as _;
use metrics::{Gauge, Histogram, Label, SharedString};
use opentelemetry::context::FutureExt as _;
use parking_lot::Mutex;
use shared_rules::otel_http::{request_method, response_status_code};
use tokio::time::Instant;
use tower_http::request_id::RequestId;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::{
    find_context_from_tracing, otel_trace_span, set_parent_or_fallback,
};

use crate::body::{BodyEnd, on_body_end};
use crate::harden::AdmissionShed;
use crate::probes::ProbeRoutes;
use crate::problem::{Code, Problem};
use infra_http_transport::ConnectionInfo;

pub(crate) const REQUEST_DURATION_METRIC: &str = "http_server_request_duration_seconds";

pub(crate) const ACTIVE_REQUESTS_METRIC: &str = "http_server_active_requests";

pub(crate) const LATENCY_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

pub(crate) const ADMISSION_WAIT_METRIC: &str = "http_server_admission_wait_seconds";

pub(crate) const SHED_REQUESTS_METRIC: &str = "http_server_shed_requests_total";

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
        "Requests rejected without running a handler because no in-flight permit freed within the admission wait."
    );
}

const UNMATCHED_ROUTE: &str = "<unmatched>";

#[derive(Clone, Debug)]
pub(crate) struct Observer {
    log_health_probes: bool,
    trust_trace_context: bool,
    probes: ProbeRoutes,
    series: Arc<Series>,
}

impl Observer {
    pub(crate) fn new(
        log_health_probes: bool,
        trust_trace_context: bool,
        probes: ProbeRoutes,
    ) -> Self {
        Self {
            log_health_probes,
            trust_trace_context,
            probes,
            series: Arc::default(),
        }
    }
}

pub(crate) async fn observe(
    State(Observer {
        log_health_probes,
        trust_trace_context,
        probes,
        series,
    }): State<Observer>,
    matched: Option<MatchedPath>,
    mut request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let request_id = request.extensions().get::<RequestId>().cloned();
    let request_id_text = request_id
        .as_ref()
        .and_then(|id| id.header_value().to_str().ok());
    let connection = request.extensions_mut().remove::<ConnectionInfo>();
    let participant =
        infra_http_transport::admit_trace_context(request.headers_mut(), trust_trace_context);
    let method = request.method();
    let method_label = request_method(method.as_str());
    let version = request.version();
    let scheme = url_scheme(request.uri(), connection.as_ref());
    let route = matched.as_ref().map(MatchedPath::as_str);
    let span = make_span(version, method_label, route, request_id_text, scheme);
    let parent = if participant {
        otel_http::extract_context(request.headers())
    } else {
        opentelemetry::Context::new()
    };
    let fallback = set_parent_or_fallback(&span, parent);
    let response_context = participant.then(|| {
        fallback
            .clone()
            .unwrap_or_else(|| find_context_from_tracing(&span))
    });
    let log_access = !suppresses_access_log(
        log_health_probes,
        &probes,
        method,
        route.unwrap_or(UNMATCHED_ROUTE),
    );
    let gauge = series.active_gauge(method_label, scheme);
    gauge.increment(1);
    let mut observation = DropGuard::new(
        RequestObservation {
            log_access,
            series,
            gauge,
            method_label,
            scheme,
            matched,
            request_id,
            span,
            started,
            connection,
            head: None,
        },
        RequestObservation::abandon,
    );

    let traced = next.run(request).instrument(observation.span.clone());
    let mut response = match fallback {
        None => traced.await,
        Some(context) => traced.with_context(context).await,
    };

    let status = response.status();
    let extensions = response.extensions();
    // template:begin authn:http-observe-scope-refusal
    if status == StatusCode::FORBIDDEN
        && extensions
            .get::<crate::authn::InsufficientScope>()
            .is_some()
    {
        observation
            .span
            .set_attribute("authz.reason", "insufficient_scope");
    }
    // template:end authn:http-observe-scope-refusal
    observation.head = Some(ResponseHead {
        status,
        problem_code: extensions.get::<Problem>().map(Problem::code),
        load_shed: extensions.get::<AdmissionShed>().is_some(),
    });
    let headers = response.headers_mut();
    headers.remove("traceparent");
    headers.remove("tracestate");
    if let Some(context) = response_context {
        otel_http::inject_context(&context, headers);
    }
    if method_label == "HEAD"
        || method_label == "CONNECT" && status.is_success()
        || status.is_informational()
        || matches!(status, StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED)
    {
        DropGuard::dismiss(observation).record(None, None);
        return response;
    }
    let remaining = http1_response_length(version, &response);
    response.map(|body| {
        on_body_end(body, remaining, move |end| {
            let error = match end {
                BodyEnd::Complete => None,
                BodyEnd::Error => Some("response_body_error"),
            };
            DropGuard::dismiss(observation).record(error, None);
        })
    })
}

fn http1_response_length(version: Version, response: &Response) -> Option<u64> {
    if version > Version::HTTP_11 || response.headers().contains_key(TRANSFER_ENCODING) {
        return None;
    }
    response.body().size_hint().exact().or_else(|| {
        response
            .headers()
            .get(CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .parse()
            .ok()
    })
}

struct ResponseHead {
    status: StatusCode,
    problem_code: Option<Code>,
    load_shed: bool,
}

struct RequestObservation {
    log_access: bool,
    series: Arc<Series>,
    gauge: Gauge,
    method_label: &'static str,
    scheme: Scheme,
    matched: Option<MatchedPath>,
    request_id: Option<RequestId>,
    span: tracing::Span,
    started: Instant,
    connection: Option<ConnectionInfo>,
    head: Option<ResponseHead>,
}

impl RequestObservation {
    fn abandon(self) {
        let (reason, log_status) = if self
            .connection
            .as_ref()
            .is_some_and(ConnectionInfo::was_forced)
        {
            ("server_shutdown", 503)
        } else {
            ("client_closed", 499)
        };
        self.record(Some(reason), Some(log_status));
    }

    fn record(self, interruption: Option<&'static str>, log_status: Option<u16>) {
        let elapsed = self.started.elapsed();
        let route = self.matched.as_ref().map(MatchedPath::as_str);
        let status = self.head.as_ref().map(|head| head.status);
        let error_type =
            interruption.or_else(|| status.filter(StatusCode::is_server_error).map(status_label));
        self.gauge.decrement(1);
        self.series.record_duration(
            route,
            DurationLabels {
                method: self.method_label,
                scheme: self.scheme,
                status,
                error_type,
            },
            elapsed,
        );
        if let Some(status) = status {
            self.span
                .set_attribute("http.response.status_code", i64::from(status.as_u16()));
        }
        if let Some(error_type) = error_type {
            self.span.set_attribute("error.type", error_type);
            self.span
                .set_status(opentelemetry::trace::Status::error(""));
        }
        if !self.log_access {
            return;
        }
        let status = status.map(|value| value.as_u16()).or(log_status);
        let termination = interruption.or_else(|| {
            self.head
                .as_ref()
                .filter(|head| head.load_shed)
                .map(|_| "load_shed")
        });
        let problem_code = self
            .head
            .and_then(|head| head.problem_code)
            .map(Code::as_str);
        let request_id = self
            .request_id
            .as_ref()
            .and_then(|id| id.header_value().to_str().ok());
        let duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        self.span.in_scope(|| {
            tracing::info!(
                method = self.method_label,
                route = route.unwrap_or(UNMATCHED_ROUTE),
                status,
                duration_ms,
                problem_code,
                termination,
                request_id,
                "http_request"
            );
        });
    }
}

#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn make_span(
    version: Version,
    method: &'static str,
    route: Option<&str>,
    request_id: Option<&str>,
    scheme: Scheme,
) -> tracing::Span {
    let name_method = if method == "_OTHER" { "HTTP" } else { method };
    let span_name = match route {
        Some(route) => format_compact!("{name_method} {route}"),
        None => CompactString::const_new(name_method),
    };
    let span = otel_trace_span!(
        "HTTP request",
        otel.name = span_name.as_str(),
        otel.kind = ?opentelemetry::trace::SpanKind::Server,
        request_id,
    );
    span.set_attribute("http.request.method", method);
    if let Some(route) = route {
        span.set_attribute("http.route", route.to_owned());
    }
    span.set_attribute("network.protocol.version", otel_http::http_flavor(version));
    span.set_attribute("url.scheme", scheme.as_str());
    span
}

#[derive(Clone, Copy, Debug, Enum, Eq, Hash, PartialEq)]
enum Scheme {
    Http,
    Https,
}

impl Scheme {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

fn url_scheme(uri: &Uri, connection: Option<&ConnectionInfo>) -> Scheme {
    match uri
        .scheme_str()
        .or_else(|| connection.map(ConnectionInfo::scheme))
    {
        Some("https") => Scheme::Https,
        _ => Scheme::Http,
    }
}

#[derive(Clone, Copy, Debug, Enum)]
enum MethodSlot {
    Connect,
    Delete,
    Get,
    Head,
    Options,
    Patch,
    Post,
    Put,
    Query,
    Trace,
    Other,
}

impl MethodSlot {
    fn from_label(method: &'static str) -> Self {
        match method {
            "CONNECT" => Self::Connect,
            "DELETE" => Self::Delete,
            "GET" => Self::Get,
            "HEAD" => Self::Head,
            "OPTIONS" => Self::Options,
            "PATCH" => Self::Patch,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "QUERY" => Self::Query,
            "TRACE" => Self::Trace,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct DurationLabels {
    method: &'static str,
    scheme: Scheme,
    status: Option<StatusCode>,
    error_type: Option<&'static str>,
}

type RouteDurations = HashMap<DurationLabels, Histogram>;

#[derive(Debug, Default)]
struct Series {
    active: EnumMap<MethodSlot, EnumMap<Scheme, OnceLock<Gauge>>>,
    durations: ArcSwap<HashMap<Box<str>, RouteDurations>>,
    durations_write: Mutex<()>,
}

impl Series {
    fn active_gauge(&self, method: &'static str, scheme: Scheme) -> Gauge {
        self.active[MethodSlot::from_label(method)][scheme]
            .get_or_init(|| {
                metrics::gauge!(
                    ACTIVE_REQUESTS_METRIC,
                    "http_request_method" => method,
                    "url_scheme" => scheme.as_str()
                )
            })
            .clone()
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn record_duration(&self, route: Option<&str>, key: DurationLabels, elapsed: Duration) {
        let route_key = route.unwrap_or_default();
        let seconds = elapsed.as_secs_f64();
        if let Some(histogram) = self
            .durations
            .load()
            .get(route_key)
            .and_then(|route_durations| route_durations.get(&key))
        {
            histogram.record(seconds);
            return;
        }
        let _guard = self.durations_write.lock();
        let current = self.durations.load_full();
        if let Some(histogram) = current
            .get(route_key)
            .and_then(|route_durations| route_durations.get(&key))
        {
            histogram.record(seconds);
            return;
        }
        let histogram = duration_histogram(route, key);
        let mut next = (*current).clone();
        next.entry(route_key.into())
            .or_default()
            .insert(key, histogram.clone());
        self.durations.store(Arc::new(next));
        histogram.record(seconds);
    }
}

fn duration_histogram(route: Option<&str>, key: DurationLabels) -> Histogram {
    let mut labels = Vec::with_capacity(5);
    labels.push(Label::new("http_request_method", key.method));
    labels.push(Label::new("url_scheme", key.scheme.as_str()));
    if let Some(status) = key.status {
        labels.push(Label::new(
            "http_response_status_code",
            status_label(status),
        ));
    }
    if let Some(route) = route {
        labels.push(Label::new(
            "http_route",
            SharedString::from(Arc::<str>::from(route)),
        ));
    }
    if let Some(error_type) = key.error_type {
        labels.push(Label::new("error_type", error_type));
    }
    metrics::histogram!(REQUEST_DURATION_METRIC, labels)
}

fn status_label(status: StatusCode) -> &'static str {
    response_status_code(status.as_u16()).unwrap_or_default()
}

fn suppresses_access_log(
    log_health_probes: bool,
    probes: &ProbeRoutes,
    method: &Method,
    route: &str,
) -> bool {
    !log_health_probes && method == Method::GET && probes.contains(method, route)
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};

    fn duration_series(snapshotter: &Snapshotter) -> Vec<Vec<Label>> {
        let mut series: Vec<_> = snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .filter(|(key, ..)| key.key().name() == REQUEST_DURATION_METRIC)
            .map(|(key, ..)| key.key().labels().cloned().collect())
            .collect();
        series.sort();
        series
    }

    #[test]
    fn duration_metric_keeps_every_numeric_status_and_its_other_labels() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let series = Series::default();
        metrics::with_local_recorder(&recorder, || {
            for code in 100..=999 {
                series.record_duration(
                    None,
                    DurationLabels {
                        method: "_OTHER",
                        scheme: Scheme::Https,
                        status: Some(StatusCode::from_u16(code).unwrap()),
                        error_type: None,
                    },
                    Duration::from_millis(1),
                );
            }
        });
        let mut expected: Vec<_> = (100..=999)
            .map(|code: u16| {
                vec![
                    Label::new("http_request_method", "_OTHER"),
                    Label::new("url_scheme", "https"),
                    Label::new("http_response_status_code", code.to_string()),
                ]
            })
            .collect();
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
                series.record_duration(
                    Some("/items/{id}"),
                    DurationLabels {
                        method: "GET",
                        scheme: Scheme::Http,
                        status: Some(status),
                        error_type: None,
                    },
                    Duration::from_millis(1),
                );
            }
        });
        let mut samples: Vec<_> = snapshotter
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
        samples.sort();
        assert_eq!(
            samples,
            [(Some("200".to_owned()), 2), (Some("404".to_owned()), 1)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_records_full_duration_and_the_known_status_for_each_terminal_outcome() {
        use axum::body::Bytes;
        use futures_util::{StreamExt as _, stream};
        use http_body_util::BodyExt as _;
        use tower::ServiceExt as _;

        let delay = Duration::from_secs(2);
        for (fail, abandon, error_type) in [
            (false, false, None),
            (true, false, Some("response_body_error")),
            (false, true, Some("client_closed")),
        ] {
            let recorder = DebuggingRecorder::new();
            let snapshotter = recorder.snapshotter();
            let _recorder = metrics::set_default_local_recorder(&recorder);
            let handler = move || async move {
                let initial = stream::iter([Ok(Bytes::from_static(b"initial"))]);
                let tail = stream::once(async move {
                    tokio::time::sleep(delay).await;
                    if fail {
                        Err(std::io::Error::other("injected response body error"))
                    } else {
                        Ok(Bytes::from_static(b"tail"))
                    }
                });
                Body::from_stream(initial.chain(tail))
            };
            #[expect(
                clippy::disallowed_methods,
                reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
            )]
            let app = crate::harden(
                axum::Router::new().route("/stream", axum::routing::get(handler)),
                &crate::HardenOptions {
                    max_body_bytes: 64,
                    request_timeout: Duration::from_secs(1),
                    max_in_flight: None,
                    admission_wait: Duration::from_secs(1),
                    log_health_probes: false,
                    trust_trace_context: false,
                    draining: None,
                },
            );
            let request = Request::builder()
                .uri("https://example.test/stream")
                .body(Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let mut body = response.into_body();
            assert_eq!(
                body.frame().await.unwrap().unwrap().into_data().unwrap(),
                "initial"
            );
            let metrics = snapshotter.snapshot().into_vec();
            assert_eq!(metrics.len(), 1, "{metrics:?}");
            let (key, _, _, value) = &metrics[0];
            assert_eq!(key.key().name(), ACTIVE_REQUESTS_METRIC);
            assert_eq!(
                key.key().labels().cloned().collect::<Vec<_>>(),
                [
                    Label::new("http_request_method", "GET"),
                    Label::new("url_scheme", "https"),
                ]
            );
            assert_eq!(*value, DebugValue::Gauge(1.0.into()));

            if abandon {
                tokio::time::advance(delay).await;
                drop(std::mem::take(&mut body));
            } else {
                let frame = body.frame().await.unwrap();
                if fail {
                    assert!(frame.is_err());
                } else {
                    assert_eq!(frame.unwrap().into_data().unwrap(), "tail");
                    assert!(body.frame().await.is_none());
                }
            }
            let metrics = snapshotter.snapshot().into_vec();
            assert_eq!(metrics.len(), 2, "{metrics:?}");
            for (key, _, _, value) in metrics {
                match key.key().name() {
                    ACTIVE_REQUESTS_METRIC => {
                        assert_eq!(value, DebugValue::Gauge((-1.0).into()));
                    }
                    REQUEST_DURATION_METRIC => {
                        let mut expected = vec![
                            Label::new("http_request_method", "GET"),
                            Label::new("url_scheme", "https"),
                            Label::new("http_response_status_code", "200"),
                            Label::new("http_route", "/stream"),
                        ];
                        if let Some(error_type) = error_type {
                            expected.push(Label::new("error_type", error_type));
                        }
                        assert_eq!(key.key().labels().cloned().collect::<Vec<_>>(), expected);
                        let DebugValue::Histogram(samples) = value else {
                            panic!("expected a duration histogram, got {value:?}");
                        };
                        assert_eq!(samples.len(), 1, "{samples:?}");
                        assert!(
                            (2.0..2.01).contains(&samples[0].into_inner()),
                            "{samples:?}"
                        );
                    }
                    name => panic!("unexpected metric {name}"),
                }
            }
            drop(body);
            assert!(snapshotter.snapshot().into_vec().into_iter().all(
                |(_, _, _, value)| match value {
                    DebugValue::Gauge(value) => value.into_inner() == 0.0,
                    DebugValue::Histogram(samples) => samples.is_empty(),
                    _ => false,
                }
            ));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_dropped_before_its_response_head_has_no_response_status_metric_label() {
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
                request_timeout: Duration::from_mins(1),
                max_in_flight: std::num::NonZeroU32::new(2),
                admission_wait: Duration::from_mins(1),
                log_health_probes: false,
                trust_trace_context: false,
                draining: None,
            },
        );
        let request = Request::builder().uri("/hang").body(Body::empty()).unwrap();

        let abandoned = tokio::time::timeout(Duration::from_millis(10), app.oneshot(request)).await;

        assert!(abandoned.is_err());
        assert_eq!(
            duration_series(&snapshotter),
            [vec![
                Label::new("http_request_method", "GET"),
                Label::new("url_scheme", "http"),
                Label::new("http_route", "/hang"),
                Label::new("error_type", "client_closed"),
            ]]
        );
    }

    #[test]
    fn probes_are_skipped_by_route_template_only() {
        let probes = ProbeRoutes::new(vec![
            ("/health/live".into(), true),
            ("/api/health/ready".into(), true),
        ]);
        assert!(suppresses_access_log(
            false,
            &probes,
            &Method::GET,
            "/health/live"
        ));
        assert!(suppresses_access_log(
            false,
            &probes,
            &Method::GET,
            "/api/health/ready"
        ));
        assert!(!suppresses_access_log(
            false,
            &probes,
            &Method::GET,
            "/business/health/live"
        ));
        assert!(!suppresses_access_log(
            false,
            &probes,
            &Method::HEAD,
            "/health/live"
        ));
        assert!(!suppresses_access_log(
            false,
            &probes,
            &Method::POST,
            "/health/live"
        ));
        assert!(!suppresses_access_log(
            true,
            &probes,
            &Method::GET,
            "/health/live"
        ));
    }

    async fn exported_span(request: Request) -> opentelemetry_sdk::trace::SpanData {
        served(request, false).await.1
    }

    async fn served(
        request: Request,
        trust_trace_context: bool,
    ) -> (axum::http::HeaderMap, opentelemetry_sdk::trace::SpanData) {
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
                admission_wait: Duration::from_secs(5),
                log_health_probes: false,
                trust_trace_context,
                draining: None,
            },
        );
        let (parts, body) = app.oneshot(request).await.unwrap().into_parts();
        http_body_util::BodyExt::collect(body).await.unwrap();

        let mut spans = exporter.get_finished_spans().unwrap();
        assert_eq!(spans.len(), 1, "one server span, got {spans:?}");
        (parts.headers, spans.remove(0))
    }

    #[tokio::test]
    async fn only_a_trusted_trace_participant_receives_the_server_context() {
        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );
        let trace_id = "0af7651916cd43dd8448eb211c80319c";
        let parent_id = "b7ad6b7169203331";
        let fields = format!("{trace_id}-{parent_id}");
        let valid = format!("00-{fields}-01");
        for (trusted, parents, accepted) in [
            (true, vec![valid.clone()], true),
            (true, vec![format!("0a-{fields}-ff-opaque-?")], true),
            (true, vec![format!("01-{fields}-01")], true),
            (false, vec![valid.clone()], false),
            (false, vec![format!("00-{fields}-00")], false),
            (true, vec![], false),
            (true, vec![valid.clone(), valid], false),
            (true, vec![format!("00-abc-{parent_id}-01")], false),
            (true, vec![format!("00-{trace_id}-def-01")], false),
            (true, vec![format!("00-{fields}-1")], false),
            (true, vec![format!("00-{fields}-01-")], false),
            (true, vec![format!("ff-{fields}-01")], false),
            (true, vec![format!("01-{fields}-01opaque")], false),
            (
                true,
                vec![format!("01-{}-{parent_id}-01", "0".repeat(32))],
                false,
            ),
            (
                true,
                vec![format!("00-{trace_id}-0000000000000000-01")],
                false,
            ),
            (true, vec![format!("0A-{fields}-01")], false),
        ] {
            let mut request = Request::builder()
                .uri("/items/1")
                .header("tracestate", "first=one")
                .header("tracestate", "second=two");
            for parent in &parents {
                request = request.header("traceparent", parent);
            }
            let (headers, span) = served(request.body(Body::empty()).unwrap(), trusted).await;
            let context = span.span_context;
            if accepted {
                assert_eq!(span.parent_span_id.to_string(), parent_id, "{parents:?}");
                assert_eq!(context.trace_id().to_string(), trace_id);
                assert_eq!(context.trace_state().header(), "first=one,second=two");
                let expected = format!("00-{trace_id}-{}-01", context.span_id());
                assert_eq!(headers["traceparent"], expected);
                assert_eq!(headers["tracestate"], "first=one,second=two");
            } else {
                assert_eq!(
                    span.parent_span_id,
                    opentelemetry::trace::SpanId::INVALID,
                    "{parents:?}"
                );
                assert_ne!(context.trace_id().to_string(), trace_id);
                assert!(context.trace_state().header().is_empty());
                assert!(!headers.contains_key("traceparent"));
                assert!(!headers.contains_key("tracestate"));
            }
        }
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
            "http.request.method_original",
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

        assert_eq!(span.name, "GET");
        std::assert_matches!(span.status, Status::Unset);
        for absent in ["http.route", "user_agent.original", "error.type"] {
            assert_eq!(attribute(&span, absent), None, "{absent}");
        }
        assert_eq!(
            attribute(&span, "http.response.status_code"),
            Some(404.into())
        );
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
            let span_method = if normalized == "_OTHER" {
                "HTTP"
            } else {
                normalized
            };
            assert_eq!(span.name, span_method);
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
            assert_eq!(attribute(&span, "error.type"), None);
        }
    }
}
