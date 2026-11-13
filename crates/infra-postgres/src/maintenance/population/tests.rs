use metrics_exporter_prometheus::PrometheusBuilder;

use super::*;

#[tokio::test(start_paused = true)]
async fn a_read_past_the_budget_fails_the_attempt_and_keeps_the_last_sample() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _recording = metrics::set_default_local_recorder(&recorder);
    let mut observer = MaintenanceObserver::start(MaintenancePopulation::Cleanup(Cleanup::Jobs));
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

    let scrape = recorder.handle().render();
    assert!(scrape.contains(r#"outcome="success"} 1"#), "{scrape}");
    assert!(scrape.contains(r#"outcome="failed"} 1"#), "{scrape}");
    assert!(scrape.contains(r#"postgres_maintenance_present{population="jobs"} 1"#));
    assert!(scrape.contains(r#"postgres_maintenance_last_attempt_success{population="jobs"} 0"#));
}

fn population_gauge(recorder: &metrics_exporter_prometheus::PrometheusRecorder, name: &str) -> f64 {
    let name = format!("postgres_maintenance_{name}");
    recorder
        .handle()
        .render()
        .lines()
        .find(|line| line.starts_with(&format!("{name}{{population=\"jobs\"}} ")))
        .and_then(|line| line.rsplit_once(' '))
        .unwrap_or_else(|| panic!("missing {name}"))
        .1
        .parse()
        .unwrap()
}

#[test]
fn population_failure_and_cancellation_retain_dated_success_until_recovery() {
    let recorder = PrometheusBuilder::new().build_recorder();
    metrics::with_local_recorder(&recorder, || {
        let mut observer =
            MaintenanceObserver::start(MaintenancePopulation::Cleanup(Cleanup::Jobs));
        assert!(population_gauge(&recorder, "present").is_nan());
        assert!(population_gauge(&recorder, "oldest_timestamp_seconds").is_nan());
        assert_eq!(
            population_gauge(&recorder, "last_success_timestamp_seconds"),
            0.0
        );
        assert!(
            recorder
                .handle()
                .render()
                .contains("postgres_cleanup_removed_rows_total{cleanup=\"jobs\"} 0")
        );

        let database_time = observer.previous_clock.wall.unwrap();
        let oldest = database_time - 600.0;
        observer.attempt().record_sample(PopulationSample {
            observed_at_unix_seconds: database_time,
            oldest_at_unix_seconds: Some(oldest),
        });
        let receipt = population_gauge(&recorder, "last_success_timestamp_seconds");
        for cause in [
            MaintenanceFailure::Acquire,
            MaintenanceFailure::TimedOut,
            MaintenanceFailure::CommitUnknown,
        ] {
            observer.attempt().failed(cause);
            assert_eq!(population_gauge(&recorder, "present"), 1.0);
            assert_eq!(
                population_gauge(&recorder, "oldest_timestamp_seconds"),
                oldest
            );
            assert_eq!(
                population_gauge(&recorder, "database_observed_timestamp_seconds"),
                database_time
            );
            assert_eq!(
                population_gauge(&recorder, "last_success_timestamp_seconds"),
                receipt
            );
            assert_eq!(population_gauge(&recorder, "last_attempt_success"), 0.0);
        }
        drop(observer.attempt());
        assert_eq!(
            population_gauge(&recorder, "oldest_timestamp_seconds"),
            oldest
        );
        assert_eq!(
            population_gauge(&recorder, "last_success_timestamp_seconds"),
            receipt
        );
        let scrape = recorder.handle().render();
        for (outcome, value) in [("success", 1), ("failed", 3), ("cancelled", 1)] {
            assert!(
                scrape.lines().any(|line| line
                    .starts_with("postgres_maintenance_observations_total{")
                    && line.contains(&format!("outcome=\"{outcome}\""))
                    && line.ends_with(&format!(" {value}"))),
                "{scrape}"
            );
        }
        observer.attempt().record_sample(PopulationSample {
            observed_at_unix_seconds: database_time,
            oldest_at_unix_seconds: None,
        });
        assert_eq!(population_gauge(&recorder, "present"), 0.0);
        assert_eq!(population_gauge(&recorder, "oldest_timestamp_seconds"), 0.0);
        assert_eq!(population_gauge(&recorder, "last_attempt_success"), 1.0);
        assert_eq!(population_gauge(&recorder, "clock_valid"), 1.0);
        drop(observer);
        assert_eq!(population_gauge(&recorder, "observer_enabled"), 0.0);
    });
}

#[test]
fn clock_inconsistency_never_replaces_a_valid_population_with_empty() {
    // A stable clock starts at wall=1000. Request mono=1s, receipt mono=2s.
    // Each row violates a distinct clock/sample condition, independently of SQL.
    for (request_wall, receipt_wall, database_time, oldest) in [
        (1001.0, 1002.0, 1008.0, None), // database ahead of receipt bracket
        (1001.0, 1002.0, 995.0, None),  // database behind request bracket
        (1010.0, 1011.0, 1010.0, None), // local jump between attempts
        (990.0, 991.0, 990.0, None),    // local backward movement
        (1001.0, 1012.0, 1001.0, None), // local jump within attempt
        (1001.0, 1002.0, 1001.0, Some(1003.0)), // future oldest value
        (1001.0, 1002.0, f64::NAN, None),
        (1001.0, 1002.0, 1001.0, Some(f64::INFINITY)),
    ] {
        let recorder = PrometheusBuilder::new().build_recorder();
        metrics::with_local_recorder(&recorder, || {
            let mut observer =
                MaintenanceObserver::start(MaintenancePopulation::Cleanup(Cleanup::Jobs));
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
            assert_eq!(population_gauge(&recorder, "clock_valid"), 0.0);
            assert_eq!(population_gauge(&recorder, "present"), 1.0);
            assert_eq!(
                population_gauge(&recorder, "oldest_timestamp_seconds"),
                400.0
            );
            assert_eq!(
                population_gauge(&recorder, "last_success_timestamp_seconds"),
                1000.0
            );
            // A stable new bracket restores observation after the rejected jump.
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
            assert_eq!(population_gauge(&recorder, "clock_valid"), 1.0);
            assert_eq!(population_gauge(&recorder, "present"), 0.0);
        });
    }
}
