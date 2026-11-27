#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test helpers panic directly on broken fixtures"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use health::Probe as _;
use infra_temporal::worker::{
    PreparedTemporalWorker, TemporalWorkerOptions, VersioningBehaviorMode,
    WorkerDeploymentMetadata, WorkerRegistry, WorkerTunerOptions, activity_operation_context,
};
use infra_temporal::{
    CloseOutcome, HISTOGRAMS, MutationDedupe, NonBlankName, RequestKey, RetryPolicy, StartFailure,
    StartIdPolicy, StartRequest, StartWorkflowError, TemporalAuth, TemporalConnection,
    TemporalOptions, TemporalStartup, UpdateId, UpdateWithStartRequest, WorkflowExecutionStatus,
    WorkflowId,
};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use observe_test_support::EventCapture;
use opentelemetry::global;
use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use operation_context::OperationContext;
use temporalio_macros::{activities, workflow, workflow_methods};
use temporalio_sdk::activities::{ActivityContext, ActivityError};
use temporalio_sdk::{ActivityOptions, SyncWorkflowContext, WorkflowContext, WorkflowResult};
use temporalio_workflow::WorkflowContextView;
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::layer::SubscriberExt as _;
use url::Url;

const OP_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

static UNIQUE_SEQ: AtomicU64 = AtomicU64::new(1);

struct TelemetryHarness {
    prometheus: PrometheusHandle,
    events: EventCapture,
    exporter: InMemorySpanExporter,
    provider: SdkTracerProvider,
    task_queues: Mutex<BTreeSet<String>>,
}

static TELEMETRY: LazyLock<TelemetryHarness> = LazyLock::new(|| {
    global::set_text_map_propagator(TextMapCompositePropagator::new(vec![
        Box::new(TraceContextPropagator::new()),
        Box::new(BaggagePropagator::new()),
    ]));
    let mut builder = PrometheusBuilder::new();
    for &(name, buckets) in HISTOGRAMS {
        builder = builder
            .set_buckets_for_metric(Matcher::Full(name.to_owned()), buckets)
            .unwrap();
    }
    let prometheus = builder.install_recorder().unwrap();
    let events = EventCapture::new();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(events.clone())
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("infra-temporal-it")));
    tracing::subscriber::set_global_default(subscriber).unwrap();
    TelemetryHarness {
        prometheus,
        events,
        exporter,
        provider,
        task_queues: Mutex::new(BTreeSet::new()),
    }
});

fn telemetry() -> &'static TelemetryHarness {
    &TELEMETRY
}

fn unique_task_queue(harness: &TelemetryHarness, prefix: &str) -> String {
    let queue = unique_name(prefix);
    harness.task_queues.lock().unwrap().insert(queue.clone());
    queue
}

fn temporal_endpoint() -> Url {
    let raw = std::env::var("TEMPORAL_ADDRESS")
        .expect("TEMPORAL_ADDRESS is required; run just test-integration-temporal");
    let normalized = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw
    } else {
        format!("http://{raw}")
    };
    Url::parse(&normalized).expect("TEMPORAL_ADDRESS must form a valid URL")
}

fn unique_name(prefix: &str) -> String {
    let seq = UNIQUE_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{seq}", std::process::id())
}

fn base_options(auth: TemporalAuth, refresh_interval: Duration) -> TemporalOptions {
    TemporalOptions {
        endpoint: temporal_endpoint(),
        namespace: "default".to_owned(),
        identity: format!("infra-temporal-it-{}@0.1.0", std::process::id()),
        auth,
        tls: None,
        allow_plaintext: true,
        allow_unauthenticated: true,
        trusted_network: false,
        credential_refresh_interval: refresh_interval,
        connect_timeout: Duration::from_secs(10),
        rpc_timeout: Duration::from_secs(10),
        keep_alive_interval: Duration::from_secs(30),
        keep_alive_timeout: Duration::from_secs(15),
        dns_load_balancing: false,
        http_proxy_url: None,
        max_payload_bytes: 512 * 1024,
        probe_budget: Duration::from_secs(5),
        payload_codec: None,
    }
}

