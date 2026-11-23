mod interceptors;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU32;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle as ThreadJoinHandle;
use std::time::Duration;

use temporalio_common::worker::{
    VersioningBehavior, WorkerDeploymentOptions, WorkerDeploymentVersion,
};
use temporalio_common::{ActivityDefinition, WorkflowDefinition};
use temporalio_sdk::activities::{ActivityImplementer, ExecutableActivity};
use temporalio_sdk::runtime::worker_tuner::{
    FixedSizeSlotSupplier, ResourceBasedController, ResourceBasedTuner, ResourceBasedTunerOptions,
    ResourceBasedTunerWithController, TunerHolder, WorkerTuner,
};
use temporalio_sdk::runtime::{PollerBehavior, RuntimeOptions};
use temporalio_sdk::workflows::WorkflowImplementation;
use temporalio_sdk::{Runtime, RuntimeError, Worker, WorkerOptions, WorkerRunError};
use temporalio_sdk_core::CoreRuntime;
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub use temporalio_sdk::WorkflowRegistrationError;

pub(crate) use crate::worker::interceptors::trace_workflow_interceptor_constructor;
pub use crate::worker::interceptors::{Preempted, activity_operation_context, run_with_preemption};

use crate::connection::{CloseOutcome, TemporalConnection};
use crate::metrics::WorkspaceMetricsMeter;
use crate::probe::TemporalProbe;
use crate::worker::interceptors::{
    ActivityOperationScopeInterceptor, ForceShutdownActivityInterceptor, TraceActivityInterceptor,
};

