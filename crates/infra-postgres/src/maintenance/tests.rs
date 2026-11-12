use std::time::Duration;

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};

use super::*;
use crate::observe::tests::Events;

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
                observed_at,
                oldest: Some(observed_at - 60.0),
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

#[tokio::test(start_paused = true)]
async fn the_first_scheduled_pass_starts_at_once_and_the_next_waits_the_interval() {
    let mut schedule = CleanupSchedule::new(Cleanup::Jobs);
    schedule.next().await;
    let started = tokio::time::Instant::now();
    schedule.next().await;
    assert_eq!(started.elapsed(), CLEANUP_INTERVAL);
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
        observer.attempt().succeeded(PopulationSample {
            observed_at: database_time,
            oldest: Some(oldest),
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
        observer.attempt().succeeded(PopulationSample {
            observed_at: database_time,
            oldest: None,
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
            initial.succeeded_at(
                PopulationSample {
                    observed_at: 1000.0,
                    oldest: Some(400.0),
                },
                original,
            );
            let mut attempt = observer.attempt();
            attempt.requested = MaintenanceClock {
                wall: Some(request_wall),
                monotonic: base + Duration::from_secs(1),
            };
            attempt.succeeded_at(
                PopulationSample {
                    observed_at: database_time,
                    oldest,
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
            resumed.succeeded_at(
                PopulationSample {
                    observed_at: receipt_wall + 1.0,
                    oldest: None,
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

#[test]
fn cleanup_buckets_separate_a_seventeen_second_pass_from_upper_tail_overflow() {
    let recorder = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(CLEANUP_DURATION_METRIC.to_owned()),
            CLEANUP_DURATION_BUCKETS,
        )
        .unwrap()
        .build_recorder();
    metrics::with_local_recorder(&recorder, || {
        let mut failed = CleanupPass::start(Cleanup::Jobs);
        failed.started -= Duration::from_secs(17);
        failed.committed(500);
        failed.failed();
        drop(failed);
        let mut long = CleanupPass::start(Cleanup::Jobs);
        long.started -= Duration::from_secs(3601);
        long.completed();
        drop(long);
    });
    let scrape = recorder.handle().render();
    for (outcome, bound, count) in [
        ("failed", "15", 0),
        ("failed", "30", 1),
        ("completed", "3600", 0),
        ("completed", "+Inf", 1),
    ] {
        assert!(
            scrape.lines().any(|line| line
                .starts_with("postgres_cleanup_pass_duration_seconds_bucket{")
                && line.contains(&format!("outcome=\"{outcome}\""))
                && line.contains(&format!("le=\"{bound}\""))
                && line.ends_with(&format!(" {count}"))),
            "{scrape}"
        );
    }
    assert!(scrape.contains("postgres_cleanup_removed_rows_total{cleanup=\"jobs\"} 500"));
    assert!(scrape.contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 0"));
}

#[test]
fn cleanup_reports_live_progress_and_one_payload_free_terminal_event_per_pass() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let events = Events::new(tracing::Level::INFO);
    let started = tokio::time::Instant::now();
    metrics::with_local_recorder(&recorder, || {
        tracing::subscriber::with_default(events.clone(), || {
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            let mut completed = CleanupPass::start(Cleanup::Jobs);
            let mut failed = CleanupPass::start(Cleanup::Jobs);
            let mut cancelled = CleanupPass::start(Cleanup::Jobs);
            completed.committed(500);
            completed.committed(0);
            failed.committed(500);
            cancelled.committed(500);
            let scrape = recorder.handle().render();
            for line in [
                "postgres_cleanup_active_passes{cleanup=\"jobs\"} 3",
                "postgres_cleanup_committed_batches_total{cleanup=\"jobs\"} 4",
                "postgres_cleanup_removed_rows_total{cleanup=\"jobs\"} 1500",
            ] {
                assert!(scrape.contains(line), "{line} missing from {scrape}");
            }
            assert!(!scrape.contains("postgres_cleanup_passes_total{"));
            assert!(events.records.lock().unwrap().is_empty());
            completed.completed();
            drop(completed);
            assert!(
                recorder
                    .handle()
                    .render()
                    .contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 2")
            );
            failed.failed();
            drop(failed);
            assert!(
                recorder
                    .handle()
                    .render()
                    .contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 1")
            );
            drop(cancelled);
        });
    });
    let scrape = recorder.handle().render();
    assert!(scrape.contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 0"));
    for outcome in ["completed", "failed", "cancelled"] {
        for metric in [
            "postgres_cleanup_passes_total",
            "postgres_cleanup_pass_duration_seconds_count",
        ] {
            let line = format!("{metric}{{cleanup=\"jobs\",outcome=\"{outcome}\"}} 1");
            assert!(scrape.contains(&line), "{line} missing from {scrape}");
        }
    }
    let events = events.records.lock().unwrap();
    assert_eq!(events.len(), 3);
    for (event, (outcome, batches)) in
        events
            .iter()
            .zip([("completed", "2"), ("failed", "1"), ("cancelled", "1")])
    {
        assert_eq!(event["message"], "postgres_cleanup_pass_finished");
        assert_eq!(event["cleanup"], "\"jobs\"");
        assert_eq!(event["outcome"], format!("\"{outcome}\""));
        assert_eq!(event["committed_batches"], batches);
        assert_eq!(event["removed_rows"], "500");
        let elapsed = event["elapsed_seconds"].parse::<f64>().unwrap();
        assert!(elapsed >= 0.0 && elapsed <= started.elapsed().as_secs_f64());
        assert_eq!(event.len(), 6, "no identifiers, SQL, errors or payloads");
    }
}

#[test]
fn cleanup_releases_the_original_active_gauge_after_moving_threads() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let pass = metrics::with_local_recorder(&recorder, || CleanupPass::start(Cleanup::Jobs));
    std::thread::spawn(move || drop(pass)).join().unwrap();
    assert!(
        recorder
            .handle()
            .render()
            .contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 0")
    );
}

const RUNS_METRIC: &str = "test_cleanup_runs_total";

#[tokio::test(start_paused = true)]
async fn scheduled_cleanup_counts_a_timed_out_batch_and_runs_the_next_pass() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _recording = metrics::set_default_local_recorder(&recorder);
    let cancel = CancellationToken::new();
    let mut passes = 0;

    let cleanup = run_scheduled_cleanup(
        Cleanup::Jobs,
        RUNS_METRIC,
        &cancel,
        || {
            passes += 1;
            let pass_number = passes;
            let cancel = &cancel;
            async move {
                match pass_number {
                    1 => {
                        let mut pass = CleanupPass::start(Cleanup::Jobs);
                        pass.await_batch(std::future::pending()).await?;
                        Ok::<_, CleanupError>(PassEnd::Completed)
                    }
                    2 => {
                        let mut pass = CleanupPass::start(Cleanup::Jobs);
                        pass.await_batch(std::future::ready(Ok(7))).await?;
                        pass.completed();
                        Ok(PassEnd::Completed)
                    }
                    _ => {
                        cancel.cancel();
                        Ok(PassEnd::Cancelled)
                    }
                }
            }
        },
        || std::future::ready(Err(MaintenanceFailure::Statement)),
    );
    tokio::time::timeout(Duration::from_secs(180), cleanup)
        .await
        .expect("the stalled batch must not stop later scheduled passes");

    assert_eq!(passes, 3);
    let scrape = recorder.handle().render();
    assert!(
        scrape.contains(&format!(r#"{RUNS_METRIC}{{outcome="failed"}} 1"#)),
        "{scrape}"
    );
    assert!(
        scrape.contains(&format!(r#"{RUNS_METRIC}{{outcome="completed"}} 1"#)),
        "{scrape}"
    );
    let series = scrape
        .lines()
        .filter(|line| line.starts_with(RUNS_METRIC))
        .count();
    assert_eq!(series, 2, "the cancelled pass is not counted: {scrape}");
    assert!(
        scrape.contains(r#"postgres_cleanup_passes_total{cleanup="jobs",outcome="failed"} 1"#),
        "{scrape}"
    );
    assert!(
        scrape.contains(r#"postgres_cleanup_committed_batches_total{cleanup="jobs"} 1"#),
        "{scrape}"
    );
    assert!(
        scrape.contains(r#"postgres_cleanup_removed_rows_total{cleanup="jobs"} 7"#),
        "{scrape}"
    );
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
    assert!(!recorder.handle().render().contains(OBSERVER_ENABLED_METRIC));
}
