use std::sync::{Arc, LazyLock, Once, OnceLock};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use foldhash::HashMap;
use metrics::SharedString;
use parking_lot::Mutex;
use shared_rules::grpc::split_method_path;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;

pub(crate) const CLIENT_STARTED_METRIC: &str = "grpc_client_started_total";
pub(crate) const CLIENT_HANDLED_METRIC: &str = "grpc_client_handled_total";
pub(crate) const CLIENT_HANDLING_METRIC: &str = "grpc_client_handling_seconds";

const DIAGNOSTIC_BYTES: usize = 1024;
const CLIENT_METHOD_LIMIT: usize = 256;
const CLIENT_COMPONENT_LIMIT: usize = 256;
const HANDLING_SECONDS_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

pub const HISTOGRAMS: &[(&str, &[f64])] = &[(CLIENT_HANDLING_METRIC, HANDLING_SECONDS_BUCKETS)];

static DESCRIBE: Once = Once::new();
static CLIENT_SERIES: LazyLock<Arc<Series>> = LazyLock::new(|| Arc::new(Series::new()));

const CODE_NAMES: [(&str, &str); 17] = [
    ("OK", "OK"),
    ("CANCELLED", "Canceled"),
    ("UNKNOWN", "Unknown"),
    ("INVALID_ARGUMENT", "InvalidArgument"),
    ("DEADLINE_EXCEEDED", "DeadlineExceeded"),
    ("NOT_FOUND", "NotFound"),
    ("ALREADY_EXISTS", "AlreadyExists"),
    ("PERMISSION_DENIED", "PermissionDenied"),
    ("RESOURCE_EXHAUSTED", "ResourceExhausted"),
    ("FAILED_PRECONDITION", "FailedPrecondition"),
    ("ABORTED", "Aborted"),
    ("OUT_OF_RANGE", "OutOfRange"),
    ("UNIMPLEMENTED", "Unimplemented"),
    ("INTERNAL", "Internal"),
    ("UNAVAILABLE", "Unavailable"),
    ("DATA_LOSS", "DataLoss"),
    ("UNAUTHENTICATED", "Unauthenticated"),
];