const MAX_QUEUE_NAME_BYTES: usize = 255;
const MAX_CACHED_WORKFLOWS_LIMIT: usize = 100_000;
const MIN_TUNER_RATIO: f64 = 0.05;
const MAX_TUNER_RATIO: f64 = 0.95;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VersioningBehaviorMode {
    #[default]
    Unspecified,
    Pinned,
    AutoUpgrade,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerDeploymentMetadata {
    pub deployment_name: String,
    pub build_id: String,
    pub use_worker_versioning: bool,
    pub default_versioning_behavior: VersioningBehaviorMode,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WorkerTunerOptions {
    ResourceBased {
        target_memory: f64,
        target_cpu: f64,
    },
    FixedSize {
        max_concurrent_workflow_tasks: usize,
        max_concurrent_activities: usize,
        max_concurrent_local_activities: usize,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskQueueOverrides {
    pub max_cached_workflows: Option<usize>,
    pub workflow_pollers: Option<NonZeroU32>,
    pub activity_pollers: Option<NonZeroU32>,
    pub max_worker_activities_per_second: Option<f64>,
    pub tuner: Option<WorkerTunerOptions>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TemporalWorkerOptions {
    pub task_queue: Option<String>,
    pub queues: BTreeMap<String, TaskQueueOverrides>,
    pub max_cached_workflows: usize,
    pub workflow_pollers: Option<NonZeroU32>,
    pub activity_pollers: Option<NonZeroU32>,
    pub max_worker_activities_per_second: Option<f64>,
    pub tuner: WorkerTunerOptions,
    pub graceful_shutdown_period: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("task queue name must not be blank")]
    BlankQueueName,
    #[error("task queue name must not contain control characters")]
    ControlCharsInQueueName,
    #[error("task queue {0} is already registered")]
    DuplicateQueue(String),
    #[error("default task queue is already registered")]
    DuplicateDefaultQueue,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorkerPrepareError {
    #[error("no workflows or activities are registered")]
    NoHandlersRegistered,
    #[error("default task queue was registered without temporal_worker.task_queue configured")]
    MissingDefaultTaskQueue,
    #[error("configured task queue overrides do not match any registered queue: {}", .0.join(", "))]
    UnmatchedQueueOverrides(Vec<String>),
    #[error("registered task queue {0} collides with temporal_worker.task_queue")]
    QueueCollidesWithDefault(String),
    #[error("invalid temporal worker configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("failed to initialize temporal core worker")]
    CoreBuild,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorkerStartError {
    #[error("failed to spawn temporal worker thread")]
    ThreadSpawn,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorkerFatalError {
    #[error("temporal worker task exited with fatal error")]
    Fatal,
    #[error("temporal worker driver thread panicked")]
    ThreadPanicked,
}

pub struct TaskQueueBuilder {
    options: WorkerOptions,
    has_workflows: bool,
    has_activities: bool,
}

impl fmt::Debug for TaskQueueBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskQueueBuilder")
            .field("has_workflows", &self.has_workflows)
            .field("has_activities", &self.has_activities)
            .finish_non_exhaustive()
    }
}

impl TaskQueueBuilder {
    fn new() -> Self {
        let placeholder_deployment = WorkerDeploymentOptions::from_build_id(String::new());
        Self {
            options: WorkerOptions::new("")
                .deployment_options(placeholder_deployment)
                .build(),
            has_workflows: false,
            has_activities: false,
        }
    }

    pub fn register_workflow<W>(&mut self) -> Result<&mut Self, WorkflowRegistrationError>
    where
        W: WorkflowImplementation,
        <W::Run as WorkflowDefinition>::Input: Send,
    {
        self.options.register_workflow::<W>()?;
        self.has_workflows = true;
        Ok(self)
    }

    pub fn register_activities<AI: ActivityImplementer>(&mut self, instance: AI) -> &mut Self {
        self.options.register_activities(instance);
        self.has_activities = true;
        self
    }

    pub fn register_activity<AD>(&mut self, instance: Arc<AD::Implementer>) -> &mut Self
    where
        AD: ActivityDefinition + ExecutableActivity,
        AD::Input: Send + Sync,
        AD::Output: Send + Sync,
    {
        self.options.register_activity::<AD>(instance);
        self.has_activities = true;
        self
    }

    const fn is_empty(&self) -> bool {
        !self.has_workflows && !self.has_activities
    }
}

#[derive(Default)]
pub struct WorkerRegistry {
    default_queue: Option<TaskQueueBuilder>,
    queues: BTreeMap<String, TaskQueueBuilder>,
}

impl fmt::Debug for WorkerRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerRegistry")
            .field("has_default_queue", &self.default_queue.is_some())
            .field("named_queues", &self.queues.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl WorkerRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            default_queue: None,
            queues: BTreeMap::new(),
        }
    }

    pub fn default_queue(&mut self) -> Result<&mut TaskQueueBuilder, RegistryError> {
        if self.default_queue.is_some() {
            return Err(RegistryError::DuplicateDefaultQueue);
        }
        Ok(self.default_queue.insert(TaskQueueBuilder::new()))
    }

    pub fn queue(
        &mut self,
        name: impl Into<String>,
    ) -> Result<&mut TaskQueueBuilder, RegistryError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(RegistryError::BlankQueueName);
        }
        if name.chars().any(char::is_control) {
            return Err(RegistryError::ControlCharsInQueueName);
        }
        if self.queues.contains_key(&name) {
            return Err(RegistryError::DuplicateQueue(name));
        }
        Ok(self
            .queues
            .entry(name)
            .or_insert_with(TaskQueueBuilder::new))
    }
}

struct PreparedQueue {
    name: String,
    worker: Worker,
}

pub struct PreparedTemporalWorker {
    connection: TemporalConnection,
    runtime: Runtime,
    workers: Vec<PreparedQueue>,
    graceful_stop: CancellationToken,
    force_abort: CancellationToken,
    failed: Arc<AtomicBool>,
}

impl fmt::Debug for PreparedTemporalWorker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let queue_names: Vec<&str> = self.workers.iter().map(|q| q.name.as_str()).collect();
        formatter
            .debug_struct("PreparedTemporalWorker")
            .field("namespace", &self.connection.namespace())
            .field("queues", &queue_names)
            .finish_non_exhaustive()
    }
}

