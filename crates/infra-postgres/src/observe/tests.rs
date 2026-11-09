use std::time::Duration;

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use sqlx::postgres::PgPoolOptions;

use super::*;
use crate::{Dsn, in_tx};

#[tokio::test]
async fn cleanup_admission_checks_both_sides_of_pacing_without_losing_confirmed_progress() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _recording = metrics::set_default_local_recorder(&recorder);
    let mut pass = CleanupPass::start(Cleanup::Jobs);
    pass.committed(500);
    let mut budget = CleanupBudget {
        started: tokio::time::Instant::now(),
        first: false,
        admission: Some(Duration::from_millis(100)),
        batch: Some(Duration::from_secs(12)),
        pause: Duration::from_millis(200),
    };
    let admission = budget.admit();
    tokio::pin!(admission);
    // Establish that the pre-pacing cutoff did not decide this attempt.
    assert!(futures_util::poll!(&mut admission).is_pending());
    assert!(!admission.await);
    pass.budget_exhausted();
    drop(pass);
    let scrape = recorder.handle().render();
    assert!(scrape.contains("postgres_cleanup_removed_rows_total{cleanup=\"jobs\"} 500"));
    assert!(scrape.contains(
        "postgres_cleanup_passes_total{cleanup=\"jobs\",outcome=\"budget_exhausted\"} 1"
    ));

    // An already expired pass returns synchronously, before another pace.
    let mut expired = CleanupBudget {
        started: tokio::time::Instant::now() - Duration::from_secs(5),
        first: false,
        admission: Some(Duration::from_secs(5)),
        batch: None,
        pause: Duration::from_secs(60),
    };
    let mut admission = std::pin::pin!(expired.admit());
    assert_eq!(
        futures_util::poll!(admission.as_mut()),
        std::task::Poll::Ready(false)
    );
}

#[tokio::test(start_paused = true)]
async fn a_read_past_the_budget_fails_the_attempt_and_keeps_the_last_sample() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _recording = metrics::set_default_local_recorder(&recorder);
    let mut observer = MaintenanceObserver::start(MaintenancePopulation::Jobs);
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

#[tokio::test]
async fn cleanup_batch_deadline_distinguishes_ready_acknowledgement_from_pending_work() {
    let budget = CleanupBudget {
        started: tokio::time::Instant::now(),
        first: true,
        admission: Some(Duration::from_secs(5)),
        batch: Some(Duration::ZERO),
        pause: Duration::ZERO,
    };
    assert_eq!(budget.batch(async { 500 }).await.unwrap(), 500);
    assert!(budget.batch(std::future::pending::<u64>()).await.is_err());
}

