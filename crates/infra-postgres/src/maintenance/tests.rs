use metrics_util::debugging::DebuggingRecorder;

use super::*;

const TEST_CLEANUP: CleanupName = CleanupName::new("jobs");
const RUNS_METRIC: &str = "test_cleanup_runs_total";

#[tokio::test(start_paused = true)]
async fn the_first_scheduled_pass_starts_at_once_and_the_next_waits_the_interval() {
    let mut schedule = CleanupSchedule::new(TEST_CLEANUP);
    schedule.next().await;
    let started = tokio::time::Instant::now();
    schedule.next().await;
    assert_eq!(started.elapsed(), CLEANUP_INTERVAL);
}

#[tokio::test(start_paused = true)]
async fn an_owner_cancelled_before_start_runs_no_pass_and_publishes_no_observer() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _recording = metrics::set_default_local_recorder(&recorder);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut attempts = 0;

    run_scheduled_cleanup(
        TEST_CLEANUP,
        Some(RUNS_METRIC),
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
        snapshotter
            .snapshot()
            .into_vec()
            .iter()
            .all(|(key, ..)| key.key().name() != "postgres_maintenance_observer_enabled")
    );
}

#[tokio::test(start_paused = true)]
async fn scheduled_cleanup_with_borrowing_closures_is_send_for_spawned_owners() {
    struct Owner {
        label: String,
    }

    impl Owner {
        async fn pass(&self, cancel: &CancellationToken) -> Result<PassEnd, ()> {
            tokio::task::yield_now().await;
            assert_ne!(self.label, "");
            cancel.cancel();
            Ok(PassEnd::Cancelled)
        }

        async fn sample(&self) -> Result<PopulationSample, MaintenanceFailure> {
            tokio::task::yield_now().await;
            assert_ne!(self.label, "");
            Err(MaintenanceFailure::Statement)
        }
    }

    let owner = Owner {
        label: String::from("spawned"),
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    tokio::spawn(async move {
        run_scheduled_cleanup(
            TEST_CLEANUP,
            None,
            &cancel,
            || owner.pass(&cancel),
            || owner.sample(),
        )
        .await;
    })
    .await
    .unwrap();
}