impl PreparedTemporalWorker {
    #[expect(
        clippy::needless_pass_by_value,
        reason = "public prepare contract takes WorkerDeploymentMetadata by value"
    )]
    pub fn prepare(
        conn: &TemporalConnection,
        cfg: &TemporalWorkerOptions,
        build: WorkerDeploymentMetadata,
        registry: WorkerRegistry,
    ) -> Result<Self, WorkerPrepareError> {
        let deployment_options = build_deployment_options(&build)?;
        validate_top_level_options(cfg)?;

        let specs = collect_queue_specs(cfg, registry)?;
        let configured_options = configure_queue_options(cfg, &deployment_options, specs)?;

        let runtime = build_runtime(conn.meter()).map_err(|_| {
            tracing::error!("temporal_core_runtime_build_failed");
            WorkerPrepareError::CoreBuild
        })?;

        let graceful_stop = CancellationToken::new();
        let force_abort = CancellationToken::new();
        let workers = build_core_workers(conn, &runtime, configured_options, &force_abort)?;

        Ok(Self {
            connection: conn.clone(),
            runtime,
            workers,
            graceful_stop,
            force_abort,
            failed: Arc::new(AtomicBool::new(false)),
        })
    }

    #[must_use]
    pub fn probe(&self) -> TemporalProbe {
        self.connection.probe_with_failure(Arc::clone(&self.failed))
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "public worker startup signature takes tokio::runtime::Handle by value"
    )]
    pub fn start(self, runtime: Handle) -> Result<TemporalWorkerHandle, WorkerStartError> {
        const fn assert_send<T: Send>() {}
        assert_send::<Worker>();
        assert_send::<Runtime>();
        assert_send::<CoreRuntime>();

        let total = self.workers.len();
        let (first_error, _) = watch::channel(None);
        let (all_done, _) = watch::channel(total == 0);
        let shared = Arc::new(WorkerShared {
            connection: self.connection,
            graceful_stop: self.graceful_stop,
            force_abort: self.force_abort,
            forced: AtomicBool::new(false),
            failed: self.failed,
            first_error,
            remaining: AtomicUsize::new(total),
            all_done,
        });

        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let mut threads = Vec::with_capacity(total);
        for (index, queue) in self.workers.into_iter().enumerate() {
            let thread_name = format!("temporal-wf-{index}");
            let handle = runtime.clone();
            let shared_ref = Arc::clone(&shared);
            let thread_dispatch = dispatch.clone();
            let spawn_result = std::thread::Builder::new()
                .name(thread_name)
                .spawn(move || {
                    tracing::dispatcher::with_default(&thread_dispatch, || {
                        drive_worker_queue(queue, &handle, &shared_ref);
                    });
                });
            let Ok(join_handle) = spawn_result else {
                tracing::error!(queue_index = index, "temporal_worker_thread_spawn_failed");
                shared.graceful_stop.cancel();
                shared.force_abort.cancel();
                return Err(WorkerStartError::ThreadSpawn);
            };
            threads.push(join_handle);
        }

        Ok(TemporalWorkerHandle {
            shared,
            _runtime: self.runtime,
            threads,
        })
    }
}

type QueueSpec<'a> = (String, TaskQueueBuilder, Option<&'a TaskQueueOverrides>);

fn collect_queue_specs(
    cfg: &TemporalWorkerOptions,
    registry: WorkerRegistry,
) -> Result<Vec<QueueSpec<'_>>, WorkerPrepareError> {
    let WorkerRegistry {
        default_queue,
        queues: named_queues,
    } = registry;

    if default_queue.is_none() && named_queues.is_empty() {
        return Err(WorkerPrepareError::NoHandlersRegistered);
    }

    if let Some(default_name) = cfg.task_queue.as_deref()
        && (cfg.queues.contains_key(default_name) || named_queues.contains_key(default_name))
    {
        return Err(WorkerPrepareError::QueueCollidesWithDefault(
            default_name.to_owned(),
        ));
    }

    let unmatched: Vec<String> = cfg
        .queues
        .keys()
        .filter(|name| !named_queues.contains_key(name.as_str()))
        .cloned()
        .collect();
    if !unmatched.is_empty() {
        return Err(WorkerPrepareError::UnmatchedQueueOverrides(unmatched));
    }

    let mut specs = Vec::with_capacity(usize::from(default_queue.is_some()) + named_queues.len());

    if let Some(builder) = default_queue {
        if builder.is_empty() {
            return Err(WorkerPrepareError::NoHandlersRegistered);
        }
        let Some(default_name) = cfg.task_queue.as_ref() else {
            return Err(WorkerPrepareError::MissingDefaultTaskQueue);
        };
        specs.push((default_name.clone(), builder, None));
    }

    for (name, builder) in named_queues {
        if builder.is_empty() {
            return Err(WorkerPrepareError::NoHandlersRegistered);
        }
        validate_queue_name(&name)?;
        let overrides = cfg.queues.get(&name);
        specs.push((name, builder, overrides));
    }

    Ok(specs)
}

