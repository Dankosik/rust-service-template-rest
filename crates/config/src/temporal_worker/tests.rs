use std::assert_matches;

use super::*;
use crate::LoadOptions;
use crate::load::{
    ErrorKind, InputError, load_from, load_temporal_worker_from,
    load_temporal_worker_with_tail_from,
};
// template:begin worker:temporal-worker-tests-import
use crate::test_support::load_worker_env;
// template:end worker:temporal-worker-tests-import
use crate::test_support::{BUILD, env, load_env, load_temporal_worker_env, write};

const TEMPORAL_TAIL: ShutdownTail =
    ShutdownTail::new(Duration::from_millis(16_500), "temporal shutdown tail");

fn load_with_tail(pairs: &[(&str, &str)]) -> Result<TemporalWorkerConfig, crate::Error> {
    load_temporal_worker_with_tail_from(&LoadOptions::default(), BUILD, TEMPORAL_TAIL, env(pairs))
}

#[test]
fn defaults_match_temporal_worker_budgets_while_service_and_jobs_worker_stay_at_45s_and_25s() {
    let service = load_env(&[]);
    assert_eq!(service.http().grace_period, Duration::from_secs(45));
    assert_eq!(service.http().drain_timeout, Duration::from_secs(25));

    // template:begin worker:temporal-worker-tests-jobs-worker
    let jobs_worker = load_worker_env(&[]);
    assert_eq!(jobs_worker.http().grace_period, Duration::from_secs(45));
    assert_eq!(jobs_worker.http().drain_timeout, Duration::from_secs(25));
    // template:end worker:temporal-worker-tests-jobs-worker

    let worker = load_with_tail(&[]).unwrap();
    assert_eq!(worker.app().version, "1.2.3");
    assert_eq!(worker.app().commit, "abc123");
    assert_eq!(worker.http().grace_period, Duration::from_secs(90));
    assert_eq!(worker.http().drain_timeout, Duration::from_secs(60));
    assert!(worker.temporal().is_none());

    let rt = worker.temporal_worker();
    assert_eq!(rt.task_queue(), None);
    assert!(rt.queues().is_empty());
    assert_eq!(rt.max_cached_workflows(), 1000);
    assert_eq!(rt.workflow_pollers(), None);
    assert_eq!(rt.activity_pollers(), None);
    assert_eq!(rt.max_worker_activities_per_second(), None);
    assert_eq!(
        rt.tuner(),
        &TemporalTunerConfig::ResourceBased {
            target_memory: 0.75,
            target_cpu: 0.80,
        }
    );
    assert_eq!(
        rt.deployment(),
        &TemporalDeploymentConfig {
            deployment_name: "service".to_owned(),
            build_id: "1.2.3-abc123".to_owned(),
            use_worker_versioning: false,
            default_versioning_behavior: TemporalVersioningBehavior::Unspecified,
        }
    );
    assert_eq!(rt.graceful_shutdown_period(), Duration::from_secs(45));
    assert_eq!(rt.activity_cancel_cleanup(), Duration::from_secs(10));
    assert_eq!(rt.finalize_budget(), Duration::from_secs(5));
}

#[rstest::rstest]
#[case::health_only("4", "", 4)]
#[case::with_diagnostics("4", ":9090", 8)]
#[case::uncapped("0", ":9090", 0)]
fn temporal_worker_counts_finite_caps_of_enabled_listeners(
    #[case] http_max_connections: &str,
    #[case] diagnostics_addr: &str,
    #[case] expected: u64,
) {
    let cfg = load_temporal_worker_env(&[
        ("APP__HTTP__MAX_CONNECTIONS", http_max_connections),
        ("APP__OBSERVABILITY__METRICS__ADDR", diagnostics_addr),
    ]);
    assert_eq!(cfg.finite_listener_connection_caps(), expected);
}