#[tokio::test]
async fn scheduled_spread_and_subsequent_fixed_delay_do_not_tick_immediately() {
    let mut schedule = CleanupSchedule::with_offset(Duration::from_millis(100));
    schedule.fixed_delay = true;
    {
        let mut first = std::pin::pin!(schedule.next());
        assert!(futures_util::poll!(first.as_mut()).is_pending());
        first.await;
    }
    let mut subsequent = std::pin::pin!(schedule.next());
    assert!(futures_util::poll!(subsequent.as_mut()).is_pending());
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
        let mut observer = MaintenanceObserver::start(MaintenancePopulation::Jobs);
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
            let mut observer = MaintenanceObserver::start(MaintenancePopulation::Jobs);
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
fn cleanup_buckets_expose_budget_duration_and_upper_tail_overflow_separately() {
    let recorder = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(CLEANUP_DURATION_METRIC.to_owned()),
            CLEANUP_DURATION_BUCKETS,
        )
        .unwrap()
        .build_recorder();
    metrics::with_local_recorder(&recorder, || {
        let mut yielded = CleanupPass::start(Cleanup::Jobs);
        yielded.started -= Duration::from_secs(17);
        yielded.committed(500);
        yielded.budget_exhausted();
        drop(yielded);
        let mut long = CleanupPass::start(Cleanup::Jobs);
        long.started -= Duration::from_secs(3601);
        long.completed();
        drop(long);
    });
    let scrape = recorder.handle().render();
    for (outcome, bound, count) in [
        ("budget_exhausted", "15", 0),
        ("budget_exhausted", "30", 1),
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

type EventFields = std::collections::BTreeMap<String, String>;

#[derive(Clone)]
struct Events {
    records: std::sync::Arc<std::sync::Mutex<Vec<EventFields>>>,
    level: tracing::Level,
}

impl Events {
    fn new(level: tracing::Level) -> Self {
        Self {
            records: std::sync::Arc::default(),
            level,
        }
    }
}

impl tracing::Subscriber for Events {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(EventFields);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.insert(field.name().to_owned(), format!("{value:?}"));
            }
        }
        assert_eq!(*event.metadata().level(), self.level);
        let mut fields = Fields(std::collections::BTreeMap::new());
        event.record(&mut fields);
        self.records.lock().unwrap().push(fields.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn acquisition_reports_only_slow_success_and_native_timeout() {
    let events = Events::new(tracing::Level::WARN);
    let started = Instant::now();
    tracing::subscriber::with_default(events.clone(), || {
        let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let budget = Duration::from_millis(2500);
                assert_eq!(
                    acquisition("fast", budget, async { Ok(7) }).await.unwrap(),
                    7
                );
                assert_eq!(
                    acquisition("slow", budget, async {
                        tokio::time::sleep(Duration::from_millis(1050)).await;
                        Ok(9)
                    })
                    .await
                    .unwrap(),
                    9
                );
                for error in [
                    sqlx::Error::PoolTimedOut,
                    sqlx::Error::PoolClosed,
                    sqlx::Error::Protocol("private server detail".to_owned()),
                ] {
                    let expected = format!("{error:?}");
                    let returned = acquisition("failed", budget, async { Err::<(), _>(error) })
                        .await
                        .unwrap_err();
                    assert_eq!(format!("{returned:?}"), expected);
                }
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(1),
                        acquisition(
                            "cancelled",
                            budget,
                            std::future::pending::<Result<(), sqlx::Error>>()
                        ),
                    )
                    .await
                    .is_err()
                );
            });
    });
    let events = events.records.lock().unwrap();
    assert_eq!(events.len(), 2, "{events:?}");
    let slow = &events[0];
    assert_eq!(slow["message"], "postgres_pool_acquire_slow");
    assert_eq!(slow["operation"], "\"slow\"");
    assert_eq!(slow["pool"], "\"postgres\"");
    assert_eq!(slow["threshold_seconds"], "1.0");
    let elapsed = slow["elapsed_seconds"].parse::<f64>().unwrap();
    assert!(elapsed > 1.0 && elapsed <= started.elapsed().as_secs_f64());
    assert_eq!(slow.len(), 5, "no additional disclosure fields");
    let timeout = &events[1];
    assert_eq!(timeout["message"], "postgres_pool_acquire_timeout");
    assert_eq!(timeout["operation"], "\"failed\"");
    assert_eq!(timeout["pool"], "\"postgres\"");
    assert_eq!(timeout["budget_seconds"], "2.5");
    assert!(timeout["elapsed_seconds"].parse::<f64>().unwrap() >= 0.0);
    assert_eq!(timeout.len(), 5, "no raw errors, DSNs or SQL");
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

#[test]
fn every_boundary_failure_has_its_own_outcome() {
    let io = || sqlx::Error::Io(std::io::Error::other("reset"));
    let outcomes = [
        (TxError::Acquire(io()), "acquire_failed"),
        (TxError::Begin(io()), "begin_failed"),
        (TxError::CommitFailed(io()), "commit_failed"),
        (TxError::CommitUnknown(io()), "commit_unknown"),
    ];
    for (error, label) in outcomes {
        assert_eq!(TransactionOutcome::of(&error).label(), label);
    }
}

#[test]
fn a_transaction_that_gets_no_connection_records_its_wait_and_its_outcome() {
    let mut builder = PrometheusBuilder::new();
    for &(metric, buckets) in HISTOGRAMS {
        builder = builder
            .set_buckets_for_metric(Matcher::Full(metric.to_owned()), buckets)
            .expect("the buckets are valid");
    }
    let recorder = builder.build_recorder();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    metrics::with_local_recorder(&recorder, || {
        describe();
        runtime.block_on(async {
            // Port 1 on loopback is refused at once; the pool retries
            // until its acquire budget ends.
            let dsn = Dsn::admit_with_environment(
                "postgres://app:pw@127.0.0.1:1/app?sslmode=disable",
                |_| false,
            )
            .expect("the dsn is admitted");
            let pool = PgPoolOptions::new()
                .acquire_timeout(Duration::from_millis(50))
                .connect_lazy_with(dsn.connect_options());
            let result: Result<(), TxError> = in_tx(&pool, async |_tx| Ok(())).await;
            assert!(matches!(result, Err(TxError::Acquire(_))), "{result:?}");
        });
        // A transaction dropped before it ended.
        drop(TransactionObservation::start(Span::none()));
    });
    let scrape = recorder.handle().render();
    for line in [
        "db_client_connection_wait_time_seconds_count{db_client_connection_pool_name=\"postgres\"} 1",
        "postgres_transaction_duration_seconds_count{outcome=\"acquire_failed\"} 1",
        "postgres_transaction_duration_seconds_count{outcome=\"cancelled\"} 1",
    ] {
        assert!(scrape.contains(line), "{line} is missing from:\n{scrape}");
    }
    // Two outcomes and one pool: nothing else became a series.
    assert_eq!(scrape.matches("_count{").count(), 3, "{scrape}");
    assert!(!scrape.contains("pw"), "{scrape}");
}