fn worker_options(task_queue: String, max_cached_workflows: usize) -> TemporalWorkerOptions {
    TemporalWorkerOptions {
        task_queue: Some(task_queue),
        queues: BTreeMap::new(),
        max_cached_workflows,
        workflow_pollers: None,
        activity_pollers: None,
        max_worker_activities_per_second: None,
        tuner: WorkerTunerOptions::FixedSize {
            max_concurrent_workflow_tasks: 8,
            max_concurrent_activities: 8,
            max_concurrent_local_activities: 8,
        },
        graceful_shutdown_period: Duration::from_secs(5),
    }
}

fn deployment_metadata(name: &str) -> WorkerDeploymentMetadata {
    WorkerDeploymentMetadata {
        deployment_name: name.to_owned(),
        build_id: "0.1.0-integration".to_owned(),
        use_worker_versioning: false,
        default_versioning_behavior: VersioningBehaviorMode::Unspecified,
    }
}

async fn admit_client(
    auth: TemporalAuth,
    refresh_interval: Duration,
) -> (TemporalStartup, TemporalConnection) {
    let ctx = OperationContext::with_timeout(OP_TIMEOUT);
    let mut startup = TemporalStartup::prepare(base_options(auth, refresh_interval), &ctx);
    let conn = startup
        .admit(CancellationToken::new())
        .await
        .expect("TemporalStartup::admit succeeds against Compose temporalio/temporal");
    (startup, conn)
}

fn counter_value(scrape: &str, series: &str) -> u64 {
    scrape
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix(series)?.strip_prefix(' ')?;
            value.trim().parse::<u64>().ok()
        })
        .unwrap_or(0)
}