fn configure_queue_options(
    cfg: &TemporalWorkerOptions,
    deployment_options: &WorkerDeploymentOptions,
    specs: Vec<QueueSpec<'_>>,
) -> Result<Vec<(String, WorkerOptions)>, WorkerPrepareError> {
    let shared_resource_controller = match &cfg.tuner {
        WorkerTunerOptions::ResourceBased {
            target_memory,
            target_cpu,
        } => Some(ResourceBasedController::new(
            ResourceBasedTunerOptions::builder()
                .target_memory_usage(*target_memory)
                .target_cpu_usage(*target_cpu)
                .build(),
        )),
        WorkerTunerOptions::FixedSize { .. } => None,
    };

    let mut configured = Vec::with_capacity(specs.len());
    for (name, mut builder, overrides) in specs {
        let max_cached_workflows = overrides
            .and_then(|o| o.max_cached_workflows)
            .unwrap_or(cfg.max_cached_workflows);
        let workflow_pollers = overrides
            .and_then(|o| o.workflow_pollers)
            .or(cfg.workflow_pollers);
        let activity_pollers = overrides
            .and_then(|o| o.activity_pollers)
            .or(cfg.activity_pollers);
        let max_worker_activities_per_second = overrides
            .and_then(|o| o.max_worker_activities_per_second)
            .or(cfg.max_worker_activities_per_second);
        let effective_tuner_cfg = overrides
            .and_then(|o| o.tuner.as_ref())
            .unwrap_or(&cfg.tuner);

        validate_effective_queue_settings(
            max_cached_workflows,
            workflow_pollers,
            max_worker_activities_per_second,
            effective_tuner_cfg,
        )?;

        let tuner = build_tuner(
            effective_tuner_cfg,
            overrides
                .and_then(|o| o.tuner.as_ref())
                .is_none()
                .then_some(shared_resource_controller.as_ref())
                .flatten(),
        );

        builder.options.task_queue.clone_from(&name);
        builder.options.deployment_options = deployment_options.clone();
        builder.options.max_cached_workflows = max_cached_workflows;
        builder.options.tuner = tuner;
        builder.options.workflow_task_poller_behavior =
            workflow_pollers.map(|pollers| PollerBehavior::SimpleMaximum(pollers.get() as usize));
        builder.options.activity_task_poller_behavior =
            activity_pollers.map(|pollers| PollerBehavior::SimpleMaximum(pollers.get() as usize));
        builder.options.max_worker_activities_per_second = max_worker_activities_per_second;
        builder.options.graceful_shutdown_period = Some(cfg.graceful_shutdown_period);
        builder.options.detect_nondeterministic_futures = true;
        configured.push((name, builder.options));
    }

    Ok(configured)
}

fn build_core_workers(
    conn: &TemporalConnection,
    runtime: &Runtime,
    configured_options: Vec<(String, WorkerOptions)>,
    force_abort: &CancellationToken,
) -> Result<Vec<PreparedQueue>, WorkerPrepareError> {
    let payload_converter = conn.data_converter().payload_converter().clone();
    let raw_client = conn.raw_client();
    let mut workers = Vec::with_capacity(configured_options.len());

    for (name, mut options) in configured_options {
        options.activity_inbound_interceptor(ActivityOperationScopeInterceptor::new(
            force_abort.clone(),
        ));
        options.activity_inbound_interceptor(ForceShutdownActivityInterceptor::new(
            force_abort.clone(),
        ));
        options
            .activity_inbound_interceptor(TraceActivityInterceptor::new(payload_converter.clone()));
        options.workflow_interceptor(trace_workflow_interceptor_constructor(
            payload_converter.clone(),
        ));

        let worker = Worker::new(runtime, raw_client.clone(), options).map_err(|_| {
            tracing::error!(task_queue = %name, "temporal_core_worker_build_failed");
            WorkerPrepareError::CoreBuild
        })?;
        workers.push(PreparedQueue { name, worker });
    }

    Ok(workers)
}

struct WorkerShared {
    connection: TemporalConnection,
    graceful_stop: CancellationToken,
    force_abort: CancellationToken,
    forced: AtomicBool,
    failed: Arc<AtomicBool>,
    first_error: watch::Sender<Option<WorkerFatalError>>,
    remaining: AtomicUsize,
    all_done: watch::Sender<bool>,
}