fn code_names(code: tonic::Code) -> (&'static str, &'static str) {
    match CODE_NAMES.get(code as usize) {
        Some(&names) => names,
        None => ("UNKNOWN", "Unknown"),
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Destination {
    address: Option<Arc<str>>,
    port: Option<u16>,
}

impl Destination {
    pub(crate) fn new(uri: &http::Uri) -> Self {
        let address: Option<Arc<str>> = uri
            .host()
            .filter(|host| !host.is_empty() && host.len() <= DIAGNOSTIC_BYTES)
            .map(|host| {
                host.strip_prefix('[')
                    .and_then(|inner| inner.strip_suffix(']'))
                    .unwrap_or(host)
                    .into()
            });
        let port = address.as_ref().and_then(|_| uri.port_u16());
        Self { address, port }
    }
}

pub(crate) struct Call {
    metrics: Arc<MethodMetrics>,
    span: tracing::Span,
    started: Instant,
    finished: bool,
}

impl Call {
    pub(crate) fn start<B>(
        series: &Arc<Series>,
        request: &http::Request<B>,
        destination: &Destination,
    ) -> Self {
        let started = Instant::now();
        let metrics = series.resolve(request);
        let span = make_client_span(request, &metrics, destination);
        metrics.started.increment(1);
        Self {
            metrics,
            span,
            started,
            finished: false,
        }
    }

    pub(crate) fn span(&self) -> &tracing::Span {
        &self.span
    }

    pub(crate) fn finish(mut self, code: tonic::Code) {
        self.record(code);
        self.finished = true;
    }

    fn record(&self, code: tonic::Code) {
        let (otel_status, metric_code) = code_names(code);
        let is_error = otel_http::grpc::status_is_error(code as u16, false);
        tracing::record_all!(
            self.span,
            rpc.status_code = otel_status,
            error.type = is_error.then_some(otel_status),
            otel.status_code = is_error.then_some("ERROR"),
        );
        self.metrics
            .handled(code, metric_code, self.started.elapsed());
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        if !self.finished {
            self.record(tonic::Code::Cancelled);
        }
    }
}

fn make_client_span<B>(
    request: &http::Request<B>,
    metrics: &MethodMetrics,
    destination: &Destination,
) -> tracing::Span {
    let rpc_method = metrics.rpc_method.as_deref().unwrap_or("_OTHER");
    let rpc_method_original = if metrics.rpc_method.is_none() {
        let raw = request.uri().path().trim_start_matches('/');
        (!raw.is_empty() && raw.len() <= DIAGNOSTIC_BYTES).then_some(raw)
    } else {
        None
    };
    let raw_ua = otel_http::user_agent(request);
    let user_agent = (!raw_ua.is_empty() && raw_ua.len() <= DIAGNOSTIC_BYTES).then_some(raw_ua);
    tracing::info_span!(
        "GRPC request",
        otel.name = metrics.span_name(),
        otel.kind = "client",
        rpc.system.name = "grpc",
        rpc.method = rpc_method,
        rpc.method_original = rpc_method_original,
        server.address = destination.address.as_deref(),
        server.port = destination.port.map(i64::from),
        user_agent.original = user_agent,
        rpc.status_code = tracing::field::Empty,
        error.type = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
    )
}

#[derive(derive_more::Debug)]
#[debug("Series {{ .. }}")]
pub(crate) struct Series {
    admitted: ArcSwap<HashMap<Box<str>, Arc<MethodMetrics>>>,
    admit_lock: Mutex<()>,
    unknown: OnceLock<Arc<MethodMetrics>>,
}

struct MethodMetrics {
    rpc_method: Option<Arc<str>>,
    service: SharedString,
    method: SharedString,
    started: metrics::Counter,
    handling_seconds: metrics::Histogram,
    handled: [OnceLock<metrics::Counter>; 17],
}

impl Series {
    pub(crate) fn client() -> Arc<Self> {
        Arc::clone(&CLIENT_SERIES)
    }

    fn new() -> Self {
        describe_client_metrics();
        Self {
            admitted: ArcSwap::default(),
            admit_lock: Mutex::new(()),
            unknown: OnceLock::new(),
        }
    }

    fn resolve<B>(&self, request: &http::Request<B>) -> Arc<MethodMetrics> {
        if let Some(path) = verified_client_path(request)
            && let Some(admitted) = self.admit_path(path)
        {
            return admitted;
        }
        Arc::clone(
            self.unknown
                .get_or_init(|| Arc::new(MethodMetrics::unknown())),
        )
    }

    fn admit_path(&self, path: &str) -> Option<Arc<MethodMetrics>> {
        let snapshot = self.admitted.load();
        if let Some(existing) = snapshot.get(path) {
            return Some(Arc::clone(existing));
        }
        drop(snapshot);
        let _guard = self.admit_lock.lock();
        let current = self.admitted.load();
        if let Some(existing) = current.get(path) {
            return Some(Arc::clone(existing));
        }
        if current.len() >= CLIENT_METHOD_LIMIT {
            return None;
        }
        let metrics = Arc::new(MethodMetrics::for_path(path));
        let mut next = (**current).clone();
        next.insert(path.into(), Arc::clone(&metrics));
        self.admitted.store(Arc::new(next));
        Some(metrics)
    }
}

impl MethodMetrics {
    fn for_path(path: &str) -> Self {
        let Some((service, method)) = split_method_path(path) else {
            return Self::unknown();
        };
        let rpc_method: Arc<str> = format!("{service}/{method}").into();
        Self::build(
            Some(rpc_method),
            SharedString::from(Arc::<str>::from(service)),
            SharedString::from(Arc::<str>::from(method)),
        )
    }

    fn unknown() -> Self {
        Self::build(
            None,
            SharedString::const_str("unknown"),
            SharedString::const_str("unknown"),
        )
    }

    fn build(rpc_method: Option<Arc<str>>, service: SharedString, method: SharedString) -> Self {
        let started = metrics::counter!(
            CLIENT_STARTED_METRIC,
            "grpc_service" => service.clone(),
            "grpc_method" => method.clone(),
        );
        let handling_seconds = metrics::histogram!(
            CLIENT_HANDLING_METRIC,
            "grpc_service" => service.clone(),
            "grpc_method" => method.clone(),
        );
        Self {
            rpc_method,
            service,
            method,
            started,
            handling_seconds,
            handled: Default::default(),
        }
    }

    fn span_name(&self) -> &str {
        self.rpc_method.as_deref().unwrap_or("grpc")
    }

    fn handled(&self, code: tonic::Code, metric_code: &'static str, elapsed: Duration) {
        self.handling_seconds.record(elapsed.as_secs_f64());
        self.handled[code as usize]
            .get_or_init(|| {
                metrics::counter!(
                    CLIENT_HANDLED_METRIC,
                    "grpc_service" => self.service.clone(),
                    "grpc_method" => self.method.clone(),
                    "grpc_code" => metric_code,
                )
            })
            .increment(1);
    }
}

fn verified_client_path<B>(request: &http::Request<B>) -> Option<&str> {
    let declared = request.extensions().get::<tonic::GrpcMethod<'static>>()?;
    if declared.service().len() > CLIENT_COMPONENT_LIMIT
        || declared.method().len() > CLIENT_COMPONENT_LIMIT
    {
        return None;
    }
    let path = request.uri().path();
    let (service, rpc) = split_method_path(path)?;
    (service == declared.service() && rpc == declared.method()).then_some(path)
}

fn describe_client_metrics() {
    DESCRIBE.call_once(|| {
        metrics::describe_counter!(
            CLIENT_STARTED_METRIC,
            metrics::Unit::Count,
            "Client gRPC calls started by service and method, including calls that end before dispatch"
        );
        metrics::describe_counter!(
            CLIENT_HANDLED_METRIC,
            metrics::Unit::Count,
            "Completed client gRPC calls by service, method, and status code"
        );
        metrics::describe_histogram!(
            CLIENT_HANDLING_METRIC,
            metrics::Unit::Seconds,
            "Client gRPC time to the call's status by service and method"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_request(
        path: &str,
        declared: Option<(&'static str, &'static str)>,
    ) -> http::Request<()> {
        let mut request = http::Request::builder()
            .method("POST")
            .uri(path)
            .body(())
            .unwrap();
        if let Some((service, method)) = declared {
            request
                .extensions_mut()
                .insert(tonic::GrpcMethod::new(service, method));
        }
        request
    }

    #[test]
    fn destination_strips_ipv6_brackets_and_caps_overlong_hosts() {
        let ipv6 = Destination::new(&"https://[::1]:50051".parse().unwrap());
        assert_eq!(ipv6.address.as_deref(), Some("::1"));
        assert_eq!(ipv6.port, Some(50051));

        let named = Destination::new(&"https://grpc.example.test".parse().unwrap());
        assert_eq!(named.address.as_deref(), Some("grpc.example.test"));
        assert_eq!(named.port, None);

        let overlong_uri: http::Uri =
            format!("https://{}.test:50051", "a".repeat(DIAGNOSTIC_BYTES))
                .parse()
                .unwrap();
        let overlong = Destination::new(&overlong_uri);
        assert!(overlong.address.is_none());
        assert!(overlong.port.is_none());
    }

    #[test]
    fn code_labels_match_otel_and_prometheus_conventions() {
        assert_eq!(code_names(tonic::Code::Ok), ("OK", "OK"));
        assert_eq!(
            code_names(tonic::Code::Cancelled),
            ("CANCELLED", "Canceled")
        );
        assert_eq!(
            code_names(tonic::Code::DeadlineExceeded),
            ("DEADLINE_EXCEEDED", "DeadlineExceeded")
        );
        assert_eq!(
            code_names(tonic::Code::FailedPrecondition),
            ("FAILED_PRECONDITION", "FailedPrecondition")
        );
        assert_eq!(
            code_names(tonic::Code::Unauthenticated),
            ("UNAUTHENTICATED", "Unauthenticated")
        );
        for raw in 0..17 {
            let code = tonic::Code::from_i32(raw);
            let (otel, prom) = code_names(code);
            assert!(!otel.is_empty() && !prom.is_empty());
        }
    }

    #[test]
    fn series_enforces_component_validation_and_method_cardinality_cap() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _guard = metrics::set_default_local_recorder(&recorder);

        let series = Arc::new(Series::new());
        let destination = Destination::new(&"http://127.0.0.1:50051".parse().unwrap());

        let overlong: &'static str =
            Box::leak("x".repeat(CLIENT_COMPONENT_LIMIT + 1).into_boxed_str());
        for (path, declared) in [
            ("/raw.Service/Unary".to_owned(), None),
            (
                "/mismatch.Service/Unary".to_owned(),
                Some(("other.Service", "Unary")),
            ),
            (
                "/extra.Service/Unary/Tail".to_owned(),
                Some(("extra.Service", "Unary/Tail")),
            ),
            (format!("/{overlong}/Unary"), Some((overlong, "Unary"))),
            (
                format!("/overlong.Service/{overlong}"),
                Some(("overlong.Service", overlong)),
            ),
        ] {
            drop(Call::start(
                &series,
                &test_request(&path, declared),
                &destination,
            ));
        }

        let boundary: &'static str = Box::leak("b".repeat(CLIENT_COMPONENT_LIMIT).into_boxed_str());
        Call::start(
            &series,
            &test_request(
                &format!("/{boundary}/{boundary}"),
                Some((boundary, boundary)),
            ),
            &destination,
        )
        .finish(tonic::Code::Ok);

        for index in 0..CLIENT_METHOD_LIMIT {
            let method: &'static str = Box::leak(format!("Rpc{index}").into_boxed_str());
            Call::start(
                &series,
                &test_request(
                    &format!("/cap.Service/{method}"),
                    Some(("cap.Service", method)),
                ),
                &destination,
            )
            .finish(tonic::Code::Unavailable);
        }

        assert_eq!(series.admitted.load().len(), CLIENT_METHOD_LIMIT);

        let rendered = handle.render();
        assert!(
            rendered.contains(&format!(
                r#"grpc_client_handled_total{{grpc_service="{boundary}",grpc_method="{boundary}",grpc_code="OK"}} 1"#
            )),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                r#"grpc_client_handled_total{grpc_service="unknown",grpc_method="unknown",grpc_code="Canceled"} 5"#
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                r#"grpc_client_handled_total{grpc_service="unknown",grpc_method="unknown",grpc_code="Unavailable"} 1"#
            ),
            "{rendered}"
        );
        assert!(!rendered.contains("Rpc255"), "{rendered}");
        assert!(!rendered.contains("raw.Service"), "{rendered}");
    }
}
