mod metrics;

use std::time::Duration;

use health::{Probe, ProbeError};
use operation_context::{Deadline, OperationContext};
use temporalio_client::{ConnectionOptions, Url};
use tokio_util::sync::CancellationToken;

pub use crate::metrics::{HISTOGRAMS, METRIC_PREFIX, WorkspaceMetricsMeter};

pub const PROBE_NAME: &str = "temporal";

#[must_use]
pub fn client_connection_options(
    target: Url,
    meter: &WorkspaceMetricsMeter,
    connect_timeout: Duration,
) -> ConnectionOptions {
    ConnectionOptions::new(target)
        .metrics_meter(meter.temporal_meter())
        .connect_timeout(connect_timeout)
        .build()
}

pub fn encode_bounded_json_str(
    value: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, json_payload::EncodeError> {
    json_payload::to_vec_bounded(value, max_bytes)
}

#[derive(Clone, Debug)]
pub struct BaselineProbe {
    draining: CancellationToken,
    budget: Duration,
}

impl BaselineProbe {
    #[must_use]
    pub const fn new(draining: CancellationToken, budget: Duration) -> Self {
        Self { draining, budget }
    }
}

impl Probe for BaselineProbe {
    fn name(&self) -> &'static str {
        PROBE_NAME
    }

    async fn check(&self) -> Result<(), ProbeError> {
        if self.draining.is_cancelled() {
            return Err(ProbeError::from_static("temporal adapter is draining"));
        }
        let context =
            OperationContext::new(Some(Deadline::after(self.budget)), self.draining.clone());
        context
            .run(tokio::task::yield_now())
            .await
            .map_err(|_| ProbeError::from_static("temporal readiness probe stopped"))
    }
}

#[cfg(feature = "worker")]
pub mod worker {
    use temporalio_sdk::runtime::RuntimeOptions;
    use temporalio_sdk::{Runtime, RuntimeError, Worker, WorkerRunError};
    use temporalio_sdk_core::CoreRuntime;
    use tokio::runtime::Handle;
    use tokio_util::sync::CancellationToken;

    use crate::WorkspaceMetricsMeter;

    pub fn build_runtime(meter: &WorkspaceMetricsMeter) -> Result<Runtime, RuntimeError> {
        let options = RuntimeOptions::builder()
            .telemetry_options(meter.telemetry_options())
            .heartbeat_interval(None)
            .build()
            .map_err(|err| RuntimeError::Initialization(err.into()))?;
        Runtime::from_current_tokio(options)
    }

    pub fn spawn_worker_thread(
        name: String,
        handle: Handle,
        worker: Worker,
        shutdown: CancellationToken,
    ) -> std::io::Result<std::thread::JoinHandle<Result<(), WorkerRunError>>> {
        const fn assert_send<T: Send>() {}
        assert_send::<Worker>();
        assert_send::<Runtime>();
        assert_send::<CoreRuntime>();

        std::thread::Builder::new()
            .name(name)
            .spawn(move || handle.block_on(worker.run_until(shutdown.cancelled())))
    }
}

#[cfg(test)]
mod tests {
    use metrics_util::MetricKind;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use temporalio_common::WorkflowDefinition;
    use temporalio_common::telemetry::metrics::{
        CoreMeter, MetricKeyValue, MetricParameters, NewAttributes,
    };
    use temporalio_macros::{workflow, workflow_methods};
    use temporalio_workflow::{WorkflowContext, WorkflowResult};

    use super::*;

    #[workflow]
    #[derive(Default)]
    struct ClientContractWorkflow;

    #[workflow_methods]
    impl ClientContractWorkflow {
        #[run]
        async fn run(ctx: &mut WorkflowContext<Self>, input: String) -> WorkflowResult<String> {
            ctx.timer(Duration::from_millis(1)).await;
            Ok(input)
        }
    }

    #[test]
    fn workflow_macro_expands_under_default_features() {
        assert_eq!(ClientContractWorkflow::run.name(), "ClientContractWorkflow");
    }

    #[test]
    fn core_meter_forwards_prometheus_attributes_with_single_prefix() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();

