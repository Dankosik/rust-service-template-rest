use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::time::Duration;

use serde::Deserialize;

use crate::de::{StrictBool, StrictCount, StrictDuration, StrictRatio};
use crate::load::{ADDRESSABLE_KEY_FORM, is_env_addressable};
use crate::temporal::{TemporalClientConfig, validate_ascii_printable_name};
use crate::validate::{
    Rejected, ValidationError, Violations, admit_with, duration_range, int_range, non_blank,
    required,
};
use crate::{
    AppConfig, BuildInfo, HealthConfig, LogConfig, ObservabilityConfig, RuntimeConfig,
    ShutdownTail, ValidationErrors, WorkerHttpConfig,
};

const DEFAULT_MAX_CACHED_WORKFLOWS: usize = 1000;
const MAX_CACHED_WORKFLOWS_LIMIT: usize = 100_000;
const MAX_QUEUE_NAME_BYTES: usize = 255;
const DEFAULT_TARGET_MEMORY: f64 = 0.75;
const DEFAULT_TARGET_CPU: f64 = 0.80;
const MIN_TUNER_RATIO: f64 = 0.05;
const MAX_TUNER_RATIO: f64 = 0.95;
const MIN_SHUTDOWN_STAGE: Duration = Duration::from_millis(100);
const MAX_SHUTDOWN_STAGE: Duration = Duration::from_mins(10);

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TemporalTunerMode {
    #[default]
    ResourceBased,
    FixedSize,
}

#[serde_with::serde_as]
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct TemporalTunerInput {
    mode: TemporalTunerMode,
    #[serde_as(as = "Option<StrictRatio>")]
    target_memory: Option<f64>,
    #[serde_as(as = "Option<StrictRatio>")]
    target_cpu: Option<f64>,
    #[serde_as(as = "Option<StrictCount>")]
    max_concurrent_workflow_tasks: Option<usize>,
    #[serde_as(as = "Option<StrictCount>")]
    max_concurrent_activities: Option<usize>,
    #[serde_as(as = "Option<StrictCount>")]
    max_concurrent_local_activities: Option<usize>,
}

