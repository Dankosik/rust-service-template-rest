use metrics::{Key, Level, Metadata, Recorder as _};
use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};

use super::*;
use crate::test_metrics::{self, Series};

const TEST_CLEANUP: CleanupName = CleanupName::new("jobs");
const RESTORE_METADATA: Metadata<'static> = Metadata::new(module_path!(), Level::INFO, None);

struct TestMetrics {
    recorder: DebuggingRecorder,
    snapshotter: Snapshotter,
}

impl TestMetrics {
    fn new() -> Self {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        Self {
            recorder,
            snapshotter,
        }
    }

    fn snapshot(&self) -> Series {
        let series = self.snapshotter.snapshot().into_hashmap();
        for (composite, (_, _, value)) in &series {
            match value {
                DebugValue::Counter(count) if *count != 0 => {
                    self.recorder
                        .register_counter(composite.key(), &RESTORE_METADATA)
                        .increment(*count);
                }
                DebugValue::Gauge(gauge) => {
                    self.recorder
                        .register_gauge(composite.key(), &RESTORE_METADATA)
                        .set(gauge.into_inner());
                }
                _ => {}
            }
        }
        series
    }

    fn gauge(&self, name: &str) -> f64 {
        let target = Key::from_parts(
            format!("postgres_maintenance_{name}"),
            &[("population", "jobs")],
        );
        test_metrics::gauge(&self.snapshot(), &target).unwrap_or_else(|| panic!("missing {target}"))
    }

    fn counter(&self, target: &Key) -> u64 {
        test_metrics::counter(&self.snapshot(), target)
            .unwrap_or_else(|| panic!("missing {target}"))
    }

    fn observation_count(&self, outcome: &'static str) -> u64 {
        self.counter(&Key::from_parts(
            OBSERVATIONS_METRIC,
            &[("population", "jobs"), ("outcome", outcome)],
        ))
    }
}

#[tokio::test(start_paused = true)]
async fn a_read_past_the_budget_fails_the_attempt_and_keeps_the_last_sample() {
    let metrics = TestMetrics::new();
    let _recording = metrics::set_default_local_recorder(&metrics.recorder);
    let mut observer = MaintenanceObserver::start(MaintenancePopulation::Cleanup(TEST_CLEANUP));
    let observed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the test clock is after the epoch")
        .as_secs_f64();

    observer
        .sample(async {
            Ok(PopulationSample {
                observed_at_unix_seconds: observed_at,
                oldest_at_unix_seconds: Some(observed_at - 60.0),
            })
        })
        .await;
    observer.sample(std::future::pending()).await;

    assert_eq!(metrics.observation_count("success"), 1);
    assert_eq!(metrics.observation_count("failed"), 1);
    assert_eq!(metrics.gauge("present"), 1.0);
    assert_eq!(metrics.gauge("last_attempt_success"), 0.0);
}

