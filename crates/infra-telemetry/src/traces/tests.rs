use super::exporter::{
    CERTIFICATE_VARS, CLIENT_CERTIFICATE_VARS, CLIENT_KEY_VARS, SettingScope, TrustFile,
};
use super::*;

mod delivery;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
}

pub(super) fn tracer_provider(
    options: &TracingOptions<'_>,
    env: impl Fn(&str) -> Option<OsString>,
    trust: &CollectorTrust,
) -> Result<TracerProviderHandle, TracingError> {
    runtime().block_on(prepare_tracer_provider(options, env, trust.clone()))
}

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
        self.attempted.lock().take().unwrap().send(timeout).unwrap();
        if let Some(release) = &self.release {
            release.lock().recv_timeout(Duration::from_secs(2)).unwrap();
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
            handle.shutdown_with_budget(sdk_budget, deadline).await,
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
        tokio::time::timeout(Duration::from_secs(2), joined)
            .await
            .unwrap()
            .unwrap();
    }
}

pub(super) fn options(endpoint: &str) -> TracingOptions<'_> {
    TracingOptions {
        service_name: "svc",
        service_version: "1.0.0",
        vcs_revision: "abc",
        instance_id: Some(""),
        deployment_environment: "test",
        sampler: ResolvedSampler::AlwaysOn,
        otlp_endpoint: (!endpoint.is_empty()).then_some(endpoint),
        otlp_headers: None,
    }
}

#[test]
fn span_processor_uses_positive_sizes_and_preserves_valid_overrides() {
    let queue = sdktrace::OTEL_BSP_MAX_QUEUE_SIZE;
    let batch = sdktrace::OTEL_BSP_MAX_EXPORT_BATCH_SIZE;
    for (vars, queue_size, batch_size, expected_invalid) in [
        (vec![], 16_384, 512, vec![]),
        (vec![(queue, "2048")], 2048, 512, vec![]),
        (vec![(queue, "16k")], 16_384, 512, vec![queue]),
        (vec![(queue, "0")], 16_384, 512, vec![queue]),
        (vec![(batch, "0")], 16_384, 512, vec![batch]),
        (vec![(queue, "2"), (batch, "0")], 2, 2, vec![batch]),
        (vec![(batch, "")], 16_384, 512, vec![]),
        (vec![(batch, "4096")], 16_384, 4096, vec![]),
    ] {
        let (config, invalid) = batch_config(&env(&vars));
        let config = format!("{config:?}");
        assert!(config.contains(&format!("max_queue_size: {queue_size},")));
        assert!(config.contains(&format!("max_export_batch_size: {batch_size},")));
        assert_eq!(invalid, expected_invalid, "{vars:?}");
    }
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
    let recorder = metrics_util::debugging::DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::with_local_recorder(&recorder, || {
        let export = |exporter: Fixed, timeout: Duration, spans: usize| {
            let _ = ObservedExporter {
                inner: exporter,
                timeout,
                drain: Arc::default(),
            }
            .export(Vec::from_fn(spans, |_| span()))
            .now_or_never()
            .expect("the fixed exporter is ready");
        };
        let budget = Duration::from_mins(1);
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
        export(
            Fixed(|| Err(OTelSdkError::InternalFailure("network error".into()))),
            Duration::ZERO,
            4,
        );
    });
    let snapshot = snapshotter.snapshot().into_vec();
    let metric = "telemetry_trace_exporter_sdk_result_spans_total";
    assert_eq!(snapshot.len(), 3, "{snapshot:?}");
    for (target, expected) in [
        (metrics::Key::from_parts(metric, &[("outcome", "ok")]), 3),
        (
            metrics::Key::from_parts(metric, &[("outcome", "error"), ("error_type", "timeout")]),
            6,
        ),
        (
            metrics::Key::from_parts(
                metric,
                &[("outcome", "error"), ("error_type", "internal_failure")],
            ),
            1,
        ),
    ] {
        let actual = snapshot
            .iter()
            .find_map(|(composite, _, _, value)| (composite.key() == &target).then_some(value));
        assert_eq!(
            actual,
            Some(&metrics_util::debugging::DebugValue::Counter(expected)),
            "{target}: {snapshot:?}"
        );
    }
}

#[test]
fn resource_carries_typed_identity_and_defaults_to_a_stable_process_uuid() {
    let mut options = options("");
    options.instance_id = Some("instance-1");
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

    let instance_key = opentelemetry::Key::new(attribute::SERVICE_INSTANCE_ID);
    options.instance_id = None;
    let generated = super::resource(&options).get(&instance_key).unwrap();
    let uuid = Uuid::parse_str(&generated.to_string()).unwrap();
    assert_eq!(uuid.get_version(), Some(uuid::Version::Random));
    options.instance_id = Some("  ");
    assert_eq!(
        super::resource(&options).get(&instance_key),
        Some(generated)
    );
}
