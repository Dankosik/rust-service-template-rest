use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::FutureExt as _;
use health::Probe as _;
use metrics_util::MetricKind;
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use opentelemetry::global;
use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::trace::{SpanId, TraceId, TracerProvider as _};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use operation_context::{OperationContext, Stopped};
use prost::Message as _;
use temporalio_client::callback_based::{
    CallbackBasedGrpcService, GrpcRequest, GrpcSuccessResponse,
};
use temporalio_common::data_converters::{
    GenericPayloadConverter as _, SerializationContext, SerializationContextData,
    WorkflowSerializationContext,
};
use temporalio_common::protos::temporal::api::command::v1::command::Attributes as CommandAttributes;
use temporalio_common::protos::temporal::api::common::v1::{
    ActivityType, Header, Payload, Payloads, WorkflowExecution, WorkflowType,
};
use temporalio_common::protos::temporal::api::enums::v1::{EventType, TaskQueueKind};
use temporalio_common::protos::temporal::api::history::v1::{
    History, HistoryEvent, WorkflowExecutionStartedEventAttributes,
    WorkflowTaskScheduledEventAttributes, WorkflowTaskStartedEventAttributes, history_event,
};
use temporalio_common::protos::temporal::api::taskqueue::v1::TaskQueue;
use temporalio_common::protos::temporal::api::workflowservice::v1::{
    DescribeNamespaceResponse, PollActivityTaskQueueRequest, PollActivityTaskQueueResponse,
    PollWorkflowTaskQueueRequest, PollWorkflowTaskQueueResponse,
    RecordActivityTaskHeartbeatResponse, RespondActivityTaskCompletedResponse,
    RespondWorkflowTaskCompletedRequest, RespondWorkflowTaskCompletedResponse,
};
use temporalio_common::protos::{ENCODING_PAYLOAD_KEY, JSON_ENCODING_VAL};
use temporalio_macros::{activities, workflow, workflow_methods};
use temporalio_sdk::activities::{ActivityContext, ActivityError};
use temporalio_sdk::{ActivityOptions, WorkflowContext, WorkflowResult};
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tonic::Status;
use tracing_subscriber::layer::SubscriberExt as _;
use url::Url;

use super::interceptors::deadline_from_system_time;
use super::{
    Preempted, PreparedTemporalWorker, RegistryError, TaskQueueOverrides, TemporalWorkerOptions,
    VersioningBehaviorMode, WorkerDeploymentMetadata, WorkerFatalError, WorkerPrepareError,
    WorkerRegistry, WorkerTunerOptions, WorkflowRegistrationError, activity_operation_context,
    run_with_preemption,
};
use crate::payload::build_data_converter;
use crate::trace::TRACER_DATA_HEADER;
use crate::{CloseOutcome, TemporalAuth, TemporalConnection, TemporalOptions, TemporalStartup};

#[workflow]
#[derive(Default)]
struct EchoWorkflow;

#[workflow_methods]
impl EchoWorkflow {
    #[run]
    async fn run(ctx: &mut WorkflowContext<Self>, input: String) -> WorkflowResult<String> {
        let result = ctx
            .execute_activity(
                EchoActivities::echo,
                input,
                ActivityOptions::start_to_close_timeout(Duration::from_secs(10)),
            )
            .await?;
        Ok(result)
    }
}

struct EchoActivities;

#[activities]
impl EchoActivities {
    #[activity]
    async fn echo(_ctx: ActivityContext, input: String) -> Result<String, ActivityError> {
        tokio::task::yield_now().await;
        Ok(input)
    }
}

struct BlockingActivities {
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    observed_deadline: Arc<AtomicBool>,
    observed_cancelled: Arc<Notify>,
}

#[activities]
impl BlockingActivities {
    #[activity]
    async fn block(
        self: Arc<Self>,
        ctx: ActivityContext,
        _input: String,
    ) -> Result<String, ActivityError> {
        let op = activity_operation_context(&ctx);
        if op.deadline().is_some() {
            self.observed_deadline.store(true, Ordering::Release);
        }
        let dropped = Arc::clone(&self.dropped);
        let _guard = std::mem::DropGuard::new((), move |()| {
            dropped.store(true, Ordering::Release);
        });
        self.started.notify_one();
        let stopped = op.wait_stopped().await;
        if stopped == Stopped::Cancelled {
            self.observed_cancelled.notify_one();
            return Err(ActivityError::cancelled());
        }
        Ok("finished".to_owned())
    }
}