#[test]
fn shared_config_file_loads_in_both_service_and_temporal_worker_and_rejects_unknown_keys() {
    let dir = tempfile::tempdir().unwrap();
    let shared = write(
        &dir,
        "shared.toml",
        concat!(
            "[app]\nenv = \"local\"\nallow_insecure_development = true\n",
            "[http]\nreadiness_propagation_delay = \"10s\"\n",
            "[temporal]\nenabled = true\nendpoint = \"http://127.0.0.1:7233\"\nallow_plaintext = true\n",
            "[temporal_worker]\ntask_queue = \"orders\"\n",
        ),
    );
    let options = LoadOptions {
        config: Some(shared),
        ..LoadOptions::default()
    };
    let service = load_from(&options, BUILD, env(&[])).unwrap();
    assert!(service.temporal().is_some());
    assert_eq!(service.http().grace_period, Duration::from_secs(45));
    assert_eq!(service.http().drain_timeout, Duration::from_secs(25));

    let worker =
        load_temporal_worker_with_tail_from(&options, BUILD, TEMPORAL_TAIL, env(&[])).unwrap();
    assert!(worker.temporal().is_some());
    assert_eq!(worker.http().grace_period, Duration::from_secs(90));
    assert_eq!(worker.http().drain_timeout, Duration::from_secs(60));
    assert_eq!(worker.temporal_worker().task_queue(), Some("orders"));

    for unknown in [
        ("APP__TEMPORAL_WORKER__BOGUS", "1"),
        ("APP__TEMPORAL_WORKER__TUNER__BOGUS", "1"),
        ("APP__TEMPORAL_WORKER__DEPLOYMENT__BOGUS", "1"),
        ("APP__TEMPORAL_WORKER__QUEUES__ORDERS__BOGUS", "1"),
        ("APP__TEMPORAL_BOGUS__X", "1"),
    ] {
        let err =
            load_temporal_worker_from(&LoadOptions::default(), BUILD, env(&[unknown])).unwrap_err();
        assert_matches!(err.0, ErrorKind::Deserialize(_), "{unknown:?}: {err}");
    }
    load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__TEMPORAL_WORKER__TASK_QUEUE", "orders")]),
    )
    .unwrap();
}

#[test]
fn nested_shutdown_budget_boundaries_are_enforced() {
    let exact = load_with_tail(&[
        ("APP__HTTP__GRACE_PERIOD", "76500ms"),
        ("APP__HTTP__DRAIN_TIMEOUT", "60s"),
        ("APP__TEMPORAL_WORKER__GRACEFUL_SHUTDOWN_PERIOD", "45s"),
        ("APP__TEMPORAL_WORKER__ACTIVITY_CANCEL_CLEANUP", "10s"),
        ("APP__TEMPORAL_WORKER__FINALIZE_BUDGET", "5s"),
    ])
    .unwrap();
    assert_eq!(exact.http().grace_period, Duration::from_millis(76_500));

    let grace_underflow = load_with_tail(&[
        ("APP__HTTP__GRACE_PERIOD", "76499ms"),
        ("APP__HTTP__DRAIN_TIMEOUT", "60s"),
    ])
    .unwrap_err();
    assert!(
        grace_underflow
            .validation_errors()
            .is_some_and(|e| e.names("http.grace_period")),
        "{grace_underflow}"
    );

    let drain_overflow_by_1ms =
        load_with_tail(&[("APP__TEMPORAL_WORKER__FINALIZE_BUDGET", "5001ms")]).unwrap_err();
    assert!(
        drain_overflow_by_1ms
            .validation_errors()
            .is_some_and(|e| e.names("temporal_worker.graceful_shutdown_period")),
        "{drain_overflow_by_1ms}"
    );

    let shrunk_drain = load_with_tail(&[("APP__HTTP__DRAIN_TIMEOUT", "59s")]).unwrap_err();
    assert!(
        shrunk_drain
            .validation_errors()
            .is_some_and(|e| e.names("temporal_worker.graceful_shutdown_period")),
        "{shrunk_drain}"
    );

    let sub_second_valid = load_with_tail(&[
        ("APP__HTTP__GRACE_PERIOD", "18s"),
        ("APP__HTTP__DRAIN_TIMEOUT", "1s"),
        ("APP__TEMPORAL_WORKER__GRACEFUL_SHUTDOWN_PERIOD", "200ms"),
        ("APP__TEMPORAL_WORKER__ACTIVITY_CANCEL_CLEANUP", "200ms"),
        ("APP__TEMPORAL_WORKER__FINALIZE_BUDGET", "200ms"),
    ])
    .unwrap();
    assert_eq!(
        sub_second_valid
            .temporal_worker()
            .graceful_shutdown_period(),
        Duration::from_millis(200)
    );

    for (var, key) in [
        (
            "APP__TEMPORAL_WORKER__GRACEFUL_SHUTDOWN_PERIOD",
            "temporal_worker.graceful_shutdown_period",
        ),
        (
            "APP__TEMPORAL_WORKER__ACTIVITY_CANCEL_CLEANUP",
            "temporal_worker.activity_cancel_cleanup",
        ),
        (
            "APP__TEMPORAL_WORKER__FINALIZE_BUDGET",
            "temporal_worker.finalize_budget",
        ),
    ] {
        let zero = load_with_tail(&[(var, "0s")]).unwrap_err();
        assert!(
            zero.validation_errors().is_some_and(|e| e.names(key)),
            "{var}: {zero}"
        );
    }

    let out_of_range_drain = load_with_tail(&[("APP__HTTP__DRAIN_TIMEOUT", "0s")]).unwrap_err();
    let drain_errors = out_of_range_drain.validation_errors().unwrap();
    assert!(drain_errors.names("http.drain_timeout"), "{drain_errors}");
    assert!(
        !drain_errors.names("temporal_worker.graceful_shutdown_period"),
        "{drain_errors}"
    );
}