        ::metrics::with_local_recorder(&recorder, || {
            let meter = WorkspaceMetricsMeter::new(METRIC_PREFIX);
            let base = meter.new_attributes(NewAttributes::new(vec![MetricKeyValue::new(
                "task-queue",
                "orders",
            )]));
            let extended = meter.extend_attributes(
                base,
                NewAttributes::new(vec![MetricKeyValue::new("namespace", "default")]),
            );

            let counter = meter.counter(MetricParameters::from("request"));
            counter.add(2, &extended);

            let prefixed_counter = meter.counter(MetricParameters::from("temporal_request"));
            prefixed_counter.add(1, &extended);

            let duration = meter.histogram_duration(MetricParameters::from("request_latency"));
            duration.record(Duration::from_millis(250), &extended);

            let gauge = meter.gauge(MetricParameters::from("sticky_cache_size"));
            gauge.record(7, &extended);

            let up_down = meter.up_down_counter(MetricParameters::from("active_tasks"));
            up_down.add(3, &extended);
            up_down.add(-1, &extended);
        });

        let snapshot = snapshotter.snapshot().into_vec();
        let mut saw_counter = false;
        let mut saw_histogram = false;
        let mut saw_gauge = false;
        let mut saw_up_down = false;

        for (ckey, _, _, value) in snapshot {
            let name = ckey.key().name();
            assert!(
                !name.starts_with("temporal_temporal_"),
                "double prefix: {name}"
            );
            let labels: Vec<(&str, &str)> = ckey
                .key()
                .labels()
                .map(|label| (label.key(), label.value()))
                .collect();
            assert_eq!(
                labels,
                vec![("namespace", "default"), ("task_queue", "orders")]
            );
            match (ckey.kind(), name) {
                (MetricKind::Counter, "temporal_request") => {
                    assert_eq!(value, DebugValue::Counter(3));
                    saw_counter = true;
                }
                (MetricKind::Histogram, "temporal_request_latency") => {
                    assert_eq!(value, DebugValue::Histogram(vec![0.25.into()]));
                    saw_histogram = true;
                }
                (MetricKind::Gauge, "temporal_sticky_cache_size") => {
                    assert_eq!(value, DebugValue::Gauge(7.0.into()));
                    saw_gauge = true;
                }
                (MetricKind::Gauge, "temporal_active_tasks") => {
                    assert_eq!(value, DebugValue::Gauge(2.0.into()));
                    saw_up_down = true;
                }
                other => panic!("unexpected metric recorded: {other:?}"),
            }
        }

        assert!(saw_counter && saw_histogram && saw_gauge && saw_up_down);

        ::metrics::with_local_recorder(&recorder, || {
            let meter = WorkspaceMetricsMeter::new(METRIC_PREFIX);
            let attrs = meter.new_attributes(NewAttributes::new(vec![
                MetricKeyValue::new("task-queue", "orders"),
                MetricKeyValue::new("namespace", "default"),
            ]));
            let up_down = meter.up_down_counter(MetricParameters::from("active_tasks"));
            up_down.add(-4, &attrs);
        });

        let active_tasks =
            snapshotter
                .snapshot()
                .into_vec()
                .into_iter()
                .find_map(|(ckey, _, _, value)| {
                    (ckey.kind() == MetricKind::Gauge
                        && ckey.key().name() == "temporal_active_tasks")
                        .then_some(value)
                });
        assert_eq!(active_tasks, Some(DebugValue::Gauge((-4.0).into())));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn baseline_probe_and_json_encoder_operate_under_default_features() {
        let token = CancellationToken::new();
        let probe = BaselineProbe::new(token.clone(), Duration::from_secs(1));
        assert_eq!(probe.name(), PROBE_NAME);
        assert!(probe.check().await.is_ok());

        token.cancel();
        assert!(probe.check().await.is_err());

        let encoded = encode_bounded_json_str("ok", 16).expect("small string encodes");
        assert_eq!(encoded, br#""ok""#);
        assert!(encode_bounded_json_str("oversized-payload", 4).is_err());

        let meter = WorkspaceMetricsMeter::default();
        let opts = client_connection_options(
            Url::parse("https://temporal.example.invalid:7233").expect("valid url"),
            &meter,
            Duration::from_secs(5),
        );
        assert_eq!(opts.connect_timeout, Some(Duration::from_secs(5)));
    }
}

#[cfg(all(test, feature = "worker"))]
mod worker_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use futures::FutureExt;
    use metrics_util::MetricKind;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use temporalio_client::callback_based::{
        CallbackBasedGrpcService, GrpcRequest, GrpcSuccessResponse,
    };
    use temporalio_client::{Client, ClientOptions, Connection, ConnectionOptions, Url};
    use temporalio_common::telemetry::TaskQueueLabelStrategy;
    use temporalio_common::worker::{WorkerDeploymentOptions, WorkerDeploymentVersion};
    use temporalio_macros::{activities, workflow, workflow_methods};
    use temporalio_sdk::activities::{ActivityContext, ActivityError};
    use temporalio_sdk::runtime::worker_tuner::{ResourceBasedTuner, ResourceBasedTunerOptions};
    use temporalio_sdk::{ActivityOptions, Worker, WorkerOptions, WorkflowContext, WorkflowResult};
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    use super::worker::{build_runtime, spawn_worker_thread};
    use super::{METRIC_PREFIX, WorkspaceMetricsMeter};