impl TemporalTunerInput {
    fn admit(self, prefix: &str, v: &mut Violations) -> Result<TemporalTunerConfig, Rejected> {
        match self.mode {
            TemporalTunerMode::ResourceBased => {
                let wf_key = format!("{prefix}.max_concurrent_workflow_tasks");
                let act_key = format!("{prefix}.max_concurrent_activities");
                let local_key = format!("{prefix}.max_concurrent_local_activities");
                v.reject_present(
                    &[
                        (&wf_key, self.max_concurrent_workflow_tasks.is_some()),
                        (&act_key, self.max_concurrent_activities.is_some()),
                        (&local_key, self.max_concurrent_local_activities.is_some()),
                    ],
                    "is only valid when tuner.mode = fixed_size",
                );
                let target_memory = self.target_memory.unwrap_or(DEFAULT_TARGET_MEMORY);
                let target_cpu = self.target_cpu.unwrap_or(DEFAULT_TARGET_CPU);
                v.check(validate_tuner_ratio(
                    &format!("{prefix}.target_memory"),
                    target_memory,
                ));
                v.check(validate_tuner_ratio(
                    &format!("{prefix}.target_cpu"),
                    target_cpu,
                ));
                Ok(TemporalTunerConfig::ResourceBased {
                    target_memory,
                    target_cpu,
                })
            }
            TemporalTunerMode::FixedSize => {
                let mem_key = format!("{prefix}.target_memory");
                let cpu_key = format!("{prefix}.target_cpu");
                v.reject_present(
                    &[
                        (&mem_key, self.target_memory.is_some()),
                        (&cpu_key, self.target_cpu.is_some()),
                    ],
                    "is only valid when tuner.mode = resource_based",
                );
                let wf_key = format!("{prefix}.max_concurrent_workflow_tasks");
                let act_key = format!("{prefix}.max_concurrent_activities");
                let local_key = format!("{prefix}.max_concurrent_local_activities");
                let wf = v.record(required_min_one(
                    &wf_key,
                    self.max_concurrent_workflow_tasks,
                ));
                let act = v.record(required_min_one(&act_key, self.max_concurrent_activities));
                let local = v.record(required_min_one(
                    &local_key,
                    self.max_concurrent_local_activities,
                ));
                Ok(TemporalTunerConfig::FixedSize {
                    max_concurrent_workflow_tasks: wf?,
                    max_concurrent_activities: act?,
                    max_concurrent_local_activities: local?,
                })
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TemporalTunerConfig {
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

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TemporalVersioningBehavior {
    #[default]
    Unspecified,
    Pinned,
    AutoUpgrade,
}

#[serde_with::serde_as]
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct TemporalDeploymentInput {
    deployment_name: Option<String>,
    build_id: Option<String>,
    #[serde_as(as = "StrictBool")]
    use_worker_versioning: bool,
    default_versioning_behavior: TemporalVersioningBehavior,
}

impl TemporalDeploymentInput {
    fn admit(
        self,
        service_name: &str,
        app_version: &str,
        app_commit: &str,
        v: &mut Violations,
    ) -> TemporalDeploymentConfig {
        let deployment_name = self
            .deployment_name
            .unwrap_or_else(|| service_name.to_owned());
        let build_id = self
            .build_id
            .unwrap_or_else(|| format!("{app_version}-{app_commit}"));
        let deployment_name = validate_non_blank_no_controls(
            "temporal_worker.deployment.deployment_name",
            &deployment_name,
            v,
        );
        let build_id =
            validate_non_blank_no_controls("temporal_worker.deployment.build_id", &build_id, v);
        if self.use_worker_versioning
            && self.default_versioning_behavior == TemporalVersioningBehavior::Unspecified
        {
            v.reject(ValidationError::new(
                "temporal_worker.deployment.default_versioning_behavior",
                "must be pinned or auto_upgrade when temporal_worker.deployment.use_worker_versioning = true",
            ));
        }
        TemporalDeploymentConfig {
            deployment_name,
            build_id,
            use_worker_versioning: self.use_worker_versioning,
            default_versioning_behavior: self.default_versioning_behavior,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalDeploymentConfig {
    pub deployment_name: String,
    pub build_id: String,
    pub use_worker_versioning: bool,
    pub default_versioning_behavior: TemporalVersioningBehavior,
}

#[serde_with::serde_as]
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct TemporalQueueInput {
    #[serde_as(as = "Option<StrictCount>")]
    max_cached_workflows: Option<usize>,
    #[serde_as(as = "Option<StrictCount>")]
    workflow_pollers: Option<NonZeroU32>,
    #[serde_as(as = "Option<StrictCount>")]
    activity_pollers: Option<NonZeroU32>,
    #[serde_as(as = "Option<StrictRatio>")]
    max_worker_activities_per_second: Option<f64>,
    tuner: Option<TemporalTunerInput>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TemporalQueueConfig {
    pub max_cached_workflows: Option<usize>,
    pub workflow_pollers: Option<NonZeroU32>,
    pub activity_pollers: Option<NonZeroU32>,
    pub max_worker_activities_per_second: Option<f64>,
    pub tuner: Option<TemporalTunerConfig>,
}

#[serde_with::serde_as]
#[derive(Debug, Deserialize, educe::Educe)]
#[educe(Default)]
#[serde(deny_unknown_fields, default)]
struct TemporalWorkerRuntimeInput {
    task_queue: Option<String>,
    queues: BTreeMap<String, TemporalQueueInput>,
    #[serde_as(as = "StrictCount")]
    #[educe(Default = DEFAULT_MAX_CACHED_WORKFLOWS)]
    max_cached_workflows: usize,
    #[serde_as(as = "Option<StrictCount>")]
    workflow_pollers: Option<NonZeroU32>,
    #[serde_as(as = "Option<StrictCount>")]
    activity_pollers: Option<NonZeroU32>,
    #[serde_as(as = "Option<StrictRatio>")]
    max_worker_activities_per_second: Option<f64>,
    tuner: TemporalTunerInput,
    deployment: TemporalDeploymentInput,
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(45)))]
    graceful_shutdown_period: Duration,
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(10)))]
    activity_cancel_cleanup: Duration,
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(5)))]
    finalize_budget: Duration,
}