#[test]
fn task_queue_and_queue_keys_enforce_addressability_characters_and_no_collision() {
    let ok = load_temporal_worker_env(&[
        ("APP__TEMPORAL_WORKER__TASK_QUEUE", "orders.default"),
        (
            "APP__TEMPORAL_WORKER__QUEUES__HIGH-PRIO__MAX_CACHED_WORKFLOWS",
            "200",
        ),
        (
            "APP__TEMPORAL_WORKER__QUEUES__HIGH-PRIO__WORKFLOW_POLLERS",
            "3",
        ),
        (
            "APP__TEMPORAL_WORKER__QUEUES__HIGH-PRIO__ACTIVITY_POLLERS",
            "4",
        ),
        (
            "APP__TEMPORAL_WORKER__QUEUES__HIGH-PRIO__MAX_WORKER_ACTIVITIES_PER_SECOND",
            "12.5",
        ),
    ]);
    assert_eq!(ok.temporal_worker().task_queue(), Some("orders.default"));
    let high = &ok.temporal_worker().queues()["high-prio"];
    assert_eq!(high.max_cached_workflows, Some(200));
    assert_eq!(high.workflow_pollers, NonZeroU32::new(3));
    assert_eq!(high.activity_pollers, NonZeroU32::new(4));
    assert_eq!(high.max_worker_activities_per_second, Some(12.5));

    let collision = load_with_tail(&[
        ("APP__TEMPORAL_WORKER__TASK_QUEUE", "orders"),
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__MAX_CACHED_WORKFLOWS",
            "50",
        ),
    ])
    .unwrap_err();
    assert!(
        collision
            .validation_errors()
            .is_some_and(|e| e.names("temporal_worker.queues.orders")),
        "{collision}"
    );

    let dir = tempfile::tempdir().unwrap();
    let bad_toml_key = write(
        &dir,
        "bad-queue.toml",
        "[temporal_worker.queues.\"Orders.V1\"]\nmax_cached_workflows = 10\n",
    );
    let err = load_temporal_worker_from(
        &LoadOptions {
            config: Some(bad_toml_key),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap_err();
    assert_matches!(
        &err.0,
        ErrorKind::Input(InputError::UnaddressableKey { key, .. })
            if key == "temporal_worker.queues.Orders.V1",
        "{err}"
    );

    let too_long = "a".repeat(256);
    for bad_key in [
        "",
        "   ",
        "Uppercase",
        "bad.dot",
        "bad__sep",
        "trailing_",
        "ctrl\nchar",
        too_long.as_str(),
    ] {
        let mut input = TemporalWorkerRuntimeInput::default();
        input
            .queues
            .insert(bad_key.to_owned(), TemporalQueueInput::default());
        let errors =
            admit_with(|v| input.admit(Some(Duration::from_secs(60)), "svc", "1.0.0", "abc", v))
                .unwrap_err();
        assert!(
            errors.names("temporal_worker.queues"),
            "{bad_key:?}: {errors}"
        );
    }

    for bad_task_queue in ["", "   ", "orders\n", "café", too_long.as_str()] {
        let err =
            load_with_tail(&[("APP__TEMPORAL_WORKER__TASK_QUEUE", bad_task_queue)]).unwrap_err();
        assert!(
            err.validation_errors()
                .is_some_and(|e| e.names("temporal_worker.task_queue")),
            "{bad_task_queue:?}: {err}"
        );
    }
}

#[test]
fn top_level_sticky_cache_requires_at_least_two_workflow_pollers_or_slots() {
    let bad_top_pollers = load_with_tail(&[
        ("APP__TEMPORAL_WORKER__MAX_CACHED_WORKFLOWS", "100"),
        ("APP__TEMPORAL_WORKER__WORKFLOW_POLLERS", "1"),
    ])
    .unwrap_err();
    assert!(
        bad_top_pollers
            .validation_errors()
            .is_some_and(|e| e.names("temporal_worker.workflow_pollers")),
        "{bad_top_pollers}"
    );

    let bad_top_fixed = load_with_tail(&[
        ("APP__TEMPORAL_WORKER__TUNER__MODE", "fixed_size"),
        (
            "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_WORKFLOW_TASKS",
            "1",
        ),
        (
            "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_ACTIVITIES",
            "4",
        ),
        (
            "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_LOCAL_ACTIVITIES",
            "4",
        ),
    ])
    .unwrap_err();
    assert!(
        bad_top_fixed
            .validation_errors()
            .is_some_and(|e| e.names("temporal_worker.tuner.max_concurrent_workflow_tasks")),
        "{bad_top_fixed}"
    );

    let uncached_single_ok = load_with_tail(&[
        ("APP__TEMPORAL_WORKER__MAX_CACHED_WORKFLOWS", "0"),
        ("APP__TEMPORAL_WORKER__WORKFLOW_POLLERS", "1"),
        ("APP__TEMPORAL_WORKER__TUNER__MODE", "fixed_size"),
        (
            "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_WORKFLOW_TASKS",
            "1",
        ),
        (
            "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_ACTIVITIES",
            "1",
        ),
        (
            "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_LOCAL_ACTIVITIES",
            "1",
        ),
    ])
    .unwrap();
    assert_eq!(
        uncached_single_ok.temporal_worker().max_cached_workflows(),
        0
    );
    assert_eq!(
        uncached_single_ok.temporal_worker().tuner(),
        &TemporalTunerConfig::FixedSize {
            max_concurrent_workflow_tasks: 1,
            max_concurrent_activities: 1,
            max_concurrent_local_activities: 1,
        }
    );

    let over_cache_cap =
        load_with_tail(&[("APP__TEMPORAL_WORKER__MAX_CACHED_WORKFLOWS", "100001")]).unwrap_err();
    assert!(
        over_cache_cap
            .validation_errors()
            .is_some_and(|e| e.names("temporal_worker.max_cached_workflows")),
        "{over_cache_cap}"
    );
}

#[test]
fn per_queue_sticky_cache_checks_effective_pollers_and_slots() {
    let queue_low_pollers = load_with_tail(&[(
        "APP__TEMPORAL_WORKER__QUEUES__ORDERS__WORKFLOW_POLLERS",
        "1",
    )])
    .unwrap_err();
    assert!(
        queue_low_pollers
            .validation_errors()
            .is_some_and(|e| e.names("temporal_worker.queues.orders.workflow_pollers")),
        "{queue_low_pollers}"
    );

    let queue_uncached_low_pollers_ok = load_with_tail(&[
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__MAX_CACHED_WORKFLOWS",
            "0",
        ),
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__WORKFLOW_POLLERS",
            "1",
        ),
    ])
    .unwrap();
    assert_eq!(
        queue_uncached_low_pollers_ok.temporal_worker().queues()["orders"].max_cached_workflows,
        Some(0)
    );

    let queue_enables_cache_with_inherited_single_poller = load_with_tail(&[
        ("APP__TEMPORAL_WORKER__MAX_CACHED_WORKFLOWS", "0"),
        ("APP__TEMPORAL_WORKER__WORKFLOW_POLLERS", "1"),
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__MAX_CACHED_WORKFLOWS",
            "10",
        ),
    ])
    .unwrap_err();
    assert!(
        queue_enables_cache_with_inherited_single_poller
            .validation_errors()
            .is_some_and(|e| e.names("temporal_worker.queues.orders.max_cached_workflows")),
        "{queue_enables_cache_with_inherited_single_poller}"
    );

    let queue_fixed_single_wf_with_cached = load_with_tail(&[
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__TUNER__MODE",
            "fixed_size",
        ),
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__TUNER__MAX_CONCURRENT_WORKFLOW_TASKS",
            "1",
        ),
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__TUNER__MAX_CONCURRENT_ACTIVITIES",
            "2",
        ),
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__TUNER__MAX_CONCURRENT_LOCAL_ACTIVITIES",
            "2",
        ),
    ])
    .unwrap_err();
    assert!(
        queue_fixed_single_wf_with_cached
            .validation_errors()
            .is_some_and(|e| {
                e.names("temporal_worker.queues.orders.tuner.max_concurrent_workflow_tasks")
            }),
        "{queue_fixed_single_wf_with_cached}"
    );

    let queue_enables_cache_with_inherited_single_wf_slot = load_with_tail(&[
        ("APP__TEMPORAL_WORKER__MAX_CACHED_WORKFLOWS", "0"),
        ("APP__TEMPORAL_WORKER__TUNER__MODE", "fixed_size"),
        (
            "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_WORKFLOW_TASKS",
            "1",
        ),
        (
            "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_ACTIVITIES",
            "2",
        ),
        (
            "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_LOCAL_ACTIVITIES",
            "2",
        ),
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__MAX_CACHED_WORKFLOWS",
            "10",
        ),
    ])
    .unwrap_err();
    assert!(
        queue_enables_cache_with_inherited_single_wf_slot
            .validation_errors()
            .is_some_and(|e| e.names("temporal_worker.queues.orders.max_cached_workflows")),
        "{queue_enables_cache_with_inherited_single_wf_slot}"
    );
}

