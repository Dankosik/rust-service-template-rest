use super::exporter::{
    CERTIFICATE_VARS, CLIENT_CERTIFICATE_VARS, CLIENT_KEY_VARS, Endpoint, TrustFile,
};
use super::*;

mod delivery;

#[derive(Debug)]
struct ShutdownExporter {
    attempted: Mutex<Option<tokio::sync::oneshot::Sender<Duration>>>,
    release: Option<Mutex<std::sync::mpsc::Receiver<()>>>,
    finished: Option<tokio::sync::oneshot::Sender<()>>,
}

impl SpanExporter for ShutdownExporter {
    fn export(&self, _batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
        std::future::ready(Ok(()))
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.attempted
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(timeout)
            .unwrap();
        if let Some(release) = &self.release {
            release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
        }
        Ok(())
    }
}

impl Drop for ShutdownExporter {
    fn drop(&mut self) {
        if let Some(finished) = self.finished.take() {
            let _ = finished.send(());
        }
    }
}

#[tokio::test]
async fn shutdown_reserves_join_slack_inside_the_absolute_deadline() {
    for (sdk_budget, maximum) in [
        (Duration::from_secs(5), Duration::from_millis(500)),
        (Duration::from_millis(20), Duration::from_millis(20)),
    ] {
        let (attempted, budget) = tokio::sync::oneshot::channel();
        let handle = TracerProviderHandle {
            provider: SdkTracerProvider::builder()
                .with_simple_exporter(ShutdownExporter {
                    attempted: Mutex::new(Some(attempted)),
                    release: None,
                    finished: None,
                })
                .build(),
            exporter_state: ExporterState::Disabled,
            drain: Arc::default(),
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        assert_eq!(
            handle.shutdown_until(sdk_budget, deadline).await,
            ProviderShutdown::Completed
        );
        let budget = budget.await.unwrap();
        assert!(budget > Duration::ZERO);
        assert!(budget <= maximum);
    }
}

#[tokio::test]
async fn expired_or_short_shutdown_still_attempts_sdk_and_reports_unfinished_work() {
    for remaining in [Duration::ZERO, Duration::from_millis(20)] {
        let (attempted, budget) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let (finished, joined) = tokio::sync::oneshot::channel();
        let handle = TracerProviderHandle {
            provider: SdkTracerProvider::builder()
                .with_simple_exporter(ShutdownExporter {
                    attempted: Mutex::new(Some(attempted)),
                    release: Some(Mutex::new(wait)),
                    finished: Some(finished),
                })
                .build(),
            exporter_state: ExporterState::Disabled,
            drain: Arc::default(),
        };
        let deadline = tokio::time::Instant::now() + remaining;
        let ProviderShutdown::Incomplete(reasons) = handle.shutdown(deadline).await else {
            panic!("blocked SDK shutdown cannot complete before release");
        };
        assert!(reasons.contains(ProviderShutdownReasons::DEADLINE));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), budget)
                .await
                .unwrap()
                .unwrap(),
            Duration::ZERO
        );
        release.send(()).unwrap();
        // Last-provider drop follows the SDK shutdown call, so the
        // fixture cannot leave its blocking exporter running.
        tokio::time::timeout(Duration::from_secs(2), joined)
            .await
            .unwrap()
            .unwrap();
    }
}

pub(super) fn options(endpoint: &str) -> TracingOptions {
    TracingOptions {
        service_name: "svc".into(),
        service_version: "1.0.0".into(),
        vcs_revision: "abc".into(),
        instance_id: Some(String::new()),
        deployment_environment: "test".into(),
        sampler: ResolvedSampler::AlwaysOn,
        otlp_endpoint: (!endpoint.is_empty()).then(|| endpoint.to_owned()),
        otlp_headers: None,
    }
}