impl TemporalWorkerRuntimeInput {
    fn admit(
        self,
        http_drain_timeout: Option<Duration>,
        service_name: &str,
        app_version: &str,
        app_commit: &str,
        v: &mut Violations,
    ) -> Result<TemporalWorkerRuntimeConfig, Rejected> {
        let task_queue = match self.task_queue {
            Some(raw) => v
                .record(validate_ascii_printable_name(
                    "temporal_worker.task_queue",
                    &raw,
                ))
                .map(Some),
            None => Ok(None),
        };
        v.check(int_range(
            "temporal_worker.max_cached_workflows",
            self.max_cached_workflows,
            0..=MAX_CACHED_WORKFLOWS_LIMIT,
        ));
        if let Some(rate) = self.max_worker_activities_per_second {
            v.check(validate_positive_rate(
                "temporal_worker.max_worker_activities_per_second",
                rate,
            ));
        }

        let tuner = self.tuner.admit("temporal_worker.tuner", v);
        validate_top_level_sticky_bounds(
            self.max_cached_workflows,
            self.workflow_pollers,
            tuner.as_ref().ok(),
            v,
        );

        let default_queue_name = task_queue.as_ref().ok().and_then(Option::as_deref);
        let sticky_defaults = StickyDefaults {
            max_cached_workflows: self.max_cached_workflows,
            workflow_pollers: self.workflow_pollers,
            tuner: tuner.as_ref().ok(),
        };
        let queues = admit_queues(self.queues, default_queue_name, &sticky_defaults, v);
        let deployment = self
            .deployment
            .admit(service_name, app_version, app_commit, v);
        validate_shutdown_budgets(
            self.graceful_shutdown_period,
            self.activity_cancel_cleanup,
            self.finalize_budget,
            http_drain_timeout,
            v,
        );

        Ok(TemporalWorkerRuntimeConfig {
            task_queue: task_queue?,
            queues,
            max_cached_workflows: self.max_cached_workflows,
            workflow_pollers: self.workflow_pollers,
            activity_pollers: self.activity_pollers,
            max_worker_activities_per_second: self.max_worker_activities_per_second,
            tuner: tuner?,
            deployment,
            graceful_shutdown_period: self.graceful_shutdown_period,
            activity_cancel_cleanup: self.activity_cancel_cleanup,
            finalize_budget: self.finalize_budget,
        })
    }
}

fn validate_top_level_sticky_bounds(
    max_cached_workflows: usize,
    workflow_pollers: Option<NonZeroU32>,
    tuner: Option<&TemporalTunerConfig>,
    v: &mut Violations,
) {
    if max_cached_workflows == 0 {
        return;
    }
    if workflow_pollers.is_some_and(|pollers| pollers.get() < 2) {
        v.reject(ValidationError::new(
            "temporal_worker.workflow_pollers",
            "must be at least 2 when temporal_worker.max_cached_workflows > 0",
        ));
    }
    if let Some(TemporalTunerConfig::FixedSize {
        max_concurrent_workflow_tasks,
        ..
    }) = tuner
        && *max_concurrent_workflow_tasks < 2
    {
        v.reject(ValidationError::new(
            "temporal_worker.tuner.max_concurrent_workflow_tasks",
            "must be at least 2 when temporal_worker.max_cached_workflows > 0",
        ));
    }
}

struct StickyDefaults<'a> {
    max_cached_workflows: usize,
    workflow_pollers: Option<NonZeroU32>,
    tuner: Option<&'a TemporalTunerConfig>,
}