#[test]
fn per_queue_cache_cap_and_activity_rate_limits_are_validated() {
    for (var, val, key) in [
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__MAX_CACHED_WORKFLOWS",
            "100001",
            "temporal_worker.queues.orders.max_cached_workflows",
        ),
        (
            "APP__TEMPORAL_WORKER__MAX_WORKER_ACTIVITIES_PER_SECOND",
            "0",
            "temporal_worker.max_worker_activities_per_second",
        ),
        (
            "APP__TEMPORAL_WORKER__MAX_WORKER_ACTIVITIES_PER_SECOND",
            "-2.5",
            "temporal_worker.max_worker_activities_per_second",
        ),
        (
            "APP__TEMPORAL_WORKER__MAX_WORKER_ACTIVITIES_PER_SECOND",
            "NaN",
            "temporal_worker.max_worker_activities_per_second",
        ),
        (
            "APP__TEMPORAL_WORKER__MAX_WORKER_ACTIVITIES_PER_SECOND",
            "inf",
            "temporal_worker.max_worker_activities_per_second",
        ),
        (
            "APP__TEMPORAL_WORKER__MAX_WORKER_ACTIVITIES_PER_SECOND",
            "-inf",
            "temporal_worker.max_worker_activities_per_second",
        ),
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__MAX_WORKER_ACTIVITIES_PER_SECOND",
            "0",
            "temporal_worker.queues.orders.max_worker_activities_per_second",
        ),
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__MAX_WORKER_ACTIVITIES_PER_SECOND",
            "-2.5",
            "temporal_worker.queues.orders.max_worker_activities_per_second",
        ),
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__MAX_WORKER_ACTIVITIES_PER_SECOND",
            "NaN",
            "temporal_worker.queues.orders.max_worker_activities_per_second",
        ),
        (
            "APP__TEMPORAL_WORKER__QUEUES__ORDERS__MAX_WORKER_ACTIVITIES_PER_SECOND",
            "inf",
            "temporal_worker.queues.orders.max_worker_activities_per_second",
        ),
    ] {
        let err = load_with_tail(&[(var, val)]).unwrap_err();
        assert!(
            err.validation_errors().is_some_and(|e| e.names(key)),
            "{var}={val}: {err}"
        );
    }
}

