use metrics_exporter_prometheus::PrometheusBuilder;

use super::*;

const RUNS_METRIC: &str = "test_cleanup_runs_total";

#[tokio::test(start_paused = true)]
async fn the_first_scheduled_pass_starts_at_once_and_the_next_waits_the_interval() {
    let mut schedule = CleanupSchedule::new(Cleanup::Jobs);
    schedule.next().await;
    let started = tokio::time::Instant::now();
    schedule.next().await;
    assert_eq!(started.elapsed(), CLEANUP_INTERVAL);
}

#[tokio::test(start_paused = true)]
async fn an_owner_cancelled_before_start_runs_no_pass_and_publishes_no_observer() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _recording = metrics::set_default_local_recorder(&recorder);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut attempts = 0;

    run_scheduled_cleanup(
        Cleanup::Jobs,
        RUNS_METRIC,
        &cancel,
        || {
            attempts += 1;
            std::future::ready(Ok::<_, ()>(PassEnd::Completed))
        },
        || std::future::ready(Err(MaintenanceFailure::Statement)),
    )
    .await;

    assert_eq!(attempts, 0);
    assert!(
        !recorder
            .handle()
            .render()
            .contains("postgres_maintenance_observer_enabled")
    );
}