fn base_client_options() -> TemporalOptions {
    TemporalOptions {
        endpoint: Url::parse("http://127.0.0.1:7233").unwrap(),
        namespace: "default".to_owned(),
        identity: "worker-test@0.1.0".to_owned(),
        auth: TemporalAuth::None,
        tls: None,
        allow_plaintext: true,
        allow_unauthenticated: true,
        trusted_network: false,
        credential_refresh_interval: Duration::from_secs(30),
        connect_timeout: Duration::from_secs(5),
        rpc_timeout: Duration::from_secs(5),
        keep_alive_interval: Duration::from_secs(30),
        keep_alive_timeout: Duration::from_secs(15),
        dns_load_balancing: false,
        http_proxy_url: None,
        max_payload_bytes: 64 * 1024,
        probe_budget: Duration::from_secs(1),
        payload_codec: None,
    }
}

fn base_worker_options() -> TemporalWorkerOptions {
    TemporalWorkerOptions {
        task_queue: Some("orders".to_owned()),
        queues: BTreeMap::new(),
        max_cached_workflows: 100,
        workflow_pollers: None,
        activity_pollers: None,
        max_worker_activities_per_second: None,
        tuner: WorkerTunerOptions::ResourceBased {
            target_memory: 0.75,
            target_cpu: 0.80,
        },
        graceful_shutdown_period: Duration::from_secs(5),
    }
}

fn base_deployment() -> WorkerDeploymentMetadata {
    WorkerDeploymentMetadata {
        deployment_name: "service-temporal-worker".to_owned(),
        build_id: "0.1.0-test".to_owned(),
        use_worker_versioning: false,
        default_versioning_behavior: VersioningBehaviorMode::Unspecified,
    }
}