fn admit_queues(
    raw_queues: BTreeMap<String, TemporalQueueInput>,
    default_queue_name: Option<&str>,
    defaults: &StickyDefaults<'_>,
    v: &mut Violations,
) -> BTreeMap<String, TemporalQueueConfig> {
    let mut queues = BTreeMap::new();
    for (name, queue_input) in raw_queues {
        if !is_valid_queue_key(&name) {
            v.reject(ValidationError::new(
                "temporal_worker.queues",
                format!(
                    "queue keys must be at most {MAX_QUEUE_NAME_BYTES} bytes of {ADDRESSABLE_KEY_FORM}"
                ),
            ));
            continue;
        }
        let prefix = format!("temporal_worker.queues.{name}");
        if default_queue_name == Some(name.as_str()) {
            v.reject(ValidationError::new(
                &prefix,
                "collides with temporal_worker.task_queue",
            ));
        }
        if let Some(max_cached) = queue_input.max_cached_workflows {
            v.check(int_range(
                &format!("{prefix}.max_cached_workflows"),
                max_cached,
                0..=MAX_CACHED_WORKFLOWS_LIMIT,
            ));
        }
        if let Some(rate) = queue_input.max_worker_activities_per_second {
            v.check(validate_positive_rate(
                &format!("{prefix}.max_worker_activities_per_second"),
                rate,
            ));
        }
        let queue_supplied_tuner = queue_input.tuner.is_some();
        let queue_tuner = match queue_input.tuner {
            Some(tuner_input) => tuner_input.admit(&format!("{prefix}.tuner"), v).ok(),
            None => None,
        };
        let admitted = TemporalQueueConfig {
            max_cached_workflows: queue_input.max_cached_workflows,
            workflow_pollers: queue_input.workflow_pollers,
            activity_pollers: queue_input.activity_pollers,
            max_worker_activities_per_second: queue_input.max_worker_activities_per_second,
            tuner: queue_tuner,
        };
        validate_queue_sticky_bounds(&prefix, &admitted, queue_supplied_tuner, defaults, v);
        queues.insert(name, admitted);
    }
    queues
}

fn validate_queue_sticky_bounds(
    prefix: &str,
    queue: &TemporalQueueConfig,
    queue_supplied_tuner: bool,
    defaults: &StickyDefaults<'_>,
    v: &mut Violations,
) {
    let effective_cached = queue
        .max_cached_workflows
        .unwrap_or(defaults.max_cached_workflows);
    if effective_cached == 0 {
        return;
    }
    let effective_wf_pollers = queue.workflow_pollers.or(defaults.workflow_pollers);
    if effective_wf_pollers.is_some_and(|pollers| pollers.get() < 2)
        && (queue.workflow_pollers.is_some() || queue.max_cached_workflows.is_some())
    {
        let key = if queue.workflow_pollers.is_some() {
            format!("{prefix}.workflow_pollers")
        } else {
            format!("{prefix}.max_cached_workflows")
        };
        v.reject(ValidationError::new(
            key,
            "effective workflow_pollers must be at least 2 when effective max_cached_workflows > 0",
        ));
    }
    let effective_tuner = if queue_supplied_tuner {
        queue.tuner.as_ref()
    } else {
        defaults.tuner
    };
    if let Some(TemporalTunerConfig::FixedSize {
        max_concurrent_workflow_tasks,
        ..
    }) = effective_tuner
        && *max_concurrent_workflow_tasks < 2
        && (queue.tuner.is_some() || queue.max_cached_workflows.is_some())
    {
        let key = if queue.tuner.is_some() {
            format!("{prefix}.tuner.max_concurrent_workflow_tasks")
        } else {
            format!("{prefix}.max_cached_workflows")
        };
        v.reject(ValidationError::new(
            key,
            "effective max_concurrent_workflow_tasks must be at least 2 when effective max_cached_workflows > 0",
        ));
    }
}