#[test]
fn the_span_queue_is_larger_than_the_sdk_default_unless_the_operator_sets_one() {
    assert!(format!("{:?}", batch_config(&env(&[]))).contains("max_queue_size: 16384"));
    assert!(
        format!(
            "{:?}",
            batch_config(&env(&[("OTEL_BSP_MAX_QUEUE_SIZE", "2048")]))
        )
        .contains("max_queue_size: 2048")
    );
    assert!(
        format!(
            "{:?}",
            batch_config(&env(&[("OTEL_BSP_MAX_QUEUE_SIZE", "16k")]))
        )
        .contains("max_queue_size: 16384")
    );
    assert!(
        format!(
            "{:?}",
            batch_config(&env(&[("OTEL_BSP_MAX_EXPORT_BATCH_SIZE", "4096")]))
        )
        .contains("max_export_batch_size: 4096")
    );
    assert!(std::mem::size_of::<SpanData>() * SPAN_QUEUE_CAPACITY <= 16 << 20);
}

pub(super) fn env<'a>(pairs: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
    }
}

#[derive(Debug)]
struct Fixed(fn() -> OTelSdkResult);

impl SpanExporter for Fixed {
    fn export(&self, _batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
        std::future::ready((self.0)())
    }
}

#[test]
fn finished_exports_are_counted_by_outcome() {
    use futures_util::FutureExt;
    use opentelemetry::trace::{SpanContext, SpanKind, Status};

    let span = || SpanData {
        span_context: SpanContext::empty_context(),
        parent_span_id: opentelemetry::trace::SpanId::INVALID,
        parent_span_is_remote: false,
        span_kind: SpanKind::Internal,
        name: "span".into(),
        start_time: std::time::SystemTime::UNIX_EPOCH,
        end_time: std::time::SystemTime::UNIX_EPOCH,
        attributes: Vec::new(),
        dropped_attributes_count: 0,
        events: sdktrace::SpanEvents::default(),
        links: sdktrace::SpanLinks::default(),
        status: Status::Unset,
        instrumentation_scope: opentelemetry::InstrumentationScope::default(),
    };
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    metrics::with_local_recorder(&recorder, || {
        let export = |exporter: Fixed, timeout: Duration, spans: usize| {
            let _ = ObservedExporter {
                inner: exporter,
                timeout,
                drain: Arc::default(),
            }
            .export((0..spans).map(|_| span()).collect())
            .now_or_never()
            .expect("the fixed exporter is ready");
        };
        let budget = Duration::from_secs(60);
        export(Fixed(|| Ok(())), budget, 3);
        export(
            Fixed(|| Err(OTelSdkError::Timeout(Duration::ZERO))),
            budget,
            2,
        );
        export(
            Fixed(|| Err(OTelSdkError::InternalFailure("refused".into()))),
            budget,
            1,
        );
        // The OTLP exporter reports its own timeout as an internal failure.
        export(
            Fixed(|| Err(OTelSdkError::InternalFailure("network error".into()))),
            Duration::ZERO,
            4,
        );
    });
    let rendered = recorder.handle().render();
    for series in [
        "otel_sdk_exporter_span_exported_total 3",
        "otel_sdk_exporter_span_exported_total{error_type=\"timeout\"} 6",
        "otel_sdk_exporter_span_exported_total{error_type=\"internal_failure\"} 1",
    ] {
        assert!(rendered.contains(series), "{series}: {rendered}");
    }
}

#[test]
fn resource_carries_typed_identity() {
    let mut options = options("");
    options.instance_id = Some("instance-1".to_owned());
    let resource = resource(&options);
    let get = |key: &str| {
        resource
            .get(&opentelemetry::Key::new(key.to_owned()))
            .map(|v| v.to_string())
    };
    assert_eq!(get(attribute::SERVICE_NAME).as_deref(), Some("svc"));
    assert_eq!(get(attribute::SERVICE_VERSION).as_deref(), Some("1.0.0"));
    assert_eq!(
        get(attribute::SERVICE_INSTANCE_ID).as_deref(),
        Some("instance-1")
    );
    assert_eq!(
        get(attribute::DEPLOYMENT_ENVIRONMENT_NAME).as_deref(),
        Some("test")
    );
    assert_eq!(
        get(attribute::VCS_REF_HEAD_REVISION).as_deref(),
        Some("abc")
    );
    assert!(get("telemetry.sdk.language").is_some());
}
