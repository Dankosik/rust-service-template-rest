use metrics::Key;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use metrics_util::debugging::DebuggingRecorder;
use sqlx::postgres::PgPoolOptions;

use super::*;
use crate::pool::acquire_until;
use crate::{Dsn, in_tx};

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

#[tokio::test(start_paused = true)]
async fn acquisition_reports_only_slow_success_and_native_timeout() {
    let events = Events::new(tracing::Level::WARN);
    let started = tokio::time::Instant::now();
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
        Pool::from(
            PgPoolOptions::new()
                .max_connections(max_connections)
                .connect_lazy_with(sqlx::postgres::PgConnectOptions::new()),
        )
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
    let pool = Pool::from(
        PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy_with(dsn.connect_options()),
    );
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
    drop(TransactionObservation::start(
        Span::none(),
        Arc::clone(pool.policy()),
    ));
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

struct CountingRecorder {
    inner: DebuggingRecorder,
    wait_registrations: std::sync::atomic::AtomicUsize,
    duration_registrations: std::sync::atomic::AtomicUsize,
}

impl metrics::Recorder for CountingRecorder {
    fn describe_counter(
        &self,
        key: metrics::KeyName,
        unit: Option<metrics::Unit>,
        description: metrics::SharedString,
    ) {
        self.inner.describe_counter(key, unit, description);
    }

    fn describe_gauge(
        &self,
        key: metrics::KeyName,
        unit: Option<metrics::Unit>,
        description: metrics::SharedString,
    ) {
        self.inner.describe_gauge(key, unit, description);
    }

    fn describe_histogram(
        &self,
        key: metrics::KeyName,
        unit: Option<metrics::Unit>,
        description: metrics::SharedString,
    ) {
        self.inner.describe_histogram(key, unit, description);
    }

    fn register_counter(&self, key: &Key, metadata: &metrics::Metadata<'_>) -> metrics::Counter {
        self.inner.register_counter(key, metadata)
    }

    fn register_gauge(&self, key: &Key, metadata: &metrics::Metadata<'_>) -> metrics::Gauge {
        self.inner.register_gauge(key, metadata)
    }

    fn register_histogram(
        &self,
        key: &Key,
        metadata: &metrics::Metadata<'_>,
    ) -> metrics::Histogram {
        use std::sync::atomic::Ordering;

        match key.name() {
            CONNECTION_WAIT_METRIC => {
                self.wait_registrations.fetch_add(1, Ordering::Relaxed);
            }
            TRANSACTION_DURATION_METRIC => {
                self.duration_registrations.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        self.inner.register_histogram(key, metadata)
    }
}

#[tokio::test]
async fn a_prepared_pool_records_wait_and_outcome_through_cached_metrics() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let recorder = CountingRecorder {
        inner: DebuggingRecorder::new(),
        wait_registrations: AtomicUsize::new(0),
        duration_registrations: AtomicUsize::new(0),
    };
    let snapshotter = recorder.inner.snapshotter();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let dsn =
        Dsn::admit_with_environment("postgres://app:pw@127.0.0.1:1/app?sslmode=disable", |_| {
            false
        })
        .expect("the dsn is admitted");
    let pool = crate::prepare_pool(
        &dsn,
        &crate::PoolOptions {
            max_connections: std::num::NonZeroU32::MIN,
            application_name: "svc",
            default_isolation: crate::Isolation::ServerDefault,
            session_settings_source: crate::SessionSettingsSource::Startup,
            wait_order: crate::WaitOrder::Arrival,
            notifications: true,
        },
    );
    for _ in 0..2 {
        let expired: Result<(), Box<dyn std::error::Error>> = crate::in_tx_with_deadline(
            &pool,
            tokio::time::Instant::now(),
            crate::TxOptions::default(),
            async |_tx| Ok(()),
        )
        .await;
        assert!(expired.unwrap_err().is::<crate::OperationDeadlineElapsed>());
    }
    assert_eq!(recorder.wait_registrations.load(Ordering::Relaxed), 1);
    assert_eq!(recorder.duration_registrations.load(Ordering::Relaxed), 1);
    let snapshot = snapshotter.snapshot().into_hashmap();
    let wait = Key::from_parts(
        CONNECTION_WAIT_METRIC,
        &[("db.client.connection.pool.name", "postgres")],
    );
    let duration = Key::from_parts(
        TRANSACTION_DURATION_METRIC,
        &[("outcome", "acquire_failed")],
    );
    assert_eq!(
        crate::test_metrics::histogram_len(&snapshot, &wait),
        Some(2),
        "{snapshot:?}"
    );
    assert_eq!(
        crate::test_metrics::histogram_len(&snapshot, &duration),
        Some(2),
        "{snapshot:?}"
    );
}

#[tokio::test]
async fn acquire_until_waits_for_the_deadline_not_for_the_pool_timeout() {
    let dsn =
        Dsn::admit_with_environment("postgres://app:pw@127.0.0.1:1/app?sslmode=disable", |_| {
            false
        })
        .expect("the dsn is admitted");
    let pool = Pool::from(
        PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy_with(dsn.connect_options()),
    );

    let started = tokio::time::Instant::now();
    let waited = acquire_until(&pool, "test", started + Duration::from_millis(400)).await;
    std::assert_matches!(waited, Err(sqlx::Error::PoolTimedOut));
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "the wait outlasts the pool's {:?} timeout: {:?}",
        pool.inner().options().get_acquire_timeout(),
        started.elapsed()
    );

    let patient = Pool::from(
        PgPoolOptions::new()
            .acquire_timeout(Duration::from_secs(30))
            .connect_lazy_with(dsn.connect_options()),
    );
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
    async fn read_widget() -> Result<i32, sqlx::Error> {
        crate::observed!("read widget", async { Ok(7) }).await
    }

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    assert_eq!(read_widget().await.unwrap(), 7);
    assert_eq!(
        crate::observed!("read widget", async { Ok(8) })
            .await
            .unwrap(),
        8
    );
    let rejected = crate::observed!("write widget", async {
        Err::<(), _>(crate::error::tests::database("23505"))
    })
    .await;
    assert_eq!(sqlstate(&rejected.unwrap_err()).as_deref(), Some("23505"));
    let lost = crate::observed!("write widget", async {
        Err::<(), _>(sqlx::Error::Io(std::io::Error::other(
            "reset by db.internal",
        )))
    })
    .await;
    assert!(lost.is_err());
    let abandoned = tokio::time::timeout(
        Duration::from_millis(1),
        crate::observed!(
            "slow widget",
            std::future::pending::<Result<(), sqlx::Error>>(),
        ),
    )
    .await;
    assert!(abandoned.is_err());

    let success = Key::from_parts(
        "db_client_operation_duration_seconds",
        &[
            ("db.system.name", "postgresql"),
            ("db.query.summary", "read widget"),
        ],
    );
    {
        let next_recorder = DebuggingRecorder::new();
        let _recorder = metrics::set_default_local_recorder(&next_recorder);
        assert_eq!(read_widget().await.unwrap(), 7);
        let snapshot = next_recorder.snapshotter().snapshot().into_hashmap();
        assert_eq!(
            crate::test_metrics::histogram_len(&snapshot, &success),
            Some(1)
        );
    }

    let snapshot = snapshotter.snapshot().into_hashmap();
    assert_eq!(
        crate::test_metrics::histogram_len(&snapshot, &success),
        Some(2)
    );
    for target in [
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