#[test]
fn tuner_bounds_and_mode_exclusions_are_validated() {
    let ratio_bounds_ok = load_with_tail(&[
        ("APP__TEMPORAL_WORKER__TUNER__TARGET_MEMORY", "0.05"),
        ("APP__TEMPORAL_WORKER__TUNER__TARGET_CPU", "0.95"),
    ])
    .unwrap();
    assert_eq!(
        ratio_bounds_ok.temporal_worker().tuner(),
        &TemporalTunerConfig::ResourceBased {
            target_memory: 0.05,
            target_cpu: 0.95,
        }
    );

    for (var, val, key) in [
        (
            "APP__TEMPORAL_WORKER__TUNER__TARGET_MEMORY",
            "0.04",
            "temporal_worker.tuner.target_memory",
        ),
        (
            "APP__TEMPORAL_WORKER__TUNER__TARGET_CPU",
            "0.96",
            "temporal_worker.tuner.target_cpu",
        ),
        (
            "APP__TEMPORAL_WORKER__TUNER__TARGET_MEMORY",
            "NaN",
            "temporal_worker.tuner.target_memory",
        ),
    ] {
        let err = load_with_tail(&[(var, val)]).unwrap_err();
        assert!(
            err.validation_errors().is_some_and(|e| e.names(key)),
            "{var}={val}: {err}"
        );
    }

    let mixed_resource_and_fixed = load_with_tail(&[(
        "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_ACTIVITIES",
        "10",
    )])
    .unwrap_err();
    assert!(
        mixed_resource_and_fixed
            .validation_errors()
            .is_some_and(|e| e.names("temporal_worker.tuner.max_concurrent_activities")),
        "{mixed_resource_and_fixed}"
    );

    let fixed_missing_and_mixed = load_with_tail(&[
        ("APP__TEMPORAL_WORKER__TUNER__MODE", "fixed_size"),
        ("APP__TEMPORAL_WORKER__TUNER__TARGET_MEMORY", "0.5"),
        (
            "APP__TEMPORAL_WORKER__TUNER__MAX_CONCURRENT_WORKFLOW_TASKS",
            "0",
        ),
    ])
    .unwrap_err();
    let errors = fixed_missing_and_mixed.validation_errors().unwrap();
    assert!(
        errors.names("temporal_worker.tuner.target_memory"),
        "{errors}"
    );
    assert!(
        errors.names("temporal_worker.tuner.max_concurrent_workflow_tasks"),
        "{errors}"
    );
    assert!(
        errors.names("temporal_worker.tuner.max_concurrent_activities"),
        "{errors}"
    );
    assert!(
        errors.names("temporal_worker.tuner.max_concurrent_local_activities"),
        "{errors}"
    );
}