async fn poll_until(label: &str, timeout: Duration, mut check: impl FnMut() -> bool) {
    tokio::time::timeout(timeout, async {
        loop {
            if check() {
                return;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {label}"));
}

async fn write_file_atomically(path: &Path, contents: &[u8]) {
    let tmp = path.with_extension("next");
    tokio::fs::write(&tmp, contents).await.unwrap();
    tokio::fs::rename(&tmp, path).await.unwrap();
}

#[workflow]
#[derive(Default)]
struct LifecycleWorkflow {
    stage: String,
    updates: Vec<String>,
    released: bool,
    signal_note: String,
}

#[workflow_methods]
impl LifecycleWorkflow {
    #[run]
    async fn run(ctx: &mut WorkflowContext<Self>, input: String) -> WorkflowResult<String> {
        ctx.state_mut(|state| {
            state.stage = format!("started:{input}");
        });
        let echoed = ctx
            .execute_activity(
                LifecycleActivities::echo_with_heartbeat,
                input,
                ActivityOptions::start_to_close_timeout(Duration::from_secs(10)),
            )
            .await?;
        ctx.state_mut(|state| {
            state.stage = format!("waiting:{echoed}");
        });
        ctx.wait_condition(|state| state.released).await?;
        Ok(ctx.state(|state| {
            format!(
                "{}|updates={}|signal={}",
                state.stage,
                state.updates.join(","),
                state.signal_note
            )
        }))
    }

    #[query]
    fn status(&self, _ctx: &WorkflowContextView) -> String {
        self.stage.clone()
    }

    #[update]
    fn append_tag(&mut self, _ctx: &mut SyncWorkflowContext<Self>, tag: String) -> String {
        self.updates.push(tag);
        format!("tags:{}", self.updates.join(","))
    }

    #[signal]
    fn release(&mut self, _ctx: &mut SyncWorkflowContext<Self>, note: String) {
        self.signal_note = note;
        self.released = true;
    }
}

struct LifecycleActivities;

#[activities]
impl LifecycleActivities {
    #[activity]
    async fn echo_with_heartbeat(
        ctx: ActivityContext,
        input: String,
    ) -> Result<String, ActivityError> {
        let op = activity_operation_context(&ctx);
        assert!(op.deadline().is_some());
        ctx.record_heartbeat(vec!["step-1".to_owned()])
            .await
            .unwrap();
        tokio::task::yield_now().await;
        Ok(format!("echo:{input}"))
    }
}

#[workflow]
#[derive(Default)]
struct ReplayTraceWorkflow {
    activity_output: String,
    done: bool,
}

#[workflow_methods]
impl ReplayTraceWorkflow {
    #[run]
    async fn run(ctx: &mut WorkflowContext<Self>, input: String) -> WorkflowResult<String> {
        let out = ctx
            .execute_activity(
                ReplayTraceActivities::work,
                input,
                ActivityOptions::start_to_close_timeout(Duration::from_secs(10)),
            )
            .await?;
        ctx.state_mut(|state| {
            state.activity_output = out;
        });
        ctx.wait_condition(|state| state.done).await?;
        Ok(ctx.state(|state| format!("replayed:{}", state.activity_output)))
    }

    #[query]
    fn activity_done(&self, _ctx: &WorkflowContextView) -> bool {
        !self.activity_output.is_empty()
    }

    #[signal]
    fn finish(&mut self, _ctx: &mut SyncWorkflowContext<Self>) {
        self.done = true;
    }
}

struct ReplayTraceActivities;

#[activities]
impl ReplayTraceActivities {
    #[activity]
    async fn work(_ctx: ActivityContext, input: String) -> Result<String, ActivityError> {
        tokio::task::yield_now().await;
        Ok(format!("traced:{input}"))
    }
}

#[workflow]
#[derive(Default)]
struct PreemptRetryWorkflow;

#[workflow_methods]
impl PreemptRetryWorkflow {
    #[run]
    async fn run(ctx: &mut WorkflowContext<Self>, input: String) -> WorkflowResult<String> {
        let mut opts = ActivityOptions::start_to_close_timeout(Duration::from_secs(2));
        let mut retry = RetryPolicy::default();
        retry
            .set_initial_interval(Duration::from_millis(100))
            .set_backoff_coefficient(1.0)
            .set_maximum_interval(Some(Duration::from_millis(200)))
            .set_maximum_attempts(5);
        opts.retry_policy = Some(retry);
        let output = ctx
            .execute_activity(PreemptRetryActivities::run_step, input, opts)
            .await?;
        Ok(output)
    }
}

struct PreemptRetryActivities {
    stall_first_attempt: bool,
    attempt_one_started: Arc<Notify>,
}

#[activities]
impl PreemptRetryActivities {
    #[activity]
    async fn run_step(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: String,
    ) -> Result<String, ActivityError> {
        let attempt = ctx.info().attempt;
        if self.stall_first_attempt && attempt == 1 {
            self.attempt_one_started.notify_one();
            futures::future::pending::<()>().await;
        }
        Ok(format!("{input}:attempt-{attempt}"))
    }
}

fn prometheus_label_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (_, rest) = line.split_once('{')?;
    let (mut rest, _) = rest.rsplit_once('}')?;
    while !rest.is_empty() {
        let (name, after_eq) = rest.split_once('=')?;
        let after_quote = after_eq.strip_prefix('"')?;
        let (value, tail) = after_quote.split_once('"')?;
        if name == key {
            return Some(value);
        }
        rest = tail.trim_prefix(',');
    }
    None
}

fn assert_lifecycle_metrics(harness: &TelemetryHarness, task_queue: &str) {
    let scrape = harness.prometheus.render();
    let known_queues = harness.task_queues.lock().unwrap().clone();
    assert!(
        scrape.contains("temporal_request{") && scrape.contains("temporal_request_failure{"),
        "{scrape}"
    );
    assert!(
        scrape.lines().any(|line| {
            line.starts_with("temporal_workflow_task_schedule_to_start_latency_bucket{")
                && prometheus_label_value(line, "le") == Some("1")
                && prometheus_label_value(line, "task_queue") == Some(task_queue)
        }),
        "missing le=\"1\" bucket on temporal_workflow_task_schedule_to_start_latency_bucket for {task_queue}:\n{scrape}"
    );
    let mut saw_task_queue = false;
    for line in scrape.lines().filter(|line| line.starts_with("temporal_")) {
        if let Some(observed_queue) = prometheus_label_value(line, "task_queue") {
            assert!(
                known_queues.contains(observed_queue),
                "unexpected or sticky task_queue label {observed_queue:?} in line: {line}"
            );
            if observed_queue == task_queue {
                saw_task_queue = true;
            }
        }
    }
    assert!(
        saw_task_queue,
        "no temporal_* metric carried task_queue={task_queue:?}:\n{scrape}"
    );
}

async fn exercise_update_with_start(
    client: &infra_temporal::TemporalClient,
    op: &OperationContext,
    queue_name: NonBlankName,
) {
    let uws_id = WorkflowId::try_from(unique_name("it-uws-wf")).unwrap();
    let uws_reply = client
        .execute_update_with_start(
            op,
            UpdateWithStartRequest::new(
                StartRequest::new(
                    LifecycleWorkflow::default(),
                    queue_name,
                    uws_id.clone(),
                    StartIdPolicy::AllowDuplicate {
                        running_conflict: infra_temporal::RunningConflictPolicy::UseExisting,
                    },
                    "order-99".to_owned(),
                ),
                LifecycleWorkflow::append_tag,
                UpdateId::try_from(unique_name("uws")).unwrap(),
                "atomic".to_owned(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(uws_reply, "tags:atomic");

    let uws_handle = client.workflow_handle::<LifecycleWorkflow>(uws_id, None);
    uws_handle
        .signal(
            op,
            LifecycleWorkflow::release,
            "done".to_owned(),
            MutationDedupe::Unkeyed,
        )
        .await
        .unwrap();
    assert!(
        uws_handle
            .get_result(op)
            .await
            .unwrap()
            .contains("updates=atomic")
    );
}

async fn await_lifecycle_status(
    handle: &infra_temporal::WorkflowHandle<LifecycleWorkflow>,
    op: &OperationContext,
    expected: &str,
) {
    tokio::time::timeout(OP_TIMEOUT, async {
        loop {
            let status = handle
                .query(op, LifecycleWorkflow::status, ())
                .await
                .unwrap();
            if status == expected {
                return;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_workflow_start_query_update_signal_duplicate_and_metrics() {
    let harness = telemetry();
    let task_queue = unique_task_queue(harness, "it-lifecycle-q");
    let (startup, conn) = admit_client(TemporalAuth::None, Duration::from_secs(30)).await;
    let client = conn.client();

    let mut registry = WorkerRegistry::new();
    registry
        .default_queue()
        .unwrap()
        .register_workflow::<LifecycleWorkflow>()
        .unwrap()
        .register_activities(LifecycleActivities);

    let prepared = PreparedTemporalWorker::prepare(
        &conn,
        &worker_options(task_queue.clone(), 64),
        deployment_metadata("it-lifecycle"),
        registry,
    )
    .unwrap();
    prepared
        .probe()
        .check()
        .await
        .expect("live Temporal probe succeeds");

    let mut worker = prepared.start(tokio::runtime::Handle::current()).unwrap();
    let op = OperationContext::with_timeout(OP_TIMEOUT);
    let workflow_id = WorkflowId::try_from(unique_name("it-lifecycle-wf")).unwrap();
    let queue_name = NonBlankName::try_from(task_queue.as_str()).unwrap();

    let mut memo = BTreeMap::new();
    memo.insert(
        "source".to_owned(),
        serde_json::Value::String("integration".to_owned()),
    );
    let req = StartRequest::new(
        LifecycleWorkflow::default(),
        queue_name.clone(),
        workflow_id.clone(),
        StartIdPolicy::RejectDuplicate,
        "order-42".to_owned(),
    )
    .memo(memo);
    let handle = client.start_workflow(&op, req).await.unwrap();
    assert_eq!(handle.workflow_id(), workflow_id.as_str());

    let duplicate = client
        .start_workflow(
            &op,
            StartRequest::new(
                LifecycleWorkflow::default(),
                queue_name.clone(),
                workflow_id.clone(),
                StartIdPolicy::RejectDuplicate,
                "order-42".to_owned(),
            ),
        )
        .await;
    assert_eq!(
        duplicate.unwrap_err(),
        StartWorkflowError::NotApplied(StartFailure::AlreadyStarted)
    );

    await_lifecycle_status(&handle, &op, "waiting:echo:order-42").await;

    let update_reply = handle
        .execute_update(
            &op,
            LifecycleWorkflow::append_tag,
            UpdateId::try_from(unique_name("upd")).unwrap(),
            "priority".to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(update_reply, "tags:priority");

    handle
        .signal(
            &op,
            LifecycleWorkflow::release,
            "approved".to_owned(),
            MutationDedupe::Key(RequestKey::try_from(unique_name("sig")).unwrap()),
        )
        .await
        .unwrap();

    let result = handle.get_result(&op).await.unwrap();
    assert_eq!(
        result,
        "waiting:echo:order-42|updates=priority|signal=approved"
    );

    let description = handle.describe(&op).await.unwrap();
    assert_eq!(description.status(), WorkflowExecutionStatus::Completed);

    exercise_update_with_start(&client, &op, queue_name).await;
    assert_lifecycle_metrics(harness, &task_queue);

    worker.initiate_shutdown();
    assert_eq!(
        worker
            .finish(Instant::now() + Duration::from_secs(10))
            .await,
        Ok(CloseOutcome::Complete)
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(5)).await,
        CloseOutcome::Complete
    );
}

fn has_span_attr(span: &opentelemetry_sdk::trace::SpanData, key: &str, expected: &str) -> bool {
    span.attributes
        .iter()
        .any(|kv| kv.key.as_str() == key && kv.value.as_str() == expected)
}

fn assert_replay_trace_spans(
    harness: &TelemetryHarness,
    root_trace_id: opentelemetry::trace::TraceId,
    task_queue: &str,
) {
    let _ = harness.provider.force_flush();
    let spans = harness.exporter.get_finished_spans().unwrap();

    let start_count = spans
        .iter()
        .filter(|span| {
            span.name == "temporal_start_workflow" && span.span_context.trace_id() == root_trace_id
        })
        .count();
    assert_eq!(start_count, 1, "{spans:?}");

    let run_wf_spans: Vec<_> = spans
        .iter()
        .filter(|span| {
            span.name == "temporal_run_workflow"
                && has_span_attr(span, "temporal.task_queue", task_queue)
        })
        .collect();
    assert_eq!(
        run_wf_spans.len(),
        1,
        "replaying workflow history on worker_two must not duplicate temporal_run_workflow: {run_wf_spans:?}"
    );
    assert!(
        run_wf_spans[0]
            .links
            .iter()
            .any(|link| link.span_context.trace_id() == root_trace_id),
        "temporal_run_workflow must link to caller trace via _tracer-data: {:?}",
        run_wf_spans[0]
    );
    let wf_trace_id = run_wf_spans[0].span_context.trace_id();
    assert_ne!(
        wf_trace_id, root_trace_id,
        "temporal_run_workflow must start a linked workflow trace rather than reusing caller trace_id"
    );

    let activity_type = ReplayTraceActivities::work.name();
    let schedule_act_spans: Vec<_> = spans
        .iter()
        .filter(|span| {
            span.name == "temporal_schedule_activity"
                && has_span_attr(span, "temporal.activity_type", activity_type)
        })
        .collect();
    assert_eq!(
        schedule_act_spans.len(),
        1,
        "replay must not duplicate temporal_schedule_activity across any trace: {schedule_act_spans:?}"
    );
    assert_eq!(
        schedule_act_spans[0].span_context.trace_id(),
        wf_trace_id,
        "temporal_schedule_activity must stay on workflow trace: {:?}",
        schedule_act_spans[0]
    );
    assert!(
        !spans.iter().any(|span| {
            span.name == "temporal_schedule_activity"
                && span.span_context.trace_id() == root_trace_id
        }),
        "replay must not emit temporal_schedule_activity under workflow_parent root_trace_id: {spans:?}"
    );

    let run_act_spans: Vec<_> = spans
        .iter()
        .filter(|span| {
            span.name == "temporal_run_activity"
                && has_span_attr(span, "temporal.task_queue", task_queue)
        })
        .collect();
    assert_eq!(run_act_spans.len(), 1, "{run_act_spans:?}");
    assert_eq!(
        run_act_spans[0].span_context.trace_id(),
        wf_trace_id,
        "activity span must propagate workflow trace context through _tracer-data"
    );
    assert_eq!(
        run_act_spans[0].parent_span_id,
        schedule_act_spans[0].span_context.span_id(),
        "activity span must parent to temporal_schedule_activity via _tracer-data"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_otel_tracer_data_propagates_and_deduplicates_on_replay() {
    let harness = telemetry();
    let task_queue = unique_task_queue(harness, "it-trace-q");
    let (startup, conn) = admit_client(TemporalAuth::None, Duration::from_secs(30)).await;
    let client = conn.client();
    let op = OperationContext::with_timeout(OP_TIMEOUT);

    let build_worker = |conn: &TemporalConnection, queue: &str, name: &str| {
        let mut registry = WorkerRegistry::new();
        registry
            .default_queue()
            .unwrap()
            .register_workflow::<ReplayTraceWorkflow>()
            .unwrap()
            .register_activities(ReplayTraceActivities);
        PreparedTemporalWorker::prepare(
            conn,
            &worker_options(queue.to_owned(), 64),
            deployment_metadata(name),
            registry,
        )
        .unwrap()
        .start(tokio::runtime::Handle::current())
        .unwrap()
    };

    let mut worker_one = build_worker(&conn, &task_queue, "it-trace-w1");

    let root_span = tracing::info_span!("it_temporal_trace_root");
    let root_trace_id = root_span.context().span().span_context().trace_id();
    assert_ne!(root_trace_id, opentelemetry::trace::TraceId::INVALID);

    let workflow_id = WorkflowId::try_from(unique_name("it-trace-wf")).unwrap();
    let handle = client
        .start_workflow(
            &op,
            StartRequest::new(
                ReplayTraceWorkflow::default(),
                NonBlankName::try_from(task_queue.as_str()).unwrap(),
                workflow_id,
                StartIdPolicy::RejectDuplicate,
                "payload-1".to_owned(),
            ),
        )
        .instrument(root_span)
        .await
        .unwrap();

    tokio::time::timeout(OP_TIMEOUT, async {
        loop {
            if handle
                .query(&op, ReplayTraceWorkflow::activity_done, ())
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .unwrap();

    worker_one.initiate_shutdown();
    assert_eq!(
        worker_one
            .finish(Instant::now() + Duration::from_secs(10))
            .await,
        Ok(CloseOutcome::Complete)
    );

    let mut worker_two = build_worker(&conn, &task_queue, "it-trace-w2");
    handle
        .signal(
            &op,
            ReplayTraceWorkflow::finish,
            (),
            MutationDedupe::Key(RequestKey::try_from(unique_name("trace-sig")).unwrap()),
        )
        .await
        .unwrap();
    let result = handle.get_result(&op).await.unwrap();
    assert_eq!(result, "replayed:traced:payload-1");

    worker_two.initiate_shutdown();
    assert_eq!(
        worker_two
            .finish(Instant::now() + Duration::from_secs(10))
            .await,
        Ok(CloseOutcome::Complete)
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(5)).await,
        CloseOutcome::Complete
    );

    assert_replay_trace_spans(harness, root_trace_id, &task_queue);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_api_key_file_rotation_and_corrupt_file_preservation() {
    let harness = telemetry();
    let dir = tempfile::tempdir().unwrap();
    let secret_filename = unique_name("sentinel-temporal-key");
    let api_key_path = dir.path().join(format!("{secret_filename}.txt"));
    let first_secret = format!("live-secret-v1-{}", unique_name("tok"));
    let second_secret = format!("live-secret-v2-{}", unique_name("tok"));
    tokio::fs::write(&api_key_path, format!("{first_secret}\n"))
        .await
        .unwrap();

    let (startup, conn) = admit_client(
        TemporalAuth::ApiKeyFile(api_key_path.clone()),
        Duration::from_millis(50),
    )
    .await;
    let probe = conn.probe();
    probe
        .check()
        .await
        .expect("initial api_key_file connection succeeds against live Temporal Server");

    let failed_series = r#"temporal_credential_reloads_total{kind="api_key",outcome="failed"}"#;
    let ok_series = r#"temporal_credential_reloads_total{kind="api_key",outcome="ok"}"#;

    let failed_before = counter_value(&harness.prometheus.render(), failed_series);
    let ok_before = counter_value(&harness.prometheus.render(), ok_series);

    write_file_atomically(&api_key_path, b"   \n").await;
    poll_until(
        "corrupt api_key_file increments failed reload counter",
        Duration::from_secs(5),
        || counter_value(&harness.prometheus.render(), failed_series) > failed_before,
    )
    .await;

    probe
        .check()
        .await
        .expect("corrupt api_key_file preserves last known good credential");
    assert_eq!(
        counter_value(&harness.prometheus.render(), ok_series),
        ok_before
    );

    write_file_atomically(&api_key_path, format!("{second_secret}\n").as_bytes()).await;
    poll_until(
        "valid replacement api_key_file increments ok reload counter",
        Duration::from_secs(5),
        || counter_value(&harness.prometheus.render(), ok_series) > ok_before,
    )
    .await;

    probe
        .check()
        .await
        .expect("rotated api_key_file continues succeeding against live Temporal Server");

    let scrape = harness.prometheus.render();
    let events = harness.events.snapshot();
    assert!(
        events
            .iter()
            .any(|ev| ev.message() == Some("temporal_credential_reload_failed")),
        "expected temporal_credential_reload_failed event"
    );
    assert!(
        events
            .iter()
            .any(|ev| ev.message() == Some("temporal_credential_reloaded")),
        "expected temporal_credential_reloaded event"
    );
    for forbidden in [
        first_secret.as_str(),
        second_secret.as_str(),
        secret_filename.as_str(),
    ] {
        assert!(!scrape.contains(forbidden), "leaked secret in metrics");
        assert!(
            !events
                .iter()
                .any(|ev| ev.fields.values().any(|val| val.contains(forbidden))),
            "leaked secret in tracing events"
        );
    }

    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(5)).await,
        CloseOutcome::Complete
    );
    assert!(probe.check().await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_force_cancel_preempts_via_will_complete_async_and_server_retries_to_completion() {
    let harness = telemetry();
    let task_queue = unique_task_queue(harness, "it-preempt-q");
    let (startup, conn) = admit_client(TemporalAuth::None, Duration::from_secs(30)).await;
    let client = conn.client();
    let op = OperationContext::with_timeout(OP_TIMEOUT);

    let attempt_one_started = Arc::new(Notify::new());
    let mut reg_one = WorkerRegistry::new();
    reg_one
        .default_queue()
        .unwrap()
        .register_workflow::<PreemptRetryWorkflow>()
        .unwrap()
        .register_activities(PreemptRetryActivities {
            stall_first_attempt: true,
            attempt_one_started: Arc::clone(&attempt_one_started),
        });

    let mut worker_one = PreparedTemporalWorker::prepare(
        &conn,
        &worker_options(task_queue.clone(), 64),
        deployment_metadata("it-preempt-w1"),
        reg_one,
    )
    .unwrap()
    .start(tokio::runtime::Handle::current())
    .unwrap();

    let handle = client
        .start_workflow(
            &op,
            StartRequest::new(
                PreemptRetryWorkflow,
                NonBlankName::try_from(task_queue.as_str()).unwrap(),
                WorkflowId::try_from(unique_name("it-preempt-wf")).unwrap(),
                StartIdPolicy::RejectDuplicate,
                "preempt-job".to_owned(),
            ),
        )
        .await
        .unwrap();

    tokio::time::timeout(OP_TIMEOUT, attempt_one_started.notified())
        .await
        .expect("worker_one started attempt 1");

    let forced_before = counter_value(&harness.prometheus.render(), "temporal_drain_forced_total");
    let force_started = Instant::now();
    worker_one.force_cancel();
    assert_eq!(
        worker_one
            .finish(Instant::now() + Duration::from_secs(5))
            .await,
        Ok(CloseOutcome::Complete)
    );
    assert!(
        force_started.elapsed() < Duration::from_secs(4),
        "forced preemption must release worker promptly"
    );
    assert!(
        counter_value(&harness.prometheus.render(), "temporal_drain_forced_total") > forced_before
    );

    let mut reg_two = WorkerRegistry::new();
    reg_two
        .default_queue()
        .unwrap()
        .register_workflow::<PreemptRetryWorkflow>()
        .unwrap()
        .register_activities(PreemptRetryActivities {
            stall_first_attempt: false,
            attempt_one_started: Arc::new(Notify::new()),
        });

    let mut worker_two = PreparedTemporalWorker::prepare(
        &conn,
        &worker_options(task_queue, 64),
        deployment_metadata("it-preempt-w2"),
        reg_two,
    )
    .unwrap()
    .start(tokio::runtime::Handle::current())
    .unwrap();

    let result = handle.get_result(&op).await.unwrap();
    assert_eq!(result, "preempt-job:attempt-2");

    worker_two.initiate_shutdown();
    assert_eq!(
        worker_two
            .finish(Instant::now() + Duration::from_secs(10))
            .await,
        Ok(CloseOutcome::Complete)
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(5)).await,
        CloseOutcome::Complete
    );
}