impl WorkerShared {
    fn record_failure(&self, error: WorkerFatalError) {
        self.first_error.send_if_modified(|slot| {
            if slot.is_some() {
                return false;
            }
            *slot = Some(error);
            true
        });
        self.failed.store(true, Ordering::Release);
        self.graceful_stop.cancel();
    }

    fn finish_thread(&self) {
        if self.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.all_done.send_replace(true);
        }
    }
}

fn drive_worker_queue(queue: PreparedQueue, handle: &Handle, shared: &Arc<WorkerShared>) {
    let PreparedQueue { name, worker } = queue;
    let completion_guard = std::mem::DropGuard::new(
        (Arc::clone(shared), name.clone()),
        |(shared, task_queue)| {
            if std::thread::panicking() {
                tracing::error!(task_queue = %task_queue, "temporal_worker_thread_panicked");
                shared.record_failure(WorkerFatalError::ThreadPanicked);
            }
            shared.finish_thread();
        },
    );

    let graceful_stop = shared.graceful_stop.clone();
    let run_result = std::panic::catch_unwind(AssertUnwindSafe(move || {
        handle.block_on(worker.run_until(graceful_stop.cancelled()))
    }));

    match run_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::error!(
                task_queue = %name,
                reason = classify_worker_run_error(&error),
                "temporal_worker_failed"
            );
            shared.record_failure(WorkerFatalError::Fatal);
        }
        Err(_) => {
            tracing::error!(task_queue = %name, "temporal_worker_thread_panicked");
            shared.record_failure(WorkerFatalError::ThreadPanicked);
        }
    }
    drop(completion_guard);
}

const fn classify_worker_run_error(error: &WorkerRunError) -> &'static str {
    match error {
        WorkerRunError::Validation(_) => "validation",
        WorkerRunError::Fatal { .. } => "fatal",
        _ => "unknown",
    }
}

pub struct TemporalWorkerHandle {
    shared: Arc<WorkerShared>,
    _runtime: Runtime,
    threads: Vec<ThreadJoinHandle<()>>,
}

impl fmt::Debug for TemporalWorkerHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TemporalWorkerHandle")
            .field("failed", &self.has_failed())
            .field("forced", &self.shared.forced.load(Ordering::Acquire))
            .field(
                "remaining_queues",
                &self.shared.remaining.load(Ordering::Acquire),
            )
            .finish_non_exhaustive()
    }
}

impl TemporalWorkerHandle {
    #[must_use]
    pub fn probe(&self) -> TemporalProbe {
        self.shared
            .connection
            .probe_with_failure(Arc::clone(&self.shared.failed))
    }

    pub fn initiate_shutdown(&self) {
        self.shared.connection.mark_draining();
        self.shared.graceful_stop.cancel();
    }

    pub fn force_cancel(&self) {
        self.initiate_shutdown();
        if !self.shared.forced.swap(true, Ordering::AcqRel) {
            WorkspaceMetricsMeter::record_drain_forced();
            self.shared.force_abort.cancel();
        }
    }

    pub async fn drained(&self) {
        let mut rx = self.shared.all_done.subscribe();
        let _ = rx.wait_for(|done| *done).await;
    }

    #[must_use]
    pub fn has_failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire)
    }

    pub async fn wait_failed(&self) -> WorkerFatalError {
        let mut rx = self.shared.first_error.subscribe();
        if let Ok(guard) = rx.wait_for(Option::is_some).await
            && let Some(error) = guard.clone()
        {
            return error;
        }
        std::future::pending().await
    }

    pub async fn finish(&mut self, deadline: Instant) -> Result<CloseOutcome, WorkerFatalError> {
        self.initiate_shutdown();
        if tokio::time::timeout_at(deadline, self.drained())
            .await
            .is_err()
        {
            return self.first_error().map_or(Ok(CloseOutcome::TimedOut), Err);
        }

        let threads = std::mem::take(&mut self.threads);
        if !threads.is_empty() {
            let join_task = tokio::task::spawn_blocking(move || {
                for thread in threads {
                    let _ = thread.join();
                }
            });
            match tokio::time::timeout_at(deadline, join_task).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Err(WorkerFatalError::ThreadPanicked),
                Err(_) => return self.first_error().map_or(Ok(CloseOutcome::TimedOut), Err),
            }
        }

        self.first_error().map_or(Ok(CloseOutcome::Complete), Err)
    }

    fn first_error(&self) -> Option<WorkerFatalError> {
        self.shared.first_error.borrow().clone()
    }
}