    #[workflow]
    #[derive(Default)]
    struct SmokeWorkflow;

    #[workflow_methods]
    impl SmokeWorkflow {
        #[run]
        async fn run(ctx: &mut WorkflowContext<Self>, input: String) -> WorkflowResult<String> {
            let output = ctx
                .execute_activity(
                    SmokeActivities::echo,
                    input,
                    ActivityOptions::start_to_close_timeout(Duration::from_secs(10)),
                )
                .await?;
            Ok(output)
        }
    }

    struct SmokeActivities;

    #[activities]
    impl SmokeActivities {
        #[activity]
        async fn echo(_ctx: ActivityContext, input: String) -> Result<String, ActivityError> {
            tokio::task::yield_now().await;
            Ok(input)
        }
    }

    fn assert_send<T: Send>() {}

    fn callback_grpc_service(
        shutdown: CancellationToken,
        poll_started: Option<Arc<Notify>>,
    ) -> CallbackBasedGrpcService {
        CallbackBasedGrpcService {
            callback: Arc::new(move |mut req: GrpcRequest| {
                let shutdown = shutdown.clone();
                let poll_started = poll_started.clone();
                async move {
                    if req.rpc.starts_with("Poll") {
                        if let Some(notify) = poll_started {
                            notify.notify_one();
                        }
                        shutdown.cancelled().await;
                    }
                    req.headers.clear();
                    Ok(GrpcSuccessResponse {
                        headers: req.headers,
                        proto: Vec::new(),
                    })
                }
                .boxed()
            }),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_is_send_and_runs_on_os_thread_with_handle_block_on_and_resource_tuner() {
        assert_send::<Worker>();

        let shutdown = CancellationToken::new();
        let poll_started = Arc::new(Notify::new());
        let callback = callback_grpc_service(shutdown.clone(), Some(Arc::clone(&poll_started)));

        let meter = WorkspaceMetricsMeter::new(METRIC_PREFIX);
        let runtime = build_runtime(&meter).expect("CoreRuntime attaches to current Tokio pool");

        let conn_opts = ConnectionOptions::new(
            Url::parse("http://127.0.0.1:7233").expect("valid loopback url"),
        )
        .dns_load_balancing(None)
        .service_override(callback)
        .metrics_meter(meter.temporal_meter())
        .build();
        let connection = Connection::connect(conn_opts)
            .await
            .expect("callback connection succeeds");
        let client = Client::new(connection, ClientOptions::new("default").build())
            .expect("client constructs");

        let tuner = ResourceBasedTuner::builder()
            .tuner_options(
                ResourceBasedTunerOptions::builder()
                    .target_memory_usage(0.75)
                    .target_cpu_usage(0.80)
                    .build(),
            )
            .build();
        let deployment = WorkerDeploymentOptions::new(
            WorkerDeploymentVersion::builder()
                .deployment_name("service-temporal-worker")
                .build_id("0.1.0-test")
                .build(),
        )
        .build();
        let worker_opts = WorkerOptions::new("orders")
            .deployment_options(deployment)
            .tuner(tuner)
            .graceful_shutdown_period(Duration::from_secs(1))
            .register_workflow::<SmokeWorkflow>()
            .expect("workflow registers")
            .register_activities(SmokeActivities)
            .build();

        let worker = Worker::new(&runtime, client, worker_opts).expect("worker initializes");
        let join = spawn_worker_thread(
            "temporal-wf-0".to_owned(),
            tokio::runtime::Handle::current(),
            worker,
            shutdown.clone(),
        )
        .expect("worker OS thread spawns");

        tokio::time::timeout(Duration::from_secs(5), poll_started.notified())
            .await
            .expect("worker OS thread did not enter gRPC polling before timeout");
        shutdown.cancel();

        let result = tokio::task::spawn_blocking(move || join.join())
            .await
            .expect("join task completes")
            .expect("worker OS thread does not panic");
        assert!(
            result.is_ok(),
            "worker.run_until failed under Handle::block_on: {result:?}"
        );
    }

    #[tokio::test]
    async fn core_telemetry_options_preserve_single_prefix_through_worker_init() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();

        let meter = WorkspaceMetricsMeter::new(METRIC_PREFIX);
        let opts = meter.telemetry_options();
        assert_eq!(opts.metric_prefix, "");
        assert!(!opts.attach_service_name);
        assert!(opts.metrics.is_some());
        assert!(matches!(
            opts.task_queue_label_strategy,
            TaskQueueLabelStrategy::UseNormal
        ));
        assert!(matches!(
            meter.temporal_meter().get_task_queue_label_strategy(),
            TaskQueueLabelStrategy::UseNormal
        ));

        let runtime = build_runtime(&meter).expect("CoreRuntime initializes with CoreMeter");
        let shutdown = CancellationToken::new();
        let callback = callback_grpc_service(shutdown.clone(), None);

        let conn_opts = ConnectionOptions::new(
            Url::parse("http://127.0.0.1:7233").expect("valid loopback url"),
        )
        .dns_load_balancing(None)
        .service_override(callback)
        .build();

        let connection = Connection::connect(conn_opts)
            .await
            .expect("callback connection succeeds");
        let client = Client::new(connection, ClientOptions::new("default").build())
            .expect("client constructs");
        let deployment = WorkerDeploymentOptions::from_build_id("0.1.0-test".to_owned());
        let worker_opts = WorkerOptions::new("orders")
            .deployment_options(deployment)
            .graceful_shutdown_period(Duration::from_secs(1))
            .register_workflow::<SmokeWorkflow>()
            .expect("workflow registers")
            .register_activities(SmokeActivities)
            .build();
        let worker = ::metrics::with_local_recorder(&recorder, || {
            Worker::new(&runtime, client, worker_opts)
        })
        .expect("worker initializes");

        shutdown.cancel();
        let join = spawn_worker_thread(
            "temporal-wf-metrics".to_owned(),
            tokio::runtime::Handle::current(),
            worker,
            shutdown,
        )
        .expect("worker OS thread spawns");
        let result = tokio::task::spawn_blocking(move || join.join())
            .await
            .expect("join task completes")
            .expect("worker OS thread does not panic");
        assert!(result.is_ok(), "worker shutdown failed: {result:?}");

        let snapshot = snapshotter.snapshot().into_vec();
        assert!(
            !snapshot.is_empty(),
            "expected CoreMeter to record Core worker initialization metrics"
        );
        let mut saw_worker_start = false;
        for (ckey, _, _, value) in snapshot {
            let name = ckey.key().name();
            assert!(
                name.starts_with(METRIC_PREFIX),
                "missing temporal_ prefix: {name}"
            );
            assert!(
                !name.starts_with("temporal_temporal_"),
                "double temporal_ prefix: {name}"
            );
            let labels: Vec<(&str, &str)> = ckey
                .key()
                .labels()
                .map(|label| (label.key(), label.value()))
                .collect();
            assert!(
                !labels.iter().any(|(key, _)| *key == "service_name"),
                "unexpected service_name label on {name}: {labels:?}"
            );
            if (ckey.kind(), name) == (MetricKind::Counter, "temporal_worker_start") {
                assert_eq!(value, DebugValue::Counter(1));
                assert_eq!(
                    labels,
                    vec![("namespace", "default"), ("task_queue", "orders")]
                );
                saw_worker_start = true;
            }
        }
        assert!(
            saw_worker_start,
            "expected Core worker initialization to record temporal_worker_start"
        );
    }
}
