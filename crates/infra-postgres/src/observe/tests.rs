use metrics::Key;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use metrics_util::debugging::DebuggingRecorder;
use sqlx::postgres::PgPoolOptions;

use super::*;
use crate::{Dsn, acquire_until, in_tx};

pub(crate) type EventFields = std::collections::BTreeMap<String, String>;

#[derive(Clone)]
pub(crate) struct Events {
    pub(crate) records: std::sync::Arc<std::sync::Mutex<Vec<EventFields>>>,
    level: tracing::Level,
}

impl Events {
    pub(crate) fn new(level: tracing::Level) -> Self {
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

#[tokio::test]
async fn acquisition_reports_only_slow_success_and_native_timeout() {
    let events = Events::new(tracing::Level::WARN);
    let started = Instant::now();
    let _subscriber = tracing::subscriber::set_default(events.clone());
    let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let budget = Duration::from_millis(2500);
    assert_eq!(
        observe_acquisition("fast", budget, async { Ok(7) })
            .await
            .unwrap(),
        7
    );
    assert_eq!(
        observe_acquisition("slow", budget, async {
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
        let returned = observe_acquisition("failed", budget, async { Err::<(), _>(error) })
            .await
            .unwrap_err();
        assert_eq!(format!("{returned:?}"), expected);
    }
    assert!(
        tokio::time::timeout(
            Duration::from_millis(1),
            observe_acquisition(
                "cancelled",
                budget,
                std::future::pending::<Result<(), sqlx::Error>>()
            ),
        )
        .await
        .is_err()
    );
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

#[tokio::test]
async fn separate_pools_report_their_own_limits() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let pool = |max_connections| {
        PgPoolOptions::new()
            .max_connections(max_connections)
            .connect_lazy_with(sqlx::postgres::PgConnectOptions::new())
    };
    super::record_metrics(&pool(5), "write");
    super::record_metrics(&pool(20), "read");
    let snapshot = snapshotter.snapshot().into_hashmap();
    for (pool_name, expected) in [("write", 5.0), ("read", 20.0)] {
        let target = Key::from_parts(
            CONNECTION_MAX_METRIC,
            &[("db.client.connection.pool.name", pool_name)],
        );
        let actual = crate::test_metrics::gauge(&snapshot, &target);
        assert_eq!(actual, Some(expected), "{target}: {snapshot:?}");
    }
}

#[tokio::test]
async fn a_transaction_that_gets_no_connection_records_its_wait_and_its_outcome() {
    let mut builder = PrometheusBuilder::new();
    for &(metric, buckets) in HISTOGRAMS {
        builder = builder
            .set_buckets_for_metric(Matcher::Full(metric.to_owned()), buckets)
            .expect("the buckets are valid");
    }
    let recorder = builder.build_recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    describe();
    let dsn =
        Dsn::admit_with_environment("postgres://app:pw@127.0.0.1:1/app?sslmode=disable", |_| {
            false
        })
        .expect("the dsn is admitted");
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy_with(dsn.connect_options());
    let result: Result<(), TxError> = in_tx(&pool, async |_tx| Ok(())).await;
    std::assert_matches!(result, Err(TxError::Acquire(_)), "{result:?}");
    let expired: Result<(), Box<dyn std::error::Error>> = crate::in_tx_with_deadline(
        &pool,
        tokio::time::Instant::now(),
        crate::TxOptions::default(),
        async |_tx| Ok(()),
    )
    .await;
    assert!(expired.unwrap_err().is::<crate::OperationDeadlineElapsed>());
    drop(TransactionObservation::start(Span::none()));
    let scrape = recorder.handle().render();
    for line in [
        "db_client_connection_wait_time_seconds_count{db_client_connection_pool_name=\"postgres\"} 2",
        "postgres_transaction_duration_seconds_count{outcome=\"acquire_failed\"} 2",
        "postgres_transaction_duration_seconds_count{outcome=\"cancelled\"} 1",
    ] {
        assert!(scrape.contains(line), "{line} is missing from:\n{scrape}");
    }
    assert_eq!(scrape.matches("_count{").count(), 3, "{scrape}");
    assert!(!scrape.contains("pw"), "{scrape}");
}

#[tokio::test]
async fn acquire_until_waits_for_the_deadline_not_for_the_pool_timeout() {
    let dsn =
        Dsn::admit_with_environment("postgres://app:pw@127.0.0.1:1/app?sslmode=disable", |_| {
            false
        })
        .expect("the dsn is admitted");
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy_with(dsn.connect_options());

    let started = tokio::time::Instant::now();
    let waited = acquire_until(&pool, "test", started + Duration::from_millis(400)).await;
    std::assert_matches!(waited, Err(sqlx::Error::PoolTimedOut));
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "the wait outlasts the pool's {:?} timeout: {:?}",
        pool.options().get_acquire_timeout(),
        started.elapsed()
    );

    let patient = PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(30))
        .connect_lazy_with(dsn.connect_options());
    let started = tokio::time::Instant::now();
    let cut = acquire_until(&patient, "test", started + Duration::from_millis(100)).await;
    std::assert_matches!(cut, Err(sqlx::Error::PoolTimedOut));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );

    let result: Result<(), TxError> = crate::in_tx_with_acquire_deadline(
        &pool,
        tokio::time::Instant::now() + Duration::from_millis(100),
        crate::TxOptions::default(),
        async |_tx| Ok(()),
    )
    .await;
    std::assert_matches!(
        result,
        Err(TxError::Acquire(sqlx::Error::PoolTimedOut)),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_statement_records_its_duration_by_summary_and_by_how_it_failed() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _recorder = metrics::set_default_local_recorder(&recorder);
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
    let abandoned = tokio::time::timeout(
        Duration::from_millis(1),
        observed(
            "slow widget",
            std::future::pending::<Result<(), sqlx::Error>>(),
        ),
    )
    .await;
    assert!(abandoned.is_err());
    let snapshot = snapshotter.snapshot().into_hashmap();
    for target in [
        Key::from_parts(
            OPERATION_DURATION_METRIC,
            &[
                ("db.system.name", "postgresql"),
                ("db.query.summary", "read widget"),
            ],
        ),
        Key::from_parts(
            OPERATION_DURATION_METRIC,
            &[
                ("db.system.name", "postgresql"),
                ("db.query.summary", "write widget"),
                ("error.type", "23505"),
            ],
        ),
        Key::from_parts(
            OPERATION_DURATION_METRIC,
            &[
                ("db.system.name", "postgresql"),
                ("db.query.summary", "write widget"),
                ("error.type", "io"),
            ],
        ),
        Key::from_parts(
            OPERATION_DURATION_METRIC,
            &[
                ("db.system.name", "postgresql"),
                ("db.query.summary", "slow widget"),
                ("error.type", "cancelled"),
            ],
        ),
    ] {
        let count = crate::test_metrics::histogram_len(&snapshot, &target);
        assert_eq!(count, Some(1), "{target} is missing from {snapshot:?}");
    }
    assert_eq!(snapshot.len(), 4, "{snapshot:?}");
    assert!(
        !format!("{snapshot:?}").contains("db.internal"),
        "{snapshot:?}"
    );
}