#[test]
fn population_failure_and_cancellation_retain_dated_success_until_recovery() {
    let metrics = TestMetrics::new();
    metrics::with_local_recorder(&metrics.recorder, || {
        let mut observer = MaintenanceObserver::start(MaintenancePopulation::Cleanup(TEST_CLEANUP));
        assert!(metrics.gauge("present").is_nan());
        assert!(metrics.gauge("oldest_timestamp_seconds").is_nan());
        assert_eq!(metrics.gauge("last_success_timestamp_seconds"), 0.0);
        assert_eq!(
            metrics.counter(&Key::from_parts(
                CLEANUP_REMOVED_ROWS_METRIC,
                &[("cleanup", "jobs")]
            )),
            0
        );

        let database_time = observer.previous_clock.wall.unwrap();
        let oldest = database_time - 600.0;
        observer.attempt().record_sample(PopulationSample {
            observed_at_unix_seconds: database_time,
            oldest_at_unix_seconds: Some(oldest),
        });
        let receipt = metrics.gauge("last_success_timestamp_seconds");
        for cause in [
            FailureCause::Read(MaintenanceFailure::Acquire),
            FailureCause::TimedOut,
            FailureCause::Read(MaintenanceFailure::CommitUnknown),
        ] {
            observer.attempt().failed(cause);
            assert_eq!(metrics.gauge("present"), 1.0);
            assert_eq!(metrics.gauge("oldest_timestamp_seconds"), oldest);
            assert_eq!(
                metrics.gauge("database_observed_timestamp_seconds"),
                database_time
            );
            assert_eq!(metrics.gauge("last_success_timestamp_seconds"), receipt);
            assert_eq!(metrics.gauge("last_attempt_success"), 0.0);
        }
        drop(observer.attempt());
        assert_eq!(metrics.gauge("oldest_timestamp_seconds"), oldest);
        assert_eq!(metrics.gauge("last_success_timestamp_seconds"), receipt);
        for (outcome, value) in [("success", 1), ("failed", 3), ("cancelled", 1)] {
            assert_eq!(metrics.observation_count(outcome), value);
        }
        observer.attempt().record_sample(PopulationSample {
            observed_at_unix_seconds: database_time,
            oldest_at_unix_seconds: None,
        });
        assert_eq!(metrics.gauge("present"), 0.0);
        assert_eq!(metrics.gauge("oldest_timestamp_seconds"), 0.0);
        assert_eq!(metrics.gauge("last_attempt_success"), 1.0);
        assert_eq!(metrics.gauge("clock_valid"), 1.0);
        drop(observer);
        assert_eq!(metrics.gauge("observer_enabled"), 0.0);
    });
}

#[test]
fn clock_inconsistency_never_replaces_a_valid_population_with_empty() {
    for (request_wall, receipt_wall, database_time, oldest) in [
        (1001.0, 1002.0, 1008.0, None),
        (1001.0, 1002.0, 995.0, None),
        (1010.0, 1011.0, 1010.0, None),
        (990.0, 991.0, 990.0, None),
        (1001.0, 1012.0, 1001.0, None),
        (1001.0, 1002.0, 1001.0, Some(1003.0)),
        (1001.0, 1002.0, f64::NAN, None),
        (1001.0, 1002.0, 1001.0, Some(f64::INFINITY)),
    ] {
        let metrics = TestMetrics::new();
        metrics::with_local_recorder(&metrics.recorder, || {
            let mut observer =
                MaintenanceObserver::start(MaintenancePopulation::Cleanup(TEST_CLEANUP));
            let base = tokio::time::Instant::now();
            let original = MaintenanceClock {
                wall: Some(1000.0),
                monotonic: base,
            };
            observer.previous_clock = original;
            let mut initial = observer.attempt();
            initial.requested = original;
            initial.record_sample_at(
                PopulationSample {
                    observed_at_unix_seconds: 1000.0,
                    oldest_at_unix_seconds: Some(400.0),
                },
                original,
            );
            let mut attempt = observer.attempt();
            attempt.requested = MaintenanceClock {
                wall: Some(request_wall),
                monotonic: base + Duration::from_secs(1),
            };
            attempt.record_sample_at(
                PopulationSample {
                    observed_at_unix_seconds: database_time,
                    oldest_at_unix_seconds: oldest,
                },
                MaintenanceClock {
                    wall: Some(receipt_wall),
                    monotonic: base + Duration::from_secs(2),
                },
            );
            assert_eq!(metrics.gauge("clock_valid"), 0.0);
            assert_eq!(metrics.gauge("present"), 1.0);
            assert_eq!(metrics.gauge("oldest_timestamp_seconds"), 400.0);
            assert_eq!(metrics.gauge("last_success_timestamp_seconds"), 1000.0);
            let mut resumed = observer.attempt();
            resumed.requested = MaintenanceClock {
                wall: Some(receipt_wall + 1.0),
                monotonic: base + Duration::from_secs(3),
            };
            resumed.record_sample_at(
                PopulationSample {
                    observed_at_unix_seconds: receipt_wall + 1.0,
                    oldest_at_unix_seconds: None,
                },
                MaintenanceClock {
                    wall: Some(receipt_wall + 2.0),
                    monotonic: base + Duration::from_secs(4),
                },
            );
            assert_eq!(metrics.gauge("clock_valid"), 1.0);
            assert_eq!(metrics.gauge("present"), 0.0);
        });
    }
}