#[test]
fn deployment_defaults_and_worker_versioning_requirements_are_enforced() {
    let inherited = load_with_tail(&[
        ("APP__OBSERVABILITY__OTEL__SERVICE_NAME", "orders"),
        ("APP__APP__VERSION", "2.4.0"),
        ("APP__APP__COMMIT", "deadbeef"),
        (
            "APP__TEMPORAL_WORKER__DEPLOYMENT__USE_WORKER_VERSIONING",
            "true",
        ),
        (
            "APP__TEMPORAL_WORKER__DEPLOYMENT__DEFAULT_VERSIONING_BEHAVIOR",
            "pinned",
        ),
    ])
    .unwrap();
    assert_eq!(
        inherited.temporal_worker().deployment(),
        &TemporalDeploymentConfig {
            deployment_name: "orders".to_owned(),
            build_id: "2.4.0-deadbeef".to_owned(),
            use_worker_versioning: true,
            default_versioning_behavior: TemporalVersioningBehavior::Pinned,
        }
    );

    let unspecified_when_versioned = load_with_tail(&[(
        "APP__TEMPORAL_WORKER__DEPLOYMENT__USE_WORKER_VERSIONING",
        "true",
    )])
    .unwrap_err();
    assert!(
        unspecified_when_versioned
            .validation_errors()
            .is_some_and(|e| e.names("temporal_worker.deployment.default_versioning_behavior")),
        "{unspecified_when_versioned}"
    );

    let blank_overrides = load_with_tail(&[
        ("APP__TEMPORAL_WORKER__DEPLOYMENT__DEPLOYMENT_NAME", "   "),
        ("APP__TEMPORAL_WORKER__DEPLOYMENT__BUILD_ID", ""),
        (
            "APP__TEMPORAL_WORKER__DEPLOYMENT__USE_WORKER_VERSIONING",
            "true",
        ),
        (
            "APP__TEMPORAL_WORKER__DEPLOYMENT__DEFAULT_VERSIONING_BEHAVIOR",
            "auto_upgrade",
        ),
    ])
    .unwrap_err();
    let errors = blank_overrides.validation_errors().unwrap();
    assert!(
        errors.names("temporal_worker.deployment.deployment_name"),
        "{errors}"
    );
    assert!(
        errors.names("temporal_worker.deployment.build_id"),
        "{errors}"
    );

    let control_char_overrides = load_with_tail(&[
        (
            "APP__TEMPORAL_WORKER__DEPLOYMENT__DEPLOYMENT_NAME",
            "orders\nprod",
        ),
        ("APP__TEMPORAL_WORKER__DEPLOYMENT__BUILD_ID", "1.2.3\tdev"),
    ])
    .unwrap_err();
    let control_errors = control_char_overrides.validation_errors().unwrap();
    assert!(
        control_errors.names("temporal_worker.deployment.deployment_name"),
        "{control_errors}"
    );
    assert!(
        control_errors.names("temporal_worker.deployment.build_id"),
        "{control_errors}"
    );
}