fn idle_callback(shutdown: CancellationToken) -> CallbackBasedGrpcService {
    CallbackBasedGrpcService {
        callback: Arc::new(move |mut req: GrpcRequest| {
            let shutdown = shutdown.clone();
            async move {
                if req.rpc.starts_with("Poll") {
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

async fn admit_connection(
    callback: CallbackBasedGrpcService,
) -> (TemporalStartup, TemporalConnection) {
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));
    let mut startup =
        TemporalStartup::prepare(base_client_options(), &ctx).with_service_override(callback);
    let conn = startup
        .admit(CancellationToken::new())
        .await
        .expect("callback startup succeeds");
    (startup, conn)
}

fn json_string_payload(value: &str) -> Payload {
    Payload {
        metadata: HashMap::from([(
            ENCODING_PAYLOAD_KEY.to_owned(),
            JSON_ENCODING_VAL.as_bytes().to_vec(),
        )]),
        data: serde_json::to_vec(value).unwrap(),
        external_payloads: Vec::new(),
    }
}

#[test]
fn registry_rejects_blank_control_chars_and_duplicate_queues() {
    let mut registry = WorkerRegistry::new();
    for blank in ["", "   ", "\t\n"] {
        assert_eq!(
            registry.queue(blank).unwrap_err(),
            RegistryError::BlankQueueName
        );
    }
    for control in ["orders\n", "orders\0", "\x07"] {
        assert_eq!(
            registry.queue(control).unwrap_err(),
            RegistryError::ControlCharsInQueueName
        );
    }

    registry
        .default_queue()
        .unwrap()
        .register_workflow::<EchoWorkflow>()
        .unwrap();
    assert_eq!(
        registry.default_queue().unwrap_err(),
        RegistryError::DuplicateDefaultQueue
    );

    let queue = registry.queue("billing").unwrap();
    queue.register_workflow::<EchoWorkflow>().unwrap();
    std::assert_matches!(
        queue.register_workflow::<EchoWorkflow>(),
        Err(WorkflowRegistrationError::DuplicateWorkflowType { .. })
    );
    assert_eq!(
        registry.queue("billing").unwrap_err(),
        RegistryError::DuplicateQueue("billing".to_owned())
    );
}

fn single_echo_workflow_registry() -> WorkerRegistry {
    let mut reg = WorkerRegistry::new();
    reg.default_queue()
        .unwrap()
        .register_workflow::<EchoWorkflow>()
        .unwrap();
    reg
}

#[tokio::test]
async fn prepared_worker_validates_registry_queues_and_configuration_bounds() {
    let shutdown = CancellationToken::new();
    let (startup, conn) = admit_connection(idle_callback(shutdown)).await;
    let opts = base_worker_options();
    let deploy = base_deployment();

    assert_eq!(
        PreparedTemporalWorker::prepare(&conn, &opts, deploy.clone(), WorkerRegistry::new())
            .unwrap_err(),
        WorkerPrepareError::NoHandlersRegistered
    );

    let mut empty_default = WorkerRegistry::new();
    empty_default.default_queue().unwrap();
    assert_eq!(
        PreparedTemporalWorker::prepare(&conn, &opts, deploy.clone(), empty_default).unwrap_err(),
        WorkerPrepareError::NoHandlersRegistered
    );

    let mut missing_default_cfg = opts.clone();
    missing_default_cfg.task_queue = None;
    assert_eq!(
        PreparedTemporalWorker::prepare(
            &conn,
            &missing_default_cfg,
            deploy.clone(),
            single_echo_workflow_registry(),
        )
        .unwrap_err(),
        WorkerPrepareError::MissingDefaultTaskQueue
    );

    let mut reg_collision = single_echo_workflow_registry();
    reg_collision
        .queue("orders")
        .unwrap()
        .register_activities(EchoActivities);
    assert_eq!(
        PreparedTemporalWorker::prepare(&conn, &opts, deploy.clone(), reg_collision).unwrap_err(),
        WorkerPrepareError::QueueCollidesWithDefault("orders".to_owned())
    );

    let mut override_collision = opts.clone();
    override_collision
        .queues
        .insert("orders".to_owned(), TaskQueueOverrides::default());
    assert_eq!(
        PreparedTemporalWorker::prepare(
            &conn,
            &override_collision,
            deploy.clone(),
            single_echo_workflow_registry(),
        )
        .unwrap_err(),
        WorkerPrepareError::QueueCollidesWithDefault("orders".to_owned())
    );

    let mut unmatched_cfg = opts.clone();
    unmatched_cfg
        .queues
        .insert("typo".to_owned(), TaskQueueOverrides::default());
    unmatched_cfg
        .queues
        .insert("extra".to_owned(), TaskQueueOverrides::default());
    let mut reg_named = WorkerRegistry::new();
    reg_named
        .queue("billing")
        .unwrap()
        .register_activities(EchoActivities);
    assert_eq!(
        PreparedTemporalWorker::prepare(&conn, &unmatched_cfg, deploy.clone(), reg_named)
            .unwrap_err(),
        WorkerPrepareError::UnmatchedQueueOverrides(vec!["extra".to_owned(), "typo".to_owned()])
    );

    assert_invalid_worker_configurations(&conn, &opts, &deploy);

    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

fn assert_invalid_worker_configurations(
    conn: &TemporalConnection,
    opts: &TemporalWorkerOptions,
    deploy: &WorkerDeploymentMetadata,
) {
    let mut bad_versioning = deploy.clone();
    bad_versioning.use_worker_versioning = true;
    bad_versioning.default_versioning_behavior = VersioningBehaviorMode::Unspecified;
    std::assert_matches!(
        PreparedTemporalWorker::prepare(
            conn,
            opts,
            bad_versioning,
            single_echo_workflow_registry()
        ),
        Err(WorkerPrepareError::InvalidConfiguration(_))
    );

    let mut bad_sticky = opts.clone();
    bad_sticky.max_cached_workflows = 10;
    bad_sticky.tuner = WorkerTunerOptions::FixedSize {
        max_concurrent_workflow_tasks: 1,
        max_concurrent_activities: 4,
        max_concurrent_local_activities: 4,
    };
    std::assert_matches!(
        PreparedTemporalWorker::prepare(
            conn,
            &bad_sticky,
            deploy.clone(),
            single_echo_workflow_registry(),
        ),
        Err(WorkerPrepareError::InvalidConfiguration(_))
    );

    let mut bad_zero_shutdown = opts.clone();
    bad_zero_shutdown.graceful_shutdown_period = Duration::ZERO;
    std::assert_matches!(
        PreparedTemporalWorker::prepare(
            conn,
            &bad_zero_shutdown,
            deploy.clone(),
            single_echo_workflow_registry(),
        ),
        Err(WorkerPrepareError::InvalidConfiguration(_))
    );
}

#[tokio::test]
async fn run_with_preemption_returns_ok_or_preempted_and_drops_pending_future() {
    let token = CancellationToken::new();
    assert_eq!(run_with_preemption(&token, async { 42 }).await, Ok(42));

    let pre_cancelled = CancellationToken::new();
    pre_cancelled.cancel();
    assert_eq!(
        run_with_preemption(&pre_cancelled, std::future::ready(99)).await,
        Err(Preempted)
    );

    let live = CancellationToken::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(Notify::new());
    let dropped_flag = Arc::clone(&dropped);
    let entered_notify = Arc::clone(&entered);
    let task_token = live.clone();

    let handle = tokio::spawn(async move {
        run_with_preemption(&task_token, async move {
            let _guard = std::mem::DropGuard::new((), move |()| {
                dropped_flag.store(true, Ordering::Release);
            });
            entered_notify.notify_one();
            std::future::pending::<()>().await;
        })
        .await
    });

    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    live.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap(),
        Err(Preempted)
    );
    assert!(dropped.load(Ordering::Acquire));
}

struct MultiQueuePollSpy {
    shutdown: CancellationToken,
    orders_polled: Notify,
    billing_polled: Notify,
    seen_orders: AtomicBool,
    seen_billing: AtomicBool,
}

impl MultiQueuePollSpy {
    fn new(shutdown: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            shutdown,
            orders_polled: Notify::new(),
            billing_polled: Notify::new(),
            seen_orders: AtomicBool::new(false),
            seen_billing: AtomicBool::new(false),
        })
    }

    fn record_queue(&self, queue: &str) {
        if queue == "orders" && !self.seen_orders.swap(true, Ordering::AcqRel) {
            self.orders_polled.notify_one();
        }
        if queue == "billing" && !self.seen_billing.swap(true, Ordering::AcqRel) {
            self.billing_polled.notify_one();
        }
    }

    fn service(self: &Arc<Self>) -> CallbackBasedGrpcService {
        let spy = Arc::clone(self);
        CallbackBasedGrpcService {
            callback: Arc::new(move |mut req: GrpcRequest| {
                let spy = Arc::clone(&spy);
                async move {
                    if req.rpc == "PollWorkflowTaskQueue" {
                        let parsed = PollWorkflowTaskQueueRequest::decode(req.proto.as_ref())
                            .unwrap_or_default();
                        spy.record_queue(&parsed.task_queue.map_or_default(|q| q.name));
                        spy.shutdown.cancelled().await;
                    } else if req.rpc == "PollActivityTaskQueue" {
                        let parsed = PollActivityTaskQueueRequest::decode(req.proto.as_ref())
                            .unwrap_or_default();
                        spy.record_queue(&parsed.task_queue.map_or_default(|q| q.name));
                        spy.shutdown.cancelled().await;
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_queue_worker_polls_both_queues_drains_and_finishes_cleanly() {
    let shutdown = CancellationToken::new();
    let spy = MultiQueuePollSpy::new(shutdown.clone());
    let (startup, conn) = admit_connection(spy.service()).await;
    let mut opts = base_worker_options();
    opts.queues.insert(
        "billing".to_owned(),
        TaskQueueOverrides {
            max_cached_workflows: Some(50),
            workflow_pollers: NonZeroU32::new(2),
            activity_pollers: NonZeroU32::new(2),
            max_worker_activities_per_second: Some(50.0),
            tuner: Some(WorkerTunerOptions::FixedSize {
                max_concurrent_workflow_tasks: 4,
                max_concurrent_activities: 4,
                max_concurrent_local_activities: 4,
            }),
        },
    );

    let mut registry = single_echo_workflow_registry();
    registry
        .queue("billing")
        .unwrap()
        .register_activities(EchoActivities);

    let prepared = PreparedTemporalWorker::prepare(&conn, &opts, base_deployment(), registry)
        .expect("multi-queue worker prepares");
    let mut handle = prepared
        .start(tokio::runtime::Handle::current())
        .expect("multi-queue worker starts");

    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(spy.orders_polled.notified(), spy.billing_polled.notified());
    })
    .await
    .expect("both queues entered polling");

    handle.initiate_shutdown();
    shutdown.cancel();

    tokio::time::timeout(Duration::from_secs(5), handle.drained())
        .await
        .expect("multi-queue worker drains");
    assert!(!handle.has_failed());
    assert_eq!(
        handle.finish(Instant::now() + Duration::from_secs(5)).await,
        Ok(CloseOutcome::Complete)
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_fatal_validation_failure_marks_probe_unhealthy_and_wakes_wait_failed() {
    let describe_calls = Arc::new(AtomicUsize::new(0));
    let calls_cb = Arc::clone(&describe_calls);
    let callback = CallbackBasedGrpcService {
        callback: Arc::new(move |mut req: GrpcRequest| {
            let calls_cb = Arc::clone(&calls_cb);
            async move {
                req.headers.clear();
                if req.rpc == "DescribeNamespace" {
                    calls_cb.fetch_add(1, Ordering::AcqRel);
                    return Err(Status::not_found("namespace does not exist"));
                }
                Ok(GrpcSuccessResponse {
                    headers: req.headers,
                    proto: Vec::new(),
                })
            }
            .boxed()
        }),
    };

    let (startup, conn) = admit_connection(callback).await;
    let prepared = PreparedTemporalWorker::prepare(
        &conn,
        &base_worker_options(),
        base_deployment(),
        single_echo_workflow_registry(),
    )
    .unwrap();
    let probe = prepared.probe();
    let mut handle = prepared.start(tokio::runtime::Handle::current()).unwrap();

    let fatal = tokio::time::timeout(Duration::from_secs(5), handle.wait_failed())
        .await
        .expect("fatal error wakes wait_failed");
    assert_eq!(fatal, WorkerFatalError::Fatal);
    assert!(handle.has_failed());

    let calls_before_probe = describe_calls.load(Ordering::Acquire);
    assert!(probe.check().await.is_err());
    assert_eq!(
        describe_calls.load(Ordering::Acquire),
        calls_before_probe,
        "failed worker flag must short-circuit readiness probe locally"
    );

    assert_eq!(
        handle.finish(Instant::now() + Duration::from_secs(5)).await,
        Err(WorkerFatalError::Fatal)
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

struct BlockingActivityRpcMock {
    delivered: AtomicBool,
    cancel_rpcs: AtomicUsize,
    complete_rpcs: AtomicUsize,
    fail_rpcs: AtomicUsize,
    poll_release: CancellationToken,
}

impl BlockingActivityRpcMock {
    fn new(poll_release: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            delivered: AtomicBool::new(false),
            cancel_rpcs: AtomicUsize::new(0),
            complete_rpcs: AtomicUsize::new(0),
            fail_rpcs: AtomicUsize::new(0),
            poll_release,
        })
    }

    async fn handle(&self, mut req: GrpcRequest) -> Result<GrpcSuccessResponse, Status> {
        req.headers.clear();
        let proto = match req.rpc.as_str() {
            "GetSystemInfo" | "DescribeNamespace" => {
                DescribeNamespaceResponse::default().encode_to_vec()
            }
            "PollActivityTaskQueue" => self.poll_activity().await,
            "RespondActivityTaskCanceled" => {
                self.cancel_rpcs.fetch_add(1, Ordering::AcqRel);
                Vec::new()
            }
            "RespondActivityTaskCompleted" => {
                self.complete_rpcs.fetch_add(1, Ordering::AcqRel);
                RespondActivityTaskCompletedResponse::default().encode_to_vec()
            }
            "RespondActivityTaskFailed" => {
                self.fail_rpcs.fetch_add(1, Ordering::AcqRel);
                Vec::new()
            }
            "RecordActivityTaskHeartbeat" => {
                RecordActivityTaskHeartbeatResponse::default().encode_to_vec()
            }
            _ => Vec::new(),
        };
        Ok(GrpcSuccessResponse {
            headers: req.headers,
            proto,
        })
    }

    async fn poll_activity(&self) -> Vec<u8> {
        if self.delivered.swap(true, Ordering::AcqRel) {
            self.poll_release.cancelled().await;
            return Vec::new();
        }
        let now = SystemTime::now().into();
        PollActivityTaskQueueResponse {
            task_token: b"act-token-1".to_vec(),
            workflow_execution: Some(WorkflowExecution {
                workflow_id: "wf-1".to_owned(),
                run_id: "run-1".to_owned(),
            }),
            activity_id: "1".to_owned(),
            activity_type: Some(ActivityType {
                name: BlockingActivities::block.name().to_owned(),
            }),
            input: Some(Payloads {
                payloads: vec![json_string_payload("job")],
            }),
            scheduled_time: Some(now),
            started_time: Some(now),
            start_to_close_timeout: Duration::from_secs(60).try_into().ok(),
            schedule_to_close_timeout: Duration::from_secs(120).try_into().ok(),
            attempt: 1,
            ..Default::default()
        }
        .encode_to_vec()
    }

    fn service(self: &Arc<Self>) -> CallbackBasedGrpcService {
        let mock = Arc::clone(self);
        CallbackBasedGrpcService {
            callback: Arc::new(move |req: GrpcRequest| {
                let mock = Arc::clone(&mock);
                async move { mock.handle(req).await }.boxed()
            }),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_cancel_preempts_in_flight_activity_via_will_complete_async_without_cancel_rpc() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let observed_deadline = Arc::new(AtomicBool::new(false));
    let observed_cancelled = Arc::new(Notify::new());
    let poll_release = CancellationToken::new();
    let rpc = BlockingActivityRpcMock::new(poll_release.clone());

    let (startup, conn) = admit_connection(rpc.service()).await;
    let mut opts = base_worker_options();
    opts.graceful_shutdown_period = Duration::from_secs(45);

    let mut registry = WorkerRegistry::new();
    registry
        .default_queue()
        .unwrap()
        .register_activities(BlockingActivities {
            started: Arc::clone(&started),
            dropped: Arc::clone(&dropped),
            observed_deadline: Arc::clone(&observed_deadline),
            observed_cancelled: Arc::clone(&observed_cancelled),
        });

    let prepared = PreparedTemporalWorker::prepare(&conn, &opts, base_deployment(), registry)
        .expect("worker prepares");
    let mut handle = prepared.start(tokio::runtime::Handle::current()).unwrap();

    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("activity started");
    assert!(observed_deadline.load(Ordering::Acquire));

    let force_start = Instant::now();
    ::metrics::with_local_recorder(&recorder, || {
        handle.force_cancel();
        handle.force_cancel();
    });
    poll_release.cancel();

    let finish_result = handle.finish(Instant::now() + Duration::from_secs(5)).await;
    assert_eq!(
        finish_result,
        Ok(CloseOutcome::Complete),
        "forced preemption must release Core slots and drop ActivityContext cleanly"
    );
    assert!(
        force_start.elapsed() < Duration::from_secs(5),
        "force_cancel must bypass graceful_shutdown_period"
    );
    assert!(dropped.load(Ordering::Acquire));
    assert_eq!(
        rpc.cancel_rpcs.load(Ordering::Acquire),
        0,
        "forced preemption must never send RespondActivityTaskCanceled"
    );
    assert_eq!(rpc.complete_rpcs.load(Ordering::Acquire), 0);
    assert_eq!(rpc.fail_rpcs.load(Ordering::Acquire), 0);

    let snapshot = snapshotter.snapshot().into_vec();
    let forced_count = snapshot.iter().find_map(|(ckey, _, _, value)| {
        (ckey.kind() == MetricKind::Counter && ckey.key().name() == "temporal_drain_forced_total")
            .then_some(value)
    });
    assert_eq!(forced_count, Some(&DebugValue::Counter(1)));

    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

struct TraceWorkflowRpcMock {
    start_tracer_payload: Payload,
    wf_delivered: AtomicBool,
    act_delivered: AtomicBool,
    act_ready: Notify,
    wf_completed: Notify,
    act_completed: Notify,
    captured_activity_header: Mutex<Option<Header>>,
    poll_release: CancellationToken,
}

impl TraceWorkflowRpcMock {
    fn new(start_tracer_payload: Payload, poll_release: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            start_tracer_payload,
            wf_delivered: AtomicBool::new(false),
            act_delivered: AtomicBool::new(false),
            act_ready: Notify::new(),
            wf_completed: Notify::new(),
            act_completed: Notify::new(),
            captured_activity_header: Mutex::new(None),
            poll_release,
        })
    }

    async fn handle(&self, mut req: GrpcRequest) -> Result<GrpcSuccessResponse, Status> {
        req.headers.clear();
        let proto = match req.rpc.as_str() {
            "GetSystemInfo" | "DescribeNamespace" => {
                DescribeNamespaceResponse::default().encode_to_vec()
            }
            "PollWorkflowTaskQueue" => self.poll_workflow().await,
            "RespondWorkflowTaskCompleted" => self.complete_workflow(&req.proto),
            "PollActivityTaskQueue" => self.poll_activity().await,
            "RespondActivityTaskCompleted" => {
                self.act_completed.notify_one();
                RespondActivityTaskCompletedResponse::default().encode_to_vec()
            }
            _ => Vec::new(),
        };
        Ok(GrpcSuccessResponse {
            headers: req.headers,
            proto,
        })
    }

    async fn poll_workflow(&self) -> Vec<u8> {
        if self.wf_delivered.swap(true, Ordering::AcqRel) {
            self.poll_release.cancelled().await;
            return Vec::new();
        }
        trace_workflow_poll_response(self.start_tracer_payload.clone()).encode_to_vec()
    }

    fn complete_workflow(&self, raw: &[u8]) -> Vec<u8> {
        let completed = RespondWorkflowTaskCompletedRequest::decode(raw).unwrap();
        for cmd in completed.commands {
            if let Some(CommandAttributes::ScheduleActivityTaskCommandAttributes(attrs)) =
                cmd.attributes
            {
                *self.captured_activity_header.lock().unwrap() = attrs.header;
                self.act_ready.notify_one();
            }
        }
        self.wf_completed.notify_one();
        RespondWorkflowTaskCompletedResponse::default().encode_to_vec()
    }

    async fn poll_activity(&self) -> Vec<u8> {
        if self.act_delivered.swap(true, Ordering::AcqRel) {
            self.poll_release.cancelled().await;
            return Vec::new();
        }
        self.act_ready.notified().await;
        let header = self.captured_activity_header.lock().unwrap().clone();
        PollActivityTaskQueueResponse {
            task_token: b"act-trace-1".to_vec(),
            workflow_execution: Some(WorkflowExecution {
                workflow_id: "wf-trace-1".to_owned(),
                run_id: "run-trace-1".to_owned(),
            }),
            activity_id: "1".to_owned(),
            activity_type: Some(ActivityType {
                name: EchoActivities::echo.name().to_owned(),
            }),
            input: Some(Payloads {
                payloads: vec![json_string_payload("ping")],
            }),
            header,
            attempt: 1,
            ..Default::default()
        }
        .encode_to_vec()
    }

    fn service(self: &Arc<Self>) -> CallbackBasedGrpcService {
        let mock = Arc::clone(self);
        CallbackBasedGrpcService {
            callback: Arc::new(move |req: GrpcRequest| {
                let mock = Arc::clone(&mock);
                async move { mock.handle(req).await }.boxed()
            }),
        }
    }
}

fn trace_workflow_poll_response(start_tracer_payload: Payload) -> PollWorkflowTaskQueueResponse {
    let now = SystemTime::now().into();
    let task_queue = TaskQueue {
        name: "orders".to_owned(),
        kind: TaskQueueKind::Normal as i32,
        normal_name: String::new(),
    };
    PollWorkflowTaskQueueResponse {
        task_token: b"wf-token-1".to_vec(),
        workflow_execution: Some(WorkflowExecution {
            workflow_id: "wf-trace-1".to_owned(),
            run_id: "run-trace-1".to_owned(),
        }),
        workflow_type: Some(WorkflowType {
            name: "EchoWorkflow".to_owned(),
        }),
        workflow_execution_task_queue: Some(task_queue.clone()),
        started_event_id: 3,
        started_time: Some(now),
        attempt: 1,
        history: Some(History {
            events: vec![
                HistoryEvent {
                    event_id: 1,
                    event_time: Some(now),
                    event_type: EventType::WorkflowExecutionStarted as i32,
                    attributes: Some(
                        history_event::Attributes::WorkflowExecutionStartedEventAttributes(
                            WorkflowExecutionStartedEventAttributes {
                                workflow_type: Some(WorkflowType {
                                    name: "EchoWorkflow".to_owned(),
                                }),
                                task_queue: Some(task_queue),
                                original_execution_run_id: "run-trace-1".to_owned(),
                                input: Some(Payloads {
                                    payloads: vec![json_string_payload("ping")],
                                }),
                                header: Some(Header {
                                    fields: HashMap::from([(
                                        TRACER_DATA_HEADER.to_owned(),
                                        start_tracer_payload,
                                    )]),
                                }),
                                workflow_id: "wf-trace-1".to_owned(),
                                ..Default::default()
                            },
                        ),
                    ),
                    ..Default::default()
                },
                HistoryEvent {
                    event_id: 2,
                    event_time: Some(now),
                    event_type: EventType::WorkflowTaskScheduled as i32,
                    attributes: Some(
                        history_event::Attributes::WorkflowTaskScheduledEventAttributes(
                            WorkflowTaskScheduledEventAttributes::default(),
                        ),
                    ),
                    ..Default::default()
                },
                HistoryEvent {
                    event_id: 3,
                    event_time: Some(now),
                    event_type: EventType::WorkflowTaskStarted as i32,
                    attributes: Some(
                        history_event::Attributes::WorkflowTaskStartedEventAttributes(
                            WorkflowTaskStartedEventAttributes {
                                scheduled_event_id: 2,
                                ..Default::default()
                            },
                        ),
                    ),
                    ..Default::default()
                },
            ],
        }),
        ..Default::default()
    }
}

#[test]
fn trace_interceptors_propagate_tracer_data_live_across_workflow_and_activity() {
    thread_local! {
        static DISPATCH_GUARD: std::cell::RefCell<Option<tracing::dispatcher::DefaultGuard>> =
            const { std::cell::RefCell::new(None) };
    }

    global::set_text_map_propagator(TextMapCompositePropagator::new(vec![
        Box::new(TraceContextPropagator::new()),
        Box::new(BaggagePropagator::new()),
    ]));
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("worker-trace-test")));
    let dispatch = tracing::Dispatch::new(subscriber);
    let worker_dispatch = dispatch.clone();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .on_thread_start(move || {
            let guard = tracing::dispatcher::set_default(&worker_dispatch);
            DISPATCH_GUARD.with(|cell| *cell.borrow_mut() = Some(guard));
        })
        .on_thread_stop(|| {
            DISPATCH_GUARD.with(|cell| *cell.borrow_mut() = None);
        })
        .build()
        .unwrap();

    let client_trace_id = TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap();
    let client_span_id = SpanId::from_hex("00f067aa0ba902b7").unwrap();
    let traceparent = format!("00-{client_trace_id}-{client_span_id}-01");
    let dc = build_data_converter(64 * 1024, None);
    let pc = dc.payload_converter();
    let ctx_data = SerializationContextData::Workflow(WorkflowSerializationContext::new());
    let start_tracer_payload = pc
        .to_payload(
            &SerializationContext::new(&ctx_data, pc),
            &HashMap::from([("traceparent".to_owned(), traceparent)]),
        )
        .unwrap();

    let poll_release = CancellationToken::new();
    let rpc = TraceWorkflowRpcMock::new(start_tracer_payload, poll_release.clone());

    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async {
            let (startup, conn) = admit_connection(rpc.service()).await;
            let mut registry = WorkerRegistry::new();
            registry
                .default_queue()
                .unwrap()
                .register_workflow::<EchoWorkflow>()
                .unwrap()
                .register_activities(EchoActivities);

            let prepared = PreparedTemporalWorker::prepare(
                &conn,
                &base_worker_options(),
                base_deployment(),
                registry,
            )
            .unwrap();
            let mut handle = prepared.start(tokio::runtime::Handle::current()).unwrap();

            tokio::time::timeout(Duration::from_secs(5), rpc.wf_completed.notified())
                .await
                .expect("workflow task completed");
            tokio::time::timeout(Duration::from_secs(5), rpc.act_completed.notified())
                .await
                .expect("activity task completed");

            handle.initiate_shutdown();
            poll_release.cancel();
            assert_eq!(
                handle.finish(Instant::now() + Duration::from_secs(5)).await,
                Ok(CloseOutcome::Complete)
            );
            assert_eq!(
                startup.close(Instant::now() + Duration::from_secs(1)).await,
                CloseOutcome::Complete
            );
        });
    });

    let _ = provider.force_flush();
    let header = rpc
        .captured_activity_header
        .lock()
        .unwrap()
        .clone()
        .expect("activity command carries header");
    assert!(
        header.fields.contains_key(TRACER_DATA_HEADER),
        "live schedule_activity must inject _tracer-data"
    );

    assert_live_trace_spans(&exporter, client_trace_id, client_span_id);
}

fn assert_live_trace_spans(
    exporter: &InMemorySpanExporter,
    client_trace_id: TraceId,
    client_span_id: SpanId,
) {
    let spans = exporter.get_finished_spans().unwrap();
    let wf_span = spans
        .iter()
        .find(|span| span.name == "temporal_run_workflow")
        .expect("live workflow exports temporal_run_workflow span");
    assert!(
        wf_span.links.iter().any(|link| {
            link.span_context.trace_id() == client_trace_id
                && link.span_context.span_id() == client_span_id
        }),
        "workflow span must link to caller trace context: {:?}",
        wf_span.links
    );

    let sched_span = spans
        .iter()
        .find(|span| span.name == "temporal_schedule_activity")
        .expect("live schedule_activity exports span");
    let act_span = spans
        .iter()
        .find(|span| span.name == "temporal_run_activity")
        .expect("activity exports temporal_run_activity span");
    assert_eq!(
        act_span.span_context.trace_id(),
        sched_span.span_context.trace_id(),
        "activity span must inherit trace_id from workflow schedule_activity via _tracer-data"
    );
    assert_eq!(
        act_span.parent_span_id,
        sched_span.span_context.span_id(),
        "activity span must SetParent to schedule_activity span"
    );
}

#[test]
fn deadline_from_system_time_maps_future_and_past_deadlines() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let future = deadline_from_system_time(SystemTime::now() + Duration::from_secs(30));
        assert!(!future.expired());
        assert!(future.remaining() > Duration::from_secs(20));

        let past = deadline_from_system_time(SystemTime::now() - Duration::from_secs(1));
        assert!(past.expired());
    });
}