impl Drop for TemporalWorkerHandle {
    fn drop(&mut self) {
        self.shared.graceful_stop.cancel();
        self.shared.force_abort.cancel();
    }
}

pub(crate) fn build_runtime(meter: &WorkspaceMetricsMeter) -> Result<Runtime, RuntimeError> {
    let options = RuntimeOptions::builder()
        .telemetry_options(meter.telemetry_options())
        .heartbeat_interval(None)
        .build()
        .map_err(|err| RuntimeError::Initialization(err.into()))?;
    Runtime::from_current_tokio(options)
}

#[cfg(test)]
pub(crate) fn spawn_worker_thread(
    name: String,
    handle: Handle,
    worker: Worker,
    shutdown: CancellationToken,
) -> std::io::Result<ThreadJoinHandle<Result<(), WorkerRunError>>> {
    const fn assert_send<T: Send>() {}
    assert_send::<Worker>();
    assert_send::<Runtime>();
    assert_send::<CoreRuntime>();

    std::thread::Builder::new()
        .name(name)
        .spawn(move || handle.block_on(worker.run_until(shutdown.cancelled())))
}

fn build_deployment_options(
    build: &WorkerDeploymentMetadata,
) -> Result<WorkerDeploymentOptions, WorkerPrepareError> {
    let deployment_name = build.deployment_name.trim();
    if deployment_name.is_empty() || deployment_name.chars().any(char::is_control) {
        return Err(WorkerPrepareError::InvalidConfiguration(
            "deployment_name must be non-blank without control characters",
        ));
    }
    let build_id = build.build_id.trim();
    if build_id.is_empty() || build_id.chars().any(char::is_control) {
        return Err(WorkerPrepareError::InvalidConfiguration(
            "build_id must be non-blank without control characters",
        ));
    }
    if build.use_worker_versioning
        && build.default_versioning_behavior == VersioningBehaviorMode::Unspecified
    {
        return Err(WorkerPrepareError::InvalidConfiguration(
            "use_worker_versioning requires pinned or auto_upgrade default_versioning_behavior",
        ));
    }
    if !build.use_worker_versioning
        && build.default_versioning_behavior != VersioningBehaviorMode::Unspecified
    {
        return Err(WorkerPrepareError::InvalidConfiguration(
            "default_versioning_behavior must be unspecified when use_worker_versioning is false",
        ));
    }
    let default_versioning_behavior = match build.default_versioning_behavior {
        VersioningBehaviorMode::Unspecified => None,
        VersioningBehaviorMode::Pinned => Some(VersioningBehavior::Pinned),
        VersioningBehaviorMode::AutoUpgrade => Some(VersioningBehavior::AutoUpgrade),
    };
    Ok(WorkerDeploymentOptions::new(
        WorkerDeploymentVersion::builder()
            .deployment_name(deployment_name.to_owned())
            .build_id(build_id.to_owned())
            .build(),
    )
    .use_worker_versioning(build.use_worker_versioning)
    .maybe_default_versioning_behavior(default_versioning_behavior)
    .build())
}

fn validate_top_level_options(cfg: &TemporalWorkerOptions) -> Result<(), WorkerPrepareError> {
    if let Some(task_queue) = cfg.task_queue.as_deref() {
        validate_queue_name(task_queue)?;
    }
    if cfg.graceful_shutdown_period.is_zero() {
        return Err(WorkerPrepareError::InvalidConfiguration(
            "graceful_shutdown_period must be positive",
        ));
    }
    if cfg.max_cached_workflows > MAX_CACHED_WORKFLOWS_LIMIT {
        return Err(WorkerPrepareError::InvalidConfiguration(
            "max_cached_workflows exceeds limit",
        ));
    }
    if let Some(rate) = cfg.max_worker_activities_per_second
        && (!rate.is_finite() || rate <= 0.0)
    {
        return Err(WorkerPrepareError::InvalidConfiguration(
            "max_worker_activities_per_second must be finite and positive",
        ));
    }
    validate_tuner_options(&cfg.tuner)
}

fn validate_queue_name(name: &str) -> Result<(), WorkerPrepareError> {
    if name.trim().is_empty()
        || name.len() > MAX_QUEUE_NAME_BYTES
        || name.trim() != name
        || !name.bytes().all(|byte| (0x20..=0x7E).contains(&byte))
    {
        return Err(WorkerPrepareError::InvalidConfiguration(
            "task queue name must be non-blank ASCII printable within 255 bytes",
        ));
    }
    Ok(())
}