#[rstest::rstest]
#[case::unversioned_explicit_unspecified(
    Some("false"),
    "unspecified",
    Some(TemporalVersioningBehavior::Unspecified)
)]
#[case::unversioned_default_pinned(None, "pinned", None)]
#[case::unversioned_explicit_pinned(Some("false"), "pinned", None)]
#[case::unversioned_default_auto_upgrade(None, "auto_upgrade", None)]
#[case::unversioned_explicit_auto_upgrade(Some("false"), "auto_upgrade", None)]
#[case::versioned_unspecified(Some("true"), "unspecified", None)]
#[case::versioned_pinned(Some("true"), "pinned", Some(TemporalVersioningBehavior::Pinned))]
#[case::versioned_auto_upgrade(
    Some("true"),
    "auto_upgrade",
    Some(TemporalVersioningBehavior::AutoUpgrade)
)]
fn deployment_versioning_behavior_matches_use_worker_versioning(
    #[case] use_worker_versioning: Option<&str>,
    #[case] behavior: &str,
    #[case] expected: Option<TemporalVersioningBehavior>,
) {
    let mut pairs = vec![(
        "APP__TEMPORAL_WORKER__DEPLOYMENT__DEFAULT_VERSIONING_BEHAVIOR",
        behavior,
    )];
    if let Some(enabled) = use_worker_versioning {
        pairs.push((
            "APP__TEMPORAL_WORKER__DEPLOYMENT__USE_WORKER_VERSIONING",
            enabled,
        ));
    }
    let result = load_with_tail(&pairs);
    if let Some(expected_behavior) = expected {
        let cfg = result.unwrap();
        let deployment = cfg.temporal_worker().deployment();
        assert_eq!(
            deployment.use_worker_versioning,
            use_worker_versioning == Some("true")
        );
        assert_eq!(deployment.default_versioning_behavior, expected_behavior);
    } else {
        let err = result.unwrap_err();
        assert!(
            err.validation_errors().is_some_and(|e| {
                e.names("temporal_worker.deployment.default_versioning_behavior")
            }),
            "{pairs:?}: {err}"
        );
    }
}

#[cfg(feature = "test-support")]
#[test]
fn test_support_temporal_worker_fixture_loads_with_tail() {
    let cfg = crate::test_support::temporal_worker(&[], BUILD, TEMPORAL_TAIL).unwrap();
    assert_eq!(cfg.http().grace_period, Duration::from_secs(90));
    assert_eq!(cfg.http().drain_timeout, Duration::from_secs(60));
    assert!(cfg.temporal().is_none());
}