fn validate_shutdown_budgets(
    graceful_shutdown_period: Duration,
    activity_cancel_cleanup: Duration,
    finalize_budget: Duration,
    http_drain_timeout: Option<Duration>,
    v: &mut Violations,
) {
    let stage_range = MIN_SHUTDOWN_STAGE..=MAX_SHUTDOWN_STAGE;
    let graceful_ok = v
        .record(duration_range(
            "temporal_worker.graceful_shutdown_period",
            graceful_shutdown_period,
            stage_range.clone(),
        ))
        .is_ok();
    let cleanup_ok = v
        .record(duration_range(
            "temporal_worker.activity_cancel_cleanup",
            activity_cancel_cleanup,
            stage_range.clone(),
        ))
        .is_ok();
    let finalize_ok = v
        .record(duration_range(
            "temporal_worker.finalize_budget",
            finalize_budget,
            stage_range,
        ))
        .is_ok();
    if graceful_ok
        && cleanup_ok
        && finalize_ok
        && let Some(drain_timeout) = http_drain_timeout
    {
        let total_worker_drain = graceful_shutdown_period
            .saturating_add(activity_cancel_cleanup)
            .saturating_add(finalize_budget);
        if total_worker_drain > drain_timeout {
            v.reject(ValidationError::new(
                "temporal_worker.graceful_shutdown_period",
                format!(
                    "graceful_shutdown_period ({}) + activity_cancel_cleanup ({}) + finalize_budget ({}) = {} must be <= http.drain_timeout ({})",
                    crate::de::display_duration(graceful_shutdown_period),
                    crate::de::display_duration(activity_cancel_cleanup),
                    crate::de::display_duration(finalize_budget),
                    crate::de::display_duration(total_worker_drain),
                    crate::de::display_duration(drain_timeout),
                ),
            ));
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TemporalWorkerRuntimeConfig {
    task_queue: Option<String>,
    queues: BTreeMap<String, TemporalQueueConfig>,
    max_cached_workflows: usize,
    workflow_pollers: Option<NonZeroU32>,
    activity_pollers: Option<NonZeroU32>,
    max_worker_activities_per_second: Option<f64>,
    tuner: TemporalTunerConfig,
    deployment: TemporalDeploymentConfig,
    graceful_shutdown_period: Duration,
    activity_cancel_cleanup: Duration,
    finalize_budget: Duration,
}

impl TemporalWorkerRuntimeConfig {
    #[must_use]
    pub fn task_queue(&self) -> Option<&str> {
        self.task_queue.as_deref()
    }

    #[must_use]
    pub const fn queues(&self) -> &BTreeMap<String, TemporalQueueConfig> {
        &self.queues
    }

    #[must_use]
    pub const fn max_cached_workflows(&self) -> usize {
        self.max_cached_workflows
    }

    #[must_use]
    pub const fn workflow_pollers(&self) -> Option<NonZeroU32> {
        self.workflow_pollers
    }

    #[must_use]
    pub const fn activity_pollers(&self) -> Option<NonZeroU32> {
        self.activity_pollers
    }

    #[must_use]
    pub const fn max_worker_activities_per_second(&self) -> Option<f64> {
        self.max_worker_activities_per_second
    }

    #[must_use]
    pub const fn tuner(&self) -> &TemporalTunerConfig {
        &self.tuner
    }

    #[must_use]
    pub const fn deployment(&self) -> &TemporalDeploymentConfig {
        &self.deployment
    }

    #[must_use]
    pub const fn graceful_shutdown_period(&self) -> Duration {
        self.graceful_shutdown_period
    }

    #[must_use]
    pub const fn activity_cancel_cleanup(&self) -> Duration {
        self.activity_cancel_cleanup
    }

    #[must_use]
    pub const fn finalize_budget(&self) -> Duration {
        self.finalize_budget
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct TemporalWorkerInput {
    app: crate::app::AppInput,
    log: LogConfig,
    runtime: RuntimeConfig,
    health: HealthConfig,
    observability: crate::observability::ObservabilityInput,
    http: crate::http::TemporalWorkerHttpInput,
    temporal: crate::temporal::TemporalInput,
    temporal_worker: TemporalWorkerRuntimeInput,
}

impl TemporalWorkerInput {
    pub(crate) fn admit(
        self,
        build: BuildInfo,
        shutdown_tail: ShutdownTail,
    ) -> Result<TemporalWorkerConfig, ValidationErrors> {
        admit_with(|v| {
            let app = self.app.admit(build, v);
            let http_drain_timeout = self.http.valid_drain_timeout();
            let http = self.http.admit(shutdown_tail, v);
            self.health.validate(v);
            self.log.validate(v);
            self.runtime.validate(v);
            let observability = self.observability.admit(v);
            let temporal = self.temporal.admit(
                app.is_local_development(),
                &observability.otel.service_name,
                &app.version,
                v,
            );
            let temporal_worker = self.temporal_worker.admit(
                http_drain_timeout,
                &observability.otel.service_name,
                &app.version,
                &app.commit,
                v,
            );
            Ok(TemporalWorkerConfig {
                app,
                log: self.log,
                runtime: self.runtime,
                health: self.health,
                observability,
                http: http?,
                temporal: temporal?,
                temporal_worker: temporal_worker?,
            })
        })
    }
}

#[derive(Clone, Debug)]
pub struct TemporalWorkerConfig {
    app: AppConfig,
    log: LogConfig,
    runtime: RuntimeConfig,
    health: HealthConfig,
    observability: ObservabilityConfig,
    http: WorkerHttpConfig,
    temporal: Option<TemporalClientConfig>,
    temporal_worker: TemporalWorkerRuntimeConfig,
}

impl TemporalWorkerConfig {
    #[must_use]
    pub const fn app(&self) -> &AppConfig {
        &self.app
    }

    #[must_use]
    pub const fn log(&self) -> &LogConfig {
        &self.log
    }

    #[must_use]
    pub const fn runtime(&self) -> &RuntimeConfig {
        &self.runtime
    }

    #[must_use]
    pub const fn health(&self) -> &HealthConfig {
        &self.health
    }

    #[must_use]
    pub const fn observability(&self) -> &ObservabilityConfig {
        &self.observability
    }

    #[must_use]
    pub const fn http(&self) -> &WorkerHttpConfig {
        &self.http
    }

    #[must_use]
    pub const fn temporal(&self) -> Option<&TemporalClientConfig> {
        self.temporal.as_ref()
    }

    #[must_use]
    pub const fn temporal_worker(&self) -> &TemporalWorkerRuntimeConfig {
        &self.temporal_worker
    }

    #[must_use]
    pub fn finite_listener_connection_caps(&self) -> u64 {
        crate::finite_http_connection_caps(self.http.listener(), &self.observability.metrics)
    }
}

fn is_valid_queue_key(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_QUEUE_NAME_BYTES
        && !name.chars().any(char::is_control)
        && name.bytes().all(|byte| matches!(byte, 0x20..=0x7E))
        && is_env_addressable(name)
}

fn validate_tuner_ratio(key: &str, value: f64) -> Result<(), ValidationError> {
    if !value.is_finite() {
        return Err(ValidationError::new(key, "must be finite"));
    }
    if !(MIN_TUNER_RATIO..=MAX_TUNER_RATIO).contains(&value) {
        return Err(ValidationError::new(
            key,
            format!("must be between {MIN_TUNER_RATIO} and {MAX_TUNER_RATIO}, got {value}"),
        ));
    }
    Ok(())
}

fn required_min_one(key: &str, value: Option<usize>) -> Result<usize, ValidationError> {
    let slots = required(key, value, "tuner.mode = fixed_size")?;
    int_range(key, slots, 1..=usize::MAX)?;
    Ok(slots)
}

fn validate_positive_rate(key: &str, value: f64) -> Result<(), ValidationError> {
    if !value.is_finite() || value <= 0.0 {
        return Err(ValidationError::new(
            key,
            "must be a finite positive number",
        ));
    }
    Ok(())
}

fn validate_non_blank_no_controls(key: &str, value: &str, v: &mut Violations) -> String {
    if value.chars().any(char::is_control) {
        v.reject(ValidationError::new(
            key,
            "must not contain control characters",
        ));
    } else {
        v.check(non_blank(key, value));
    }
    value.trim().to_owned()
}

#[cfg(test)]
mod tests;