fn validate_tuner_options(tuner: &WorkerTunerOptions) -> Result<(), WorkerPrepareError> {
    match tuner {
        WorkerTunerOptions::ResourceBased {
            target_memory,
            target_cpu,
        } => {
            if !target_memory.is_finite()
                || !(MIN_TUNER_RATIO..=MAX_TUNER_RATIO).contains(target_memory)
            {
                return Err(WorkerPrepareError::InvalidConfiguration(
                    "tuner target_memory must be finite between 0.05 and 0.95",
                ));
            }
            if !target_cpu.is_finite() || !(MIN_TUNER_RATIO..=MAX_TUNER_RATIO).contains(target_cpu)
            {
                return Err(WorkerPrepareError::InvalidConfiguration(
                    "tuner target_cpu must be finite between 0.05 and 0.95",
                ));
            }
        }
        WorkerTunerOptions::FixedSize {
            max_concurrent_workflow_tasks,
            max_concurrent_activities,
            max_concurrent_local_activities,
        } => {
            if *max_concurrent_workflow_tasks == 0
                || *max_concurrent_activities == 0
                || *max_concurrent_local_activities == 0
            {
                return Err(WorkerPrepareError::InvalidConfiguration(
                    "fixed_size tuner slots must be at least 1",
                ));
            }
        }
    }
    Ok(())
}

fn validate_effective_queue_settings(
    max_cached_workflows: usize,
    workflow_pollers: Option<NonZeroU32>,
    max_worker_activities_per_second: Option<f64>,
    tuner: &WorkerTunerOptions,
) -> Result<(), WorkerPrepareError> {
    if max_cached_workflows > MAX_CACHED_WORKFLOWS_LIMIT {
        return Err(WorkerPrepareError::InvalidConfiguration(
            "max_cached_workflows exceeds limit",
        ));
    }
    if let Some(rate) = max_worker_activities_per_second
        && (!rate.is_finite() || rate <= 0.0)
    {
        return Err(WorkerPrepareError::InvalidConfiguration(
            "max_worker_activities_per_second must be finite and positive",
        ));
    }
    validate_tuner_options(tuner)?;
    if max_cached_workflows > 0 {
        if workflow_pollers.is_some_and(|pollers| pollers.get() < 2) {
            return Err(WorkerPrepareError::InvalidConfiguration(
                "workflow_pollers must be at least 2 when max_cached_workflows > 0",
            ));
        }
        if let WorkerTunerOptions::FixedSize {
            max_concurrent_workflow_tasks,
            ..
        } = tuner
            && *max_concurrent_workflow_tasks < 2
        {
            return Err(WorkerPrepareError::InvalidConfiguration(
                "max_concurrent_workflow_tasks must be at least 2 when max_cached_workflows > 0",
            ));
        }
    }
    Ok(())
}

fn build_tuner(
    tuner: &WorkerTunerOptions,
    shared_controller: Option<&ResourceBasedController>,
) -> WorkerTuner {
    match tuner {
        WorkerTunerOptions::ResourceBased {
            target_memory,
            target_cpu,
        } => {
            if let Some(controller) = shared_controller {
                ResourceBasedTunerWithController::builder()
                    .controller(controller.clone())
                    .build()
                    .into()
            } else {
                ResourceBasedTuner::builder()
                    .tuner_options(
                        ResourceBasedTunerOptions::builder()
                            .target_memory_usage(*target_memory)
                            .target_cpu_usage(*target_cpu)
                            .build(),
                    )
                    .build()
                    .into()
            }
        }
        WorkerTunerOptions::FixedSize {
            max_concurrent_workflow_tasks,
            max_concurrent_activities,
            max_concurrent_local_activities,
        } => TunerHolder::builder()
            .workflow_task_slot_supplier(FixedSizeSlotSupplier::new(*max_concurrent_workflow_tasks))
            .activity_task_slot_supplier(FixedSizeSlotSupplier::new(*max_concurrent_activities))
            .local_activity_task_slot_supplier(FixedSizeSlotSupplier::new(
                *max_concurrent_local_activities,
            ))
            .nexus_task_slot_supplier(FixedSizeSlotSupplier::new(1))
            .build()
            .into(),
    }
}