#[test]
fn acquire_until_waits_for_the_deadline_not_for_the_pool_timeout() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        // Port 1 on loopback is refused at once; the pool retries until its
        // own acquire timeout ends and then reports `PoolTimedOut`.
        let dsn = Dsn::admit_with_environment(
            "postgres://app:pw@127.0.0.1:1/app?sslmode=disable",
            |_| false,
        )
        .expect("the dsn is admitted");
        let pool = PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy_with(dsn.connect_options());

        let started = tokio::time::Instant::now();
        let waited = acquire_until(&pool, "test", started + Duration::from_millis(400)).await;
        assert!(matches!(waited, Err(sqlx::Error::PoolTimedOut)));
        assert!(
            started.elapsed() >= Duration::from_millis(400),
            "the wait outlasts the pool's {:?} timeout: {:?}",
            pool.options().get_acquire_timeout(),
            started.elapsed()
        );

        // A deadline inside the pool's timeout wins over it.
        let patient = PgPoolOptions::new()
            .acquire_timeout(Duration::from_secs(30))
            .connect_lazy_with(dsn.connect_options());
        let started = tokio::time::Instant::now();
        let cut = acquire_until(&patient, "test", started + Duration::from_millis(100)).await;
        assert!(matches!(cut, Err(sqlx::Error::PoolTimedOut)));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );

        let result: Result<(), TxError> = crate::in_tx_until(
            &pool,
            tokio::time::Instant::now() + Duration::from_millis(100),
            crate::TxOptions::default(),
            async |_tx| Ok(()),
        )
        .await;
        assert!(
            matches!(result, Err(TxError::Acquire(sqlx::Error::PoolTimedOut))),
            "{result:?}"
        );
    });
}

#[test]
fn a_statement_records_its_duration_by_summary_and_by_how_it_failed() {
    let recorder = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(OPERATION_DURATION_METRIC.to_owned()),
            OPERATION_DURATION_BUCKETS,
        )
        .expect("the buckets are valid")
        .build_recorder();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    metrics::with_local_recorder(&recorder, || {
        runtime.block_on(async {
            assert_eq!(observed("read widget", async { Ok(7) }).await.unwrap(), 7);
            let rejected = observed("write widget", async {
                Err::<(), _>(crate::error::tests::database("23505"))
            })
            .await;
            assert_eq!(sqlstate(&rejected.unwrap_err()).as_deref(), Some("23505"));
            let lost = observed("write widget", async {
                Err::<(), _>(sqlx::Error::Io(std::io::Error::other(
                    "reset by db.internal",
                )))
            })
            .await;
            assert!(lost.is_err());
            // The caller stops waiting before the server answers.
            let abandoned = tokio::time::timeout(
                Duration::from_millis(1),
                observed(
                    "slow widget",
                    std::future::pending::<Result<(), sqlx::Error>>(),
                ),
            )
            .await;
            assert!(abandoned.is_err());
        });
    });
    let scrape = recorder.handle().render();
    let series = |labels: &str| {
        format!(
            "db_client_operation_duration_seconds_count{{db_system_name=\"postgresql\",{labels}}} 1"
        )
    };
    for line in [
        series("db_query_summary=\"read widget\""),
        series("db_query_summary=\"write widget\",error_type=\"23505\""),
        series("db_query_summary=\"write widget\",error_type=\"io\""),
        series("db_query_summary=\"slow widget\",error_type=\"cancelled\""),
    ] {
        assert!(scrape.contains(&line), "{line} is missing from:\n{scrape}");
    }
    assert_eq!(scrape.matches("_count{").count(), 4, "{scrape}");
    assert!(!scrape.contains("db.internal"), "{scrape}");
}
