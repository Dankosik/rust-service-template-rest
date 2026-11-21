#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests are test code; the production levels for unwrap, expect and panic do not apply"
)]

#[path = "postgres/operational_recovery.rs"]
mod operational_recovery;

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use backon::{ConstantBuilder, Retryable};
use health::Probe;
use infra_postgres::test_support::PASSWORD_REFRESH_INTERVAL;
use infra_postgres::{
    ACQUIRE_TIMEOUT, ConnectError, Dsn, Isolation, Pool, PoolOptions, PostgresProbe,
    SessionSettingsSource, SslPolicy, TxError, TxOptions, sqlstate,
};
use integration_tests::commit_proxy::{Boundary, CommitProxy, Fault, ReadyBoundary};
use integration_tests::{
    DATABASE_URL, dsn_for, fixture_dir, poll_until, pooler_dsn_for, propagate_join_panic, url_for,
};
use migrate::{HistoryError, MIGRATOR, RunError, RunOptions};
use sqlx::migrate::{Migrate, MigrateError, Migrator};
use sqlx::{AssertSqlSafe, Connection, Executor, PgPool};
use tokio_util::sync::CancellationToken;
use tracing::instrument::WithSubscriber;

const APP: &str = "integration-tests";

#[derive(Clone, Default)]
struct PoolEvents(observe_test_support::EventCapture);

impl PoolEvents {
    fn subscriber(&self) -> tracing::Dispatch {
        self.subscriber_for(&["infra_postgres::observe", "health"])
    }

    fn subscriber_for(&self, targets: &'static [&'static str]) -> tracing::Dispatch {
        self.0.dispatch_for_targets(targets)
    }

    fn take(&self) -> Vec<observe_test_support::FieldMap> {
        self.0
            .take()
            .into_iter()
            .map(|event| event.fields)
            .collect()
    }
}

async fn template_pool(dsn: &Dsn, max_connections: u32) -> Pool {
    integration_tests::template_pool(dsn, APP, max_connections, Isolation::ServerDefault).await
}

async fn order_served_by_a_one_connection_pool(
    pool: &PgPool,
    wait_order: infra_postgres::WaitOrder,
) -> Vec<&'static str> {
    let ours = Pool::connect(
        &dsn_for(pool).await,
        &PoolOptions {
            max_connections: NonZeroU32::MIN,
            application_name: APP,
            default_isolation: Isolation::ServerDefault,
            session_settings_source: SessionSettingsSource::Startup,
            wait_order,
            notifications: true,
        },
    )
    .await
    .unwrap();
    let held = ours.acquire("held").await.unwrap();
    let served = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let caller = |name: &'static str| {
        let ours = ours.clone();
        let served = std::sync::Arc::clone(&served);
        tokio::spawn(async move {
            let connection = ours.acquire(name).await.unwrap();
            served.lock().unwrap().push(name);
            drop(connection);
        })
    };

    let first = caller("first");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let early = caller("early");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let late = caller("late");
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(held);

    for task in [first, early, late] {
        task.await.unwrap();
    }
    infra_postgres::test_support::as_sqlx(&ours).close().await;
    std::mem::take(&mut *served.lock().unwrap())
}

#[sqlx::test(migrations = "../migrations")]
async fn callers_beyond_the_pool_are_served_newest_first_once_the_queue_stands(pool: PgPool) {
    assert_eq!(
        order_served_by_a_one_connection_pool(&pool, infra_postgres::WaitOrder::NewestOnceStanding)
            .await,
        ["first", "late", "early"]
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn a_pool_in_arrival_order_serves_its_longest_waiter_first(pool: PgPool) {
    assert_eq!(
        order_served_by_a_one_connection_pool(&pool, infra_postgres::WaitOrder::Arrival).await,
        ["first", "early", "late"]
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn ipv6_literal_reaches_the_admitted_database(pool: PgPool) {
    let mut url = url_for(&pool, DATABASE_URL).await;
    let target = (url.host_str().unwrap().to_owned(), url.port().unwrap());
    let listener = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
    url.set_host(Some("[::1]")).unwrap();
    url.set_port(Some(listener.local_addr().unwrap().port()))
        .unwrap();
    let dsn = Dsn::admit_with(url.as_str(), None, SslPolicy::AllowLocalDevelopment).unwrap();
    let work = async {
        let connected = template_pool(&dsn, 1).await;
        let result = sqlx::query_scalar::<_, String>("SELECT current_database()")
            .fetch_one(infra_postgres::test_support::as_sqlx(&connected))
            .await;
        infra_postgres::test_support::as_sqlx(&connected)
            .close()
            .await;
        result
    };
    let relay = async {
        let (mut incoming, _) = listener.accept().await.unwrap();
        let mut outgoing = tokio::net::TcpStream::connect(target).await.unwrap();
        tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await
    };
    let (result, relay) = Box::pin(tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(work, relay)
    }))
    .await
    .expect("IPv6 database exchange and relay terminate");
    relay.unwrap();
    assert_eq!(result.unwrap(), url.path().trim_start_matches('/'));
}

async fn pool_with_default_isolation(
    dsn: &Dsn,
    max_connections: u32,
    default_isolation: Isolation,
) -> Pool {
    Pool::connect(
        dsn,
        &PoolOptions {
            max_connections: NonZeroU32::new(max_connections).expect("pool size in tests"),
            application_name: APP,
            default_isolation,
            session_settings_source: SessionSettingsSource::Startup,
            wait_order: infra_postgres::WaitOrder::Arrival,
            notifications: true,
        },
    )
    .await
    .expect("pool connects")
}

async fn pool_with_server_budgets(
    dsn: &Dsn,
    default_isolation: Isolation,
) -> Result<Pool, ConnectError> {
    Pool::connect(
        dsn,
        &PoolOptions {
            max_connections: NonZeroU32::MIN,
            application_name: APP,
            default_isolation,
            session_settings_source: SessionSettingsSource::Server,
            wait_order: infra_postgres::WaitOrder::Arrival,
            notifications: true,
        },
    )
    .await
}

async fn alter_database(pool: &PgPool, setting: &str) {
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query(AssertSqlSafe(format!(
        "ALTER DATABASE {database:?} SET {setting}"
    )))
    .execute(pool)
    .await
    .unwrap();
}

async fn server_address(dsn: &Dsn) -> SocketAddr {
    let host = dsn.host().trim_start_matches('[').trim_end_matches(']');
    tokio::net::lookup_host((host, dsn.port()))
        .await
        .expect("the server address resolves")
        .next()
        .expect("the server has an address")
}

async fn dsn_at(pool: &PgPool, address: SocketAddr) -> Dsn {
    let mut url = url_for(pool, DATABASE_URL).await;
    url.set_ip_host(address.ip())
        .expect("the relay address is a host");
    url.set_port(Some(address.port()))
        .expect("the relay URL accepts a port");
    Dsn::admit_with(url.as_str(), None, SslPolicy::AllowLocalDevelopment)
        .expect("the relayed DSN is admitted")
}

async fn proxied_pool(pool: &PgPool, max_connections: u32) -> (CommitProxy, Pool) {
    let (proxy, proxied) = CommitProxy::start_for(pool).await;
    (proxy, template_pool(&proxied, max_connections).await)
}

const RETURN_OBSERVATION_BUDGET: Duration = Duration::from_secs(7);

async fn wait_for_idle(pool: &Pool) {
    poll_until(
        "a healthy return reaches the idle queue",
        RETURN_OBSERVATION_BUDGET,
        Duration::from_millis(10),
        async || (pool.num_idle() > 0).then_some(()),
    )
    .await;
}

async fn wait_for_slot_reclamation(pool: &Pool) {
    poll_until(
        "the silent connection releases its local pool slot without a reply",
        RETURN_OBSERVATION_BUDGET,
        Duration::from_millis(10),
        async || (pool.size() == 0).then_some(()),
    )
    .await;
}

async fn show(pool: &Pool, setting: &'static str) -> String {
    let sql = match setting {
        "statement_timeout" => "SHOW statement_timeout",
        "idle_in_transaction_session_timeout" => "SHOW idle_in_transaction_session_timeout",
        "application_name" => "SHOW application_name",
        "default_transaction_isolation" => "SHOW default_transaction_isolation",
        other => panic!("unexpected setting {other}"),
    };
    sqlx::query_scalar(sql)
        .fetch_one(infra_postgres::test_support::as_sqlx(pool))
        .await
        .unwrap()
}

async fn fixture(name: &str) -> Migrator {
    Migrator::new(fixture_dir(name).as_path())
        .await
        .expect("fixture resolves")
}

fn options(dsn: &Dsn) -> RunOptions<'_> {
    RunOptions::defaults(dsn, APP, Duration::from_mins(5))
}

async fn applied_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[derive(Debug, derive_more::From)]
enum AppError {
    Tx(TxError),
    Deadline(infra_postgres::OperationDeadlineElapsed),
    Query(sqlx::Error),
    #[from(skip)]
    Business,
}

#[sqlx::test(migrations = false)]
async fn silent_session_readback_rejects_admission_before_the_relay_is_released(pool: PgPool) {
    for session_settings_source in [
        SessionSettingsSource::Startup,
        SessionSettingsSource::Server,
    ] {
        let (proxy, dsn) = CommitProxy::start_for(&pool).await;
        proxy.arm(
            Fault::ForwardThenSilence,
            Boundary::Autocommit("pg_settings"),
        );
        let options = PoolOptions {
            max_connections: NonZeroU32::MIN,
            application_name: APP,
            default_isolation: Isolation::ServerDefault,
            session_settings_source,
            wait_order: infra_postgres::WaitOrder::Arrival,
            notifications: true,
        };
        let started = Instant::now();
        let mut admission = tokio::spawn(async move { Pool::connect(&dsn, &options).await });
        proxy.silenced().await;
        let result = tokio::time::timeout(Duration::from_secs(12), &mut admission).await;
        let elapsed = started.elapsed();
        proxy.shutdown().await;
        if result.is_err() {
            admission.abort();
            let _ = admission.await;
        }
        let error = result
            .expect("verification and rejection cleanup are bounded")
            .expect("admission task completes")
            .unwrap_err();
        std::assert_matches!(error, ConnectError::SessionVerificationTimeout { budget } if budget == Duration::from_secs(5),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            "postgres session verification: did not complete inside the 5s budget"
        );
        assert!(
            elapsed >= Duration::from_secs(5),
            "the peer remains silent until the client deadline"
        );
        assert!(
            elapsed < Duration::from_secs(12),
            "verification plus cleanup and scheduling allowance"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn pending_session_admission_keeps_a_closeable_pool_and_bounds_its_wait(pool: PgPool) {
    for expire in [false, true] {
        let (proxy, proxied) = CommitProxy::start_for(&pool).await;
        let options = PoolOptions {
            max_connections: NonZeroU32::MIN,
            application_name: APP,
            default_isolation: Isolation::ServerDefault,
            session_settings_source: SessionSettingsSource::Startup,
            wait_order: infra_postgres::WaitOrder::Arrival,
            notifications: true,
        };
        let ours = Pool::prepare(&proxied, &options);
        proxy.arm(
            Fault::ForwardThenSilence,
            Boundary::Autocommit("pg_settings"),
        );
        let started = Instant::now();
        let mut admission = Box::pin(ours.admit());
        tokio::select! {
            result = &mut admission => panic!("session admission completed before its held reply: {result:?}"),
            () = proxy.silenced() => {}
        }
        if expire {
            let refused = tokio::time::timeout(Duration::from_secs(7), &mut admission)
                .await
                .expect("session admission must have its own client deadline")
                .unwrap_err();
            std::assert_matches!(refused, ConnectError::SessionVerificationTimeout { budget } if budget == Duration::from_secs(5),
                "{refused}"
            );
            assert!(started.elapsed() >= Duration::from_secs(5));
            assert!(started.elapsed() < Duration::from_secs(7));
        }
        drop(admission);
        assert!(
            !ours.is_closed(),
            "the process still owns the retained pool"
        );
        assert_eq!(
            ours.close(RETURN_OBSERVATION_BUDGET).await,
            infra_postgres::CloseOutcome::Complete,
            "cleanup must finish while the old socket remains silent"
        );
        assert!(ours.is_closed());
        proxy.shutdown().await;
    }
}

#[sqlx::test(migrations = false)]
async fn pool_publishes_the_session_defaults(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let ours = template_pool(&dsn, 2).await;
    assert_eq!(show(&ours, "statement_timeout").await, "8s");
    assert_eq!(
        show(&ours, "idle_in_transaction_session_timeout").await,
        "8s"
    );
    assert_eq!(show(&ours, "application_name").await, APP);
    ours.record_metrics("postgres");
    assert_eq!(
        ours.close(Duration::from_secs(5)).await,
        infra_postgres::CloseOutcome::Complete
    );
}

#[sqlx::test(migrations = false)]
async fn pool_default_isolation_survives_replacement_and_explicit_transactions_override_it(
    pool: PgPool,
) {
    let dsn = dsn_for(&pool).await;
    let ours = pool_with_default_isolation(&dsn, 1, Isolation::ReadCommitted).await;

    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
        .await
        .unwrap();
    let physical_connection = infra_postgres::test_support::as_sqlx(&ours)
        .acquire()
        .await
        .unwrap();
    physical_connection.close().await.unwrap();

    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
        .await
        .unwrap();
    assert_ne!(
        first_pid, second_pid,
        "the pool opened a replacement connection"
    );
    assert_eq!(
        show(&ours, "default_transaction_isolation").await,
        "read committed"
    );

    let explicit: Result<String, AppError> = ours
        .in_tx_with(
            TxOptions {
                isolation: Isolation::Serializable,
                read_only: false,
            },
            async |tx| {
                Ok(sqlx::query_scalar("SHOW transaction_isolation")
                    .fetch_one(&mut *tx)
                    .await?)
            },
        )
        .await;
    assert_eq!(explicit.unwrap(), "serializable");
}

#[sqlx::test(migrations = false)]
async fn an_idle_connection_the_server_closed_is_replaced_before_use(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let ours = template_pool(&dsn, 1).await;
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
        .await
        .unwrap();

    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(first_pid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(terminated);
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
        .await
        .expect("the idle ping discards the closed connection");
    assert_ne!(first_pid, second_pid);
}

#[sqlx::test(migrations = false)]
async fn an_idle_connection_whose_peer_went_silent_is_replaced_inside_the_acquire_budget(
    pool: PgPool,
) {
    let (proxy, ours) = proxied_pool(&pool, 1).await;
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
        .await
        .unwrap();

    wait_for_idle(&ours).await;
    proxy.silence_connection().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let started = Instant::now();
    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
        .await
        .expect("the bounded idle ping discards the silent connection");
    assert_ne!(first_pid, second_pid);
    assert!(started.elapsed() >= Duration::from_millis(1900));
    assert!(started.elapsed() < ACQUIRE_TIMEOUT);
    infra_postgres::test_support::as_sqlx(&ours).close().await;
    proxy.shutdown().await;
}

#[sqlx::test(migrations = false)]
async fn cancelled_operations_release_capacity_while_the_old_socket_stays_silent(pool: PgPool) {
    pool.execute("CREATE TABLE pool_return_finality (id text PRIMARY KEY)")
        .await
        .unwrap();
    for boundary in [
        "pooled statement",
        "transaction statement",
        "pre-commit",
        "commit",
    ] {
        let (proxy, ours) = proxied_pool(&pool, 1).await;
        let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
            .await
            .unwrap();
        let mut pending = Box::pin(async {
            if boundary == "pooled statement" {
                proxy.arm(
                    Fault::ForwardThenSilence,
                    Boundary::Autocommit("pool_return_finality"),
                );
                sqlx::query("INSERT INTO pool_return_finality VALUES ($1)")
                    .bind(boundary)
                    .execute(infra_postgres::test_support::as_sqlx(&ours))
                    .await?;
                return Ok::<(), AppError>(());
            }
            if boundary == "transaction statement" {
                proxy.arm(
                    Fault::ForwardThenSilence,
                    Boundary::Autocommit("pool_return_finality"),
                );
            } else if boundary == "commit" {
                proxy.arm(Fault::ForwardThenSilence, Boundary::Commit(None));
            }
            ours.in_tx(async |tx| {
                sqlx::query("INSERT INTO pool_return_finality VALUES ($1)")
                    .bind(boundary)
                    .execute(&mut *tx)
                    .await?;
                if boundary == "pre-commit" {
                    let _ = tx.connection();
                    proxy.arm(Fault::ForwardThenSilence, Boundary::Autocommit("SELECT 1"));
                }
                Ok::<(), AppError>(())
            })
            .await
        });
        let events = PoolEvents::default();
        async {
            tokio::select! {
                result = &mut pending => panic!("{boundary} completed before its held reply: {result:?}"),
                () = proxy.silenced() => {}
            }
            drop(pending);
        }
        .with_subscriber(events.subscriber())
        .await;
        let events = events.take();
        let mut cancellations = events
            .iter()
            .filter(|event| event["message"] == "postgres_transaction_cancelled");
        if boundary == "commit" {
            let event = cancellations.next().expect("cancelled COMMIT diagnostic");
            assert_eq!(event["phase"], "\"commit\"");
            assert_eq!(event["commit_outcome"], "\"unknown\"");
        }
        assert!(cancellations.next().is_none(), "{boundary}: {events:?}");

        wait_for_slot_reclamation(&ours).await;
        let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
            .await
            .expect("later ordinary work succeeds with the same one-slot pool");
        assert_ne!(first_pid, second_pid, "{boundary}");
        let reused_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
            .await
            .unwrap();
        assert_eq!(
            reused_pid, second_pid,
            "healthy replacement remains reusable"
        );
        assert_eq!(
            infra_postgres::test_support::as_sqlx(&ours)
                .options()
                .get_max_connections(),
            1
        );
        let visible: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pool_return_finality WHERE id = $1")
                .bind(boundary)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            visible,
            i64::from(matches!(boundary, "pooled statement" | "commit")),
            "server visibility is independent of cancelled client work at {boundary}"
        );
        assert!(PostgresProbe::new(ours.clone()).check().await.is_ok());
        infra_postgres::test_support::as_sqlx(&ours).close().await;
        proxy.shutdown().await;
    }
}

#[sqlx::test(migrations = false)]
async fn a_successful_statement_still_has_a_bounded_silent_return(pool: PgPool) {
    let (proxy, ours) = proxied_pool(&pool, 1).await;
    for _ in 0..2 {
        let mut conn = infra_postgres::test_support::as_sqlx(&ours)
            .acquire()
            .await
            .unwrap();
        let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        proxy.silence_connection().await;
        drop(conn);
        wait_for_slot_reclamation(&ours).await;
        let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
            .await
            .unwrap();
        assert_ne!(first_pid, second_pid);
        assert_eq!(
            infra_postgres::test_support::as_sqlx(&ours)
                .options()
                .get_max_connections(),
            1
        );
        wait_for_idle(&ours).await;
    }
    infra_postgres::test_support::as_sqlx(&ours).close().await;
    proxy.shutdown().await;
}

#[sqlx::test(migrations = false)]
async fn a_cancelled_readiness_ping_releases_its_silent_connection(pool: PgPool) {
    let (proxy, ours) = proxied_pool(&pool, 1).await;
    wait_for_idle(&ours).await;
    proxy.silence_connection().await;
    let probe = PostgresProbe::new(ours.clone());
    let result = tokio::time::timeout(Duration::from_millis(200), probe.check()).await;
    assert!(
        result.is_err(),
        "the caller cancels while the ping has no reply"
    );
    assert_eq!(ours.num_idle(), 0);
    wait_for_slot_reclamation(&ours).await;
    assert!(
        probe.check().await.is_ok(),
        "readiness can use the replacement"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT 42")
            .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
            .await
            .unwrap(),
        42
    );
    infra_postgres::test_support::as_sqlx(&ours).close().await;
    proxy.shutdown().await;
}

#[sqlx::test(migrations = false)]
async fn cancelling_an_acquire_wait_preserves_capacity_and_healthy_reuse(pool: PgPool) {
    let ours = template_pool(&dsn_for(&pool).await, 1).await;
    let mut held = infra_postgres::test_support::as_sqlx(&ours)
        .acquire()
        .await
        .unwrap();
    let before: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            infra_postgres::test_support::as_sqlx(&ours).acquire()
        )
        .await
        .is_err()
    );
    drop(held);
    let after: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "a cancelled waiter does not discard a healthy return"
    );
    infra_postgres::test_support::as_sqlx(&ours).close().await;
}

#[sqlx::test(migrations = false)]
async fn budgets_the_server_carries_are_verified_when_the_pool_opens(pool: PgPool) {
    let dsn = dsn_for(&pool).await;

    let unlimited = pool_with_server_budgets(&dsn, Isolation::ServerDefault)
        .await
        .unwrap_err();
    std::assert_matches!(
        unlimited,
        ConnectError::SessionBudget {
            setting: "statement_timeout",
            found_ms: 0,
            ..
        },
        "{unlimited}"
    );

    alter_database(&pool, "statement_timeout = '30s'").await;
    alter_database(&pool, "idle_in_transaction_session_timeout = '8s'").await;
    let looser = pool_with_server_budgets(&dsn, Isolation::ServerDefault)
        .await
        .unwrap_err();
    std::assert_matches!(
        looser,
        ConnectError::SessionBudget {
            setting: "statement_timeout",
            found_ms: 30_000,
            ..
        },
        "{looser}"
    );

    alter_database(&pool, "statement_timeout = '5s'").await;
    let ours = pool_with_server_budgets(&dsn, Isolation::ServerDefault)
        .await
        .expect("the database carries both budgets");
    assert_eq!(show(&ours, "statement_timeout").await, "5s");
    assert_eq!(
        show(&ours, "idle_in_transaction_session_timeout").await,
        "8s"
    );
    infra_postgres::test_support::as_sqlx(&ours).close().await;

    alter_database(&pool, "default_transaction_isolation = 'serializable'").await;
    let isolation = pool_with_server_budgets(&dsn, Isolation::ReadCommitted)
        .await
        .unwrap_err();
    std::assert_matches!(
        isolation,
        ConnectError::SessionIsolation {
            expected: "READ COMMITTED",
            ..
        },
        "{isolation}"
    );
}

#[sqlx::test(migrations = false)]
async fn through_a_transaction_pooler_the_published_budgets_reach_every_transaction(pool: PgPool) {
    sqlx::query("CREATE TABLE pooled_items (id int PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    let ours =
        pool_with_default_isolation(&pooler_dsn_for(&pool).await, 2, Isolation::ReadCommitted)
            .await;
    assert_eq!(show(&ours, "statement_timeout").await, "8s");
    assert_eq!(
        show(&ours, "idle_in_transaction_session_timeout").await,
        "8s"
    );
    assert_eq!(
        show(&ours, "default_transaction_isolation").await,
        "read committed"
    );
    assert_eq!(show(&ours, "application_name").await, APP);

    let insert = async |id: i32| -> Result<(), AppError> {
        ours.in_tx(async |tx| {
            sqlx::query("INSERT INTO pooled_items (id) VALUES ($1)")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            Ok(())
        })
        .await
    };
    for round in 0..3 {
        let (left, right) = tokio::join!(insert(round * 2), insert(round * 2 + 1));
        left.unwrap();
        right.unwrap();
    }
    let rolled_back: Result<(), AppError> = ours
        .in_tx(async |tx| {
            sqlx::query("INSERT INTO pooled_items (id) VALUES (100)")
                .execute(&mut *tx)
                .await?;
            Err(AppError::Business)
        })
        .await;
    std::assert_matches!(rolled_back, Err(AppError::Business));
    let swallowed: Result<(), AppError> = ours
        .in_tx(async |tx| {
            let duplicate = sqlx::query("INSERT INTO pooled_items (id) VALUES (0)")
                .execute(&mut *tx)
                .await;
            assert!(duplicate.is_err());
            Ok(())
        })
        .await;
    std::assert_matches!(
        swallowed,
        Err(AppError::Tx(TxError::CommitFailed(_))),
        "{swallowed:?}"
    );
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM pooled_items")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, 6);
}

#[sqlx::test(migrations = false)]
async fn through_a_pooler_the_database_carries_the_budgets_when_nothing_is_published(pool: PgPool) {
    alter_database(&pool, "statement_timeout = '8s'").await;
    alter_database(&pool, "idle_in_transaction_session_timeout = '8s'").await;
    let ours = pool_with_server_budgets(&pooler_dsn_for(&pool).await, Isolation::ReadCommitted)
        .await
        .expect("the database carries both budgets");
    assert_eq!(show(&ours, "statement_timeout").await, "8s");
    let committed: Result<i32, AppError> = ours
        .in_tx(async |tx| Ok(sqlx::query_scalar("SELECT 1").fetch_one(&mut *tx).await?))
        .await;
    assert_eq!(committed.unwrap(), 1);
}

#[sqlx::test(migrations = false)]
#[warn(clippy::let_underscore_must_use)]
async fn a_rotated_password_file_reaches_the_connections_opened_after_it(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let database = dsn.database();
    let tail_start = database.ceil_char_boundary(database.len().saturating_sub(24));
    let role = format!(
        "rotating_{}",
        database.get(tail_start..).unwrap_or(database)
    );
    sqlx::query(AssertSqlSafe(format!(
        "CREATE ROLE {role:?} LOGIN PASSWORD 'first'"
    )))
    .execute(&pool)
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("password");
    tokio::fs::write(&file, "first\n").await.unwrap();
    let mut url = url_for(&pool, DATABASE_URL).await;
    url.set_username(&role).unwrap();
    url.set_password(None).unwrap();
    let rotating = Dsn::admit_with(url.as_str(), Some(&file), SslPolicy::AllowLocalDevelopment)
        .expect("the file is the password");
    let ours = template_pool(&rotating, 1).await;
    let cancel = CancellationToken::new();
    let refresh = tokio::spawn(
        ours.clone()
            .refresh_password_periodically(file.clone(), cancel.clone()),
    );
    let current_user = || async {
        sqlx::query_scalar::<_, String>("SELECT current_user")
            .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
            .await
    };
    assert_eq!(current_user().await.unwrap(), role);

    sqlx::query(AssertSqlSafe(format!(
        "ALTER ROLE {role:?} PASSWORD 'second'"
    )))
    .execute(&pool)
    .await
    .unwrap();
    infra_postgres::test_support::as_sqlx(&ours)
        .acquire()
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let refused = current_user().await.unwrap_err();
    assert_eq!(sqlstate(&refused).as_deref(), Some("28P01"), "{refused}");

    tokio::fs::write(&file, "second\n").await.unwrap();
    let retry_delay = Duration::from_millis(200);
    let max_times = usize::try_from(
        (PASSWORD_REFRESH_INTERVAL + Duration::from_secs(5)).as_millis() / retry_delay.as_millis(),
    )
    .unwrap();
    let user = (|| async { current_user().await })
        .retry(
            ConstantBuilder::default()
                .with_delay(retry_delay)
                .with_max_times(max_times),
        )
        .when(|err| sqlstate(err).as_deref() == Some("28P01"))
        .await
        .unwrap_or_else(|err| panic!("the pool never picked up the rotated password: {err}"));
    assert_eq!(user, role);

    cancel.cancel();
    refresh.await.unwrap();
    infra_postgres::test_support::as_sqlx(&ours).close().await;
    sqlx::query(AssertSqlSafe(format!("DROP ROLE {role:?}")))
        .execute(&pool)
        .await
        .expect("remove the password rotation role");
}

#[sqlx::test(migrations = false)]
async fn acquisition_diagnostics_cover_transactions_history_and_readiness(pool: PgPool) {
    let ours = template_pool(&dsn_for(&pool).await, 1).await;
    let probe = PostgresProbe::new(ours.clone());
    let events = PoolEvents::default();
    let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    async {
        let held = infra_postgres::test_support::as_sqlx(&ours)
            .acquire()
            .await
            .unwrap();
        let started = Instant::now();
        let (transaction, history, readiness) = tokio::join!(
            ours.in_tx(async |_tx| Ok::<_, AppError>(())),
            migrate::verify_history(&ours),
            probe.check(),
        );
        std::assert_matches!(
            transaction,
            Err(AppError::Tx(TxError::Acquire(sqlx::Error::PoolTimedOut)))
        );
        assert_eq!(history, Err(HistoryError::Unavailable));
        assert_eq!(
            readiness.unwrap_err().to_string(),
            "no connection available inside the acquire budget"
        );
        let timeouts = events.take();
        assert_eq!(
            timeouts.len(),
            3,
            "one event per actual acquisition: {timeouts:?}"
        );
        for operation in ["transaction", "check migration history", "readiness"] {
            let event = timeouts
                .iter()
                .find(|event| event["operation"] == format!("{operation:?}"))
                .unwrap();
            assert_eq!(event["message"], "postgres_pool_acquire_timeout");
            assert_eq!(event["pool"], "\"postgres\"");
            assert_eq!(event["budget_seconds"], "3.0");
            let elapsed = event["elapsed_seconds"].parse::<f64>().unwrap();
            assert!(elapsed >= 2.8 && elapsed <= started.elapsed().as_secs_f64());
        }

        let cancelled = tokio::time::timeout(
            Duration::from_millis(20),
            ours.acquire("cancelled diagnostic"),
        )
        .await;
        assert!(cancelled.is_err());
        assert!(
            events.take().is_empty(),
            "unfinished acquire has no invented outcome"
        );
        drop(held);
        wait_for_idle(&ours).await;

        let held = infra_postgres::test_support::as_sqlx(&ours)
            .acquire()
            .await
            .unwrap();
        let release = async {
            tokio::time::sleep(Duration::from_millis(1100)).await;
            drop(held);
        };
        let ((), transaction, history, readiness) = tokio::join!(
            release,
            ours.in_tx(async |tx| {
                Ok::<i32, AppError>(sqlx::query_scalar("SELECT 42").fetch_one(&mut *tx).await?)
            }),
            migrate::verify_history(&ours),
            probe.check(),
        );
        assert_eq!(transaction.unwrap(), 42);
        assert_eq!(
            history,
            Err(HistoryError::Pending),
            "acquired successfully and read the empty history"
        );
        assert!(readiness.is_ok());
        let slow = events.take();
        assert_eq!(slow.len(), 3, "one slow success per named path: {slow:?}");
        for operation in ["transaction", "check migration history", "readiness"] {
            let event = slow
                .iter()
                .find(|event| event["operation"] == format!("{operation:?}"))
                .unwrap();
            assert_eq!(event["message"], "postgres_pool_acquire_slow");
            assert_eq!(event["threshold_seconds"], "1.0");
            assert!(event["elapsed_seconds"].parse::<f64>().unwrap() > 1.0);
        }

        let execution: Result<i32, _> = infra_postgres::observed!("diagnostic division", async {
            let mut connection = ours.acquire("diagnostic division").await?;
            sqlx::query_scalar("SELECT 1 / 0")
                .fetch_one(&mut *connection)
                .await
        })
        .await;
        assert_eq!(sqlstate(&execution.unwrap_err()).as_deref(), Some("22012"));
        assert!(
            events.take().is_empty(),
            "execution failure is not an acquire timeout"
        );
        infra_postgres::test_support::as_sqlx(&ours).close().await;
        std::assert_matches!(
            ours.acquire("closed diagnostic").await,
            Err(sqlx::Error::PoolClosed)
        );
        assert!(
            events.take().is_empty(),
            "pool closure is not an acquire timeout"
        );
    }
    .with_subscriber(events.subscriber())
    .await;
}

#[expect(
    clippy::print_stdout,
    reason = "print observed recovery timing with --no-capture"
)]
#[sqlx::test(migrations = false)]
async fn responsive_saturation_recovers_work_and_readiness_under_current_policy(pool: PgPool) {
    let ours = template_pool(&dsn_for(&pool).await, 1).await;
    let interval = Duration::from_secs(2);
    let probe_budget = Duration::from_secs(4);
    let policy = health::RefreshPolicy::try_new(
        interval,
        probe_budget,
        std::num::NonZeroU32::new(3).unwrap(),
    )
    .unwrap();
    let readiness = health::Readiness::new(
        vec![health::BoxedProbe::new(PostgresProbe::new(ours.clone()))],
        policy,
    );
    let reader = readiness.control().reader();
    let refresher = readiness.prepare().await.unwrap().activate().unwrap();
    assert!(reader.verdict().is_ok());
    let mut held = infra_postgres::test_support::as_sqlx(&ours)
        .acquire()
        .await
        .unwrap();
    let events = PoolEvents::default();
    let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let cancel = CancellationToken::new();
    let observe = async {
        let saturated = Instant::now();
        let verdict = poll_until(
            "current policy withdraws readiness after bounded saturation",
            Duration::from_secs(20),
            Duration::from_millis(20),
            async || reader.verdict().err(),
        )
        .await;
        std::assert_matches!(
            verdict,
            health::NotReady::ProbeFailed {
                probe: "postgres",
                ..
            },
            "{verdict:?}"
        );
        let responsive: i32 = sqlx::query_scalar("SELECT 7")
            .fetch_one(&mut *held)
            .await
            .unwrap();
        assert_eq!(responsive, 7);
        let loss = events.take();
        assert_eq!(
            loss.iter()
                .filter(|event| event["message"] == "postgres_pool_acquire_timeout")
                .count(),
            3
        );
        assert!(
            loss.iter()
                .any(|event| event["message"] == "readiness_lost")
        );
        let lost_after = saturated.elapsed();
        let released = Instant::now();
        drop(held);
        let value: i32 = ours
            .in_tx(async |tx| {
                Ok::<i32, AppError>(sqlx::query_scalar("SELECT 42").fetch_one(&mut *tx).await?)
            })
            .await
            .unwrap();
        assert_eq!(value, 42);
        poll_until(
            "next successful refresh restores readiness",
            interval + probe_budget + Duration::from_secs(1),
            Duration::from_millis(20),
            async || reader.verdict().ok(),
        )
        .await;
        let recovery = events.take();
        assert!(
            recovery
                .iter()
                .any(|event| event["message"] == "readiness_recovered")
        );
        assert_eq!(
            infra_postgres::test_support::as_sqlx(&ours)
                .options()
                .get_max_connections(),
            1
        );
        println!(
            "responsive saturation: readiness lost after {lost_after:?}; useful work and readiness recovered {:?} after release; pool max=1",
            released.elapsed()
        );
        cancel.cancel();
    };
    async {
        tokio::join!(refresher.refresh_until(cancel.clone()), observe);
    }
    .with_subscriber(events.subscriber())
    .await;
    infra_postgres::test_support::as_sqlx(&ours).close().await;
}

#[sqlx::test(migrations = false)]
async fn in_tx_commits_on_ok_and_rolls_back_on_err(pool: PgPool) {
    pool.execute("CREATE TABLE t (id int PRIMARY KEY)")
        .await
        .unwrap();
    let ours = infra_postgres::test_support::pool_from_sqlx(pool.clone());

    let inserted: Result<u64, AppError> = ours
        .in_tx(async |tx| {
            Ok(tx
                .execute("INSERT INTO t VALUES (1)")
                .await?
                .rows_affected())
        })
        .await;
    assert_eq!(inserted.unwrap(), 1);

    let failed: Result<(), AppError> = ours
        .in_tx(async |tx| {
            tx.execute("INSERT INTO t VALUES (2)").await?;
            Err(AppError::Business)
        })
        .await;
    std::assert_matches!(failed, Err(AppError::Business), "{failed:?}");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM t")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "the failed closure's insert was rolled back");
}

#[sqlx::test(migrations = false)]
async fn later_locked_work_consumes_the_deadline_and_restores_server_settings(pool: PgPool) {
    use tokio::time::Instant;

    const SETTINGS: &str = "SELECT pg_backend_pid(), setting::integer FROM pg_settings \
                           WHERE name = 'statement_timeout'";
    pool.execute("CREATE TABLE effects (id int PRIMARY KEY)")
        .await
        .unwrap();
    alter_database(&pool, "statement_timeout = '4s'").await;
    alter_database(&pool, "idle_in_transaction_session_timeout = '8s'").await;
    let writer = pool_with_server_budgets(&dsn_for(&pool).await, Isolation::ServerDefault)
        .await
        .unwrap();
    let session: (i32, i32) = sqlx::query_as(SETTINGS)
        .fetch_one(infra_postgres::test_support::as_sqlx(&writer))
        .await
        .unwrap();
    assert_eq!(session.1, 4000);
    let options = TxOptions::default();
    for (seconds, expected_ms) in [(10, 4000..=4000), (2, 1..=2000)] {
        let committed: Result<(i32, i32), AppError> = writer
            .in_tx_with_deadline(
                Instant::now() + Duration::from_secs(seconds),
                options,
                async |tx| Ok(sqlx::query_as(SETTINGS).fetch_one(tx).await?),
            )
            .await;
        let (pid, effective_ms) = committed.unwrap();
        assert_eq!(pid, session.0);
        assert!(
            expected_ms.contains(&effective_ms),
            "active limit was {effective_ms} ms"
        );
        let after_commit: (i32, i32) = sqlx::query_as(SETTINGS)
            .fetch_one(infra_postgres::test_support::as_sqlx(&writer))
            .await
            .unwrap();
        assert_eq!(session, after_commit);
    }

    let mut connection = pool.acquire().await.unwrap();
    let mut holder = connection.begin().await.unwrap();
    (&mut *holder)
        .execute("SELECT pg_advisory_xact_lock(1234567)")
        .await
        .unwrap();
    let cutoff = Instant::now() + Duration::from_secs(2);
    let attempt = writer.in_tx_with_deadline(cutoff, options, async |tx| {
        tx.execute("INSERT INTO effects VALUES (2)").await?;
        tokio::time::sleep_until(cutoff - Duration::from_millis(700)).await;
        let (pid, later): (i32, i32) = sqlx::query_as(SETTINGS).fetch_one(&mut *tx).await?;
        assert_eq!(pid, session.0);
        assert!((1..=700).contains(&later), "later statement got {later} ms");
        let cancelled = tx
            .execute("SELECT pg_advisory_xact_lock(1234567)")
            .await
            .unwrap_err();
        assert_eq!(sqlstate(&cancelled).as_deref(), Some("57014"));
        Err::<(), _>(AppError::Business)
    });
    let expired = tokio::time::timeout_at(cutoff + Duration::from_secs(1), attempt)
        .await
        .expect("PostgreSQL cancels the blocked statement while the lock is still held");
    std::assert_matches!(expired, Err(AppError::Deadline(_)));
    holder.rollback().await.unwrap();
    let restored: (i32, i32) = sqlx::query_as(SETTINGS)
        .fetch_one(infra_postgres::test_support::as_sqlx(&writer))
        .await
        .unwrap();
    assert_eq!(session, restored);
    let effects: Vec<i32> = sqlx::query_scalar("SELECT id FROM effects")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(effects, [] as [i32; 0]);
    infra_postgres::test_support::as_sqlx(&writer).close().await;
}

#[sqlx::test(migrations = false)]
async fn a_budgeted_attempt_rejects_multi_statement_sql_and_an_expired_commit(pool: PgPool) {
    use tokio::time::Instant;

    pool.execute("CREATE TABLE effects (id int)").await.unwrap();
    let ours = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let options = TxOptions::default();
    let multiple = ours
        .in_tx_with_deadline(
            Instant::now() + Duration::from_secs(5),
            options,
            async |tx| {
                tx.execute("INSERT INTO effects VALUES (1); INSERT INTO effects VALUES (2)")
                    .await?;
                Ok::<_, AppError>(())
            },
        )
        .await;
    std::assert_matches!(multiple, Err(AppError::Query(ref error)) if sqlstate(error).as_deref() == Some("42601"));
    let cutoff = Instant::now() + Duration::from_secs(1);
    let expired = ours
        .in_tx_with_deadline(cutoff, options, async |tx| {
            tx.execute("INSERT INTO effects VALUES (3)").await?;
            tokio::time::sleep_until(cutoff).await;
            Ok::<_, AppError>(())
        })
        .await;
    std::assert_matches!(expired, Err(AppError::Deadline(_)));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM effects")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = false)]
async fn a_finished_transaction_returns_its_connection_outside_any_transaction(pool: PgPool) {
    pool.execute("CREATE TABLE t (id int PRIMARY KEY)")
        .await
        .unwrap();
    let ours = template_pool(&dsn_for(&pool).await, 1).await;
    let serializable = TxOptions {
        isolation: Isolation::Serializable,
        read_only: false,
    };

    let committed: Result<(), AppError> = ours
        .in_tx(async |tx| {
            tx.execute("INSERT INTO t VALUES (1)").await?;
            Ok(())
        })
        .await;
    assert!(committed.is_ok(), "{committed:?}");
    let explicit: Result<(), AppError> = ours
        .in_tx_with(serializable, async |tx| {
            tx.execute("INSERT INTO t VALUES (2)").await?;
            Ok(())
        })
        .await;
    assert!(explicit.is_ok(), "{explicit:?}");

    let failed: Result<(), AppError> = ours
        .in_tx_with(serializable, async |tx| {
            tx.execute("INSERT INTO t VALUES (3)").await?;
            Err(AppError::Business)
        })
        .await;
    std::assert_matches!(failed, Err(AppError::Business), "{failed:?}");
    infra_postgres::test_support::as_sqlx(&ours)
        .execute("INSERT INTO t VALUES (4)")
        .await
        .unwrap();
    let ids: Vec<i32> = sqlx::query_scalar("SELECT id FROM t ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids, [1, 2, 4]);
    assert_eq!(ours.size(), 1, "no connection was discarded");
}

#[sqlx::test(migrations = false)]
async fn a_savepoint_inside_the_transaction_contains_an_expected_failure(pool: PgPool) {
    pool.execute("CREATE TABLE t (id int PRIMARY KEY)")
        .await
        .unwrap();
    let result: Result<(), AppError> = infra_postgres::test_support::pool_from_sqlx(pool.clone())
        .in_tx(async |tx| {
            tx.execute("INSERT INTO t VALUES (1)").await?;
            let mut savepoint = tx.connection().begin().await?;
            let duplicate = (&mut *savepoint).execute("INSERT INTO t VALUES (1)").await;
            assert!(duplicate.is_err());
            savepoint.rollback().await?;
            tx.execute("INSERT INTO t VALUES (2)").await?;
            Ok(())
        })
        .await;
    assert!(result.is_ok(), "{result:?}");
    let ids: Vec<i32> = sqlx::query_scalar("SELECT id FROM t ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids, [1, 2]);
}

#[sqlx::test(migrations = false)]
async fn a_commit_the_server_rejects_is_commit_failed(pool: PgPool) {
    pool.execute(
        "CREATE TABLE t (id int PRIMARY KEY, other int, \
         CONSTRAINT t_other UNIQUE (other) DEFERRABLE INITIALLY DEFERRED)",
    )
    .await
    .unwrap();
    let result: Result<(), AppError> = infra_postgres::test_support::pool_from_sqlx(pool.clone())
        .in_tx(async |tx| {
            tx.execute("INSERT INTO t VALUES (1, 1), (2, 1)").await?;
            Ok(())
        })
        .await;
    match result {
        Err(AppError::Tx(TxError::CommitFailed(err))) => {
            assert_eq!(err.as_database_error().unwrap().code().unwrap(), "23505");
        }
        other => panic!("expected CommitFailed, got {other:?}"),
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM t")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = false)]
async fn a_swallowed_statement_failure_is_commit_failed_not_success(pool: PgPool) {
    pool.execute("CREATE TABLE t (id int PRIMARY KEY)")
        .await
        .unwrap();
    let result: Result<(), AppError> = infra_postgres::test_support::pool_from_sqlx(pool.clone())
        .in_tx(async |tx| {
            tx.execute("INSERT INTO t VALUES (1)").await?;
            let _ = tx.execute("INSERT INTO t VALUES (1)").await;
            Ok(())
        })
        .await;
    match result {
        Err(AppError::Tx(TxError::CommitFailed(err))) => {
            assert_eq!(err.as_database_error().unwrap().code().unwrap(), "25P02");
        }
        other => panic!("expected CommitFailed, got {other:?}"),
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM t")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = false)]
async fn a_failure_on_the_borrowed_connection_is_found_before_the_commit(pool: PgPool) {
    pool.execute("CREATE TABLE t (id int PRIMARY KEY)")
        .await
        .unwrap();
    let ours = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let result: Result<(), AppError> = ours
        .in_tx(async |tx| {
            tx.execute("INSERT INTO t VALUES (1)").await?;
            let _ = tx.connection().execute("INSERT INTO t VALUES (1)").await;
            Ok(())
        })
        .await;
    match result {
        Err(AppError::Tx(TxError::CommitFailed(err))) => {
            assert_eq!(sqlstate(&err).as_deref(), Some("25P02"));
        }
        other => panic!("expected CommitFailed from the borrowed connection, got {other:?}"),
    }

    let borrowed: Result<(), AppError> = ours
        .in_tx(async |tx| {
            tx.connection().execute("INSERT INTO t VALUES (2)").await?;
            Ok(())
        })
        .await;
    assert!(borrowed.is_ok(), "{borrowed:?}");
    let through_the_handle: Result<(), AppError> = ours
        .in_tx(async |tx| {
            tx.connection().execute("INSERT INTO t VALUES (3)").await?;
            tx.execute("INSERT INTO t VALUES (4)").await?;
            Ok(())
        })
        .await;
    assert!(through_the_handle.is_ok(), "{through_the_handle:?}");
    let empty: Result<(), AppError> = ours.in_tx(async |_tx| Ok(())).await;
    assert!(empty.is_ok(), "{empty:?}");
    let ids: Vec<i32> = sqlx::query_scalar("SELECT id FROM t ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids, [2, 3, 4]);
}

#[sqlx::test(migrations = false)]
async fn a_serialization_failure_reaches_the_caller_with_its_sqlstate(pool: PgPool) {
    pool.execute("CREATE TABLE counters (id int PRIMARY KEY, n int NOT NULL)")
        .await
        .unwrap();
    pool.execute("INSERT INTO counters VALUES (1, 0)")
        .await
        .unwrap();

    let ours = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let other = ours.clone();
    let result: Result<(), AppError> = ours
        .in_tx_with(
            TxOptions {
                isolation: Isolation::Serializable,
                read_only: false,
            },
            async |tx| {
                let seen: i32 = sqlx::query_scalar("SELECT n FROM counters WHERE id = 1")
                    .fetch_one(&mut *tx)
                    .await?;
                let concurrent: Result<(), AppError> = other
                    .in_tx_with(
                        TxOptions {
                            isolation: Isolation::Serializable,
                            read_only: false,
                        },
                        async |tx| {
                            tx.execute("UPDATE counters SET n = n + 1 WHERE id = 1")
                                .await?;
                            Ok(())
                        },
                    )
                    .await;
                concurrent.expect("the concurrent writer commits first");
                sqlx::query("UPDATE counters SET n = $1 WHERE id = 1")
                    .bind(seen + 1)
                    .execute(&mut *tx)
                    .await?;
                Ok(())
            },
        )
        .await;
    match result {
        Err(AppError::Query(err)) => {
            assert_eq!(sqlstate(&err).as_deref(), Some("40001"), "{err}");
        }
        other => panic!("expected a serialization failure, got {other:?}"),
    }
}

#[sqlx::test(migrations = false)]
async fn every_transaction_option_reaches_the_server(pool: PgPool) {
    let ours = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    for (isolation, read_only, expected) in [
        (Isolation::ServerDefault, false, ("read committed", "off")),
        (Isolation::ServerDefault, true, ("read committed", "on")),
        (Isolation::ReadCommitted, false, ("read committed", "off")),
        (Isolation::ReadCommitted, true, ("read committed", "on")),
        (Isolation::RepeatableRead, false, ("repeatable read", "off")),
        (Isolation::RepeatableRead, true, ("repeatable read", "on")),
        (Isolation::Serializable, false, ("serializable", "off")),
        (Isolation::Serializable, true, ("serializable", "on")),
    ] {
        let observed: Result<(String, String), AppError> = ours
            .in_tx_with(
                TxOptions {
                    isolation,
                    read_only,
                },
                async |tx| {
                    let level = sqlx::query_scalar("SHOW transaction_isolation")
                        .fetch_one(&mut *tx)
                        .await?;
                    let read_only = sqlx::query_scalar("SHOW transaction_read_only")
                        .fetch_one(&mut *tx)
                        .await?;
                    Ok((level, read_only))
                },
            )
            .await;
        let observed = observed.unwrap();
        assert_eq!(
            (observed.0.as_str(), observed.1.as_str()),
            expected,
            "{isolation:?} read_only={read_only}"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn a_read_only_transaction_refuses_writes(pool: PgPool) {
    pool.execute("CREATE TABLE t (id int)").await.unwrap();
    let result: Result<(), AppError> = infra_postgres::test_support::pool_from_sqlx(pool.clone())
        .in_tx_with(
            TxOptions {
                isolation: Isolation::ReadCommitted,
                read_only: true,
            },
            async |tx| {
                tx.execute("INSERT INTO t VALUES (1)").await?;
                Ok(())
            },
        )
        .await;
    match result {
        Err(AppError::Query(err)) => {
            assert_eq!(err.as_database_error().unwrap().code().unwrap(), "25006");
        }
        other => panic!("expected read_only_sql_transaction, got {other:?}"),
    }
}

#[sqlx::test(migrations = false)]
async fn cancelled_begin_discards_its_connection_before_later_autocommit_and_options(pool: PgPool) {
    pool.execute("CREATE TABLE pending_begin_visibility (id int PRIMARY KEY)")
        .await
        .unwrap();
    let (proxy, proxied) = proxied_pool(&pool, 1).await;
    proxy.arm_ready_hold(ReadyBoundary::Begin);
    let mut pending = Box::pin(proxied.in_tx_with(
        TxOptions {
            isolation: Isolation::RepeatableRead,
            read_only: false,
        },
        async |_tx| -> Result<(), AppError> { Ok(()) },
    ));
    tokio::select! {
        outcome = &mut pending => panic!("BEGIN completed before its acknowledgement was held: {outcome:?}"),
        () = proxy.ready_held() => {}
    }
    drop(pending);
    wait_for_slot_reclamation(&proxied).await;

    sqlx::query("INSERT INTO pending_begin_visibility VALUES (1)")
        .execute(infra_postgres::test_support::as_sqlx(&proxied))
        .await
        .unwrap();
    let committed: i64 = sqlx::query_scalar("SELECT count(*) FROM pending_begin_visibility")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        committed, 1,
        "the statement after a cancelled BEGIN must run in ordinary autocommit"
    );
    proxy.release_ready();

    let session_before: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(infra_postgres::test_support::as_sqlx(&proxied))
        .await
        .unwrap();
    let options: Result<(String, String), AppError> = proxied
        .in_tx_with(
            TxOptions {
                isolation: Isolation::Serializable,
                read_only: true,
            },
            async |tx| {
                let isolation = sqlx::query_scalar("SHOW transaction_isolation")
                    .fetch_one(&mut *tx)
                    .await?;
                let read_only = sqlx::query_scalar("SHOW transaction_read_only")
                    .fetch_one(&mut *tx)
                    .await?;
                Ok((isolation, read_only))
            },
        )
        .await;
    assert_eq!(
        options.unwrap(),
        ("serializable".to_owned(), "on".to_owned())
    );
    let session_after: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(infra_postgres::test_support::as_sqlx(&proxied))
        .await
        .unwrap();
    assert_eq!(
        session_before, session_after,
        "completed transactions retain normal pool reuse"
    );
    assert_eq!(
        proxied.close(Duration::from_secs(5)).await,
        infra_postgres::CloseOutcome::Complete
    );
    proxy.shutdown().await;
}

#[sqlx::test(migrations = false)]
async fn stalled_or_failed_return_preserves_the_operation_result_and_releases_capacity(
    pool: PgPool,
) {
    pool.execute("CREATE TABLE release_effects (id int PRIMARY KEY)")
        .await
        .unwrap();
    for (id, commit, fail_return) in [(1, true, false), (2, false, false), (3, true, true)] {
        let (proxy, ours) = proxied_pool(&pool, 1).await;
        let result = ours
            .in_tx(async |tx| -> Result<i32, AppError> {
                sqlx::query("INSERT INTO release_effects VALUES ($1)")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                proxy.arm_ready_hold(ReadyBoundary::Sync);
                if commit {
                    Ok(id)
                } else {
                    Err(AppError::Business)
                }
            })
            .await;
        proxy.ready_held().await;
        assert_eq!(ours.size(), 1, "cleanup still owns the connection");
        if fail_return {
            proxy.cancel();
        }
        wait_for_slot_reclamation(&ours).await;
        if !fail_return {
            proxy.release_ready();
        }
        if commit {
            std::assert_matches!(result, Ok(value) if value == id, "{result:?}");
        } else {
            std::assert_matches!(result, Err(AppError::Business), "{result:?}");
        }
        assert_eq!(
            ours.size(),
            0,
            "failed return cannot retain the local permit"
        );
        let written: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM release_effects WHERE id = $1)")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(written, commit, "cleanup does not change transaction truth");
        infra_postgres::test_support::as_sqlx(&ours).close().await;
        proxy.shutdown().await;
    }
}

#[sqlx::test(migrations = false)]
async fn migrations_apply_once_and_then_report_no_change(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let widgets = fixture("widgets").await;

    let first = migrate::run(&widgets, &options(&dsn)).await.unwrap();
    assert_eq!(first.before, None);
    assert_eq!(first.applied, vec![20_260_918_000_001, 20_260_918_000_002]);
    assert_eq!(first.after(), Some(20_260_918_000_002));
    assert_eq!(applied_count(&pool).await, 2);
    pool.execute("INSERT INTO widgets (id, name, sku) VALUES (1, 'w', 's')")
        .await
        .unwrap();

    let second = migrate::run(&widgets, &options(&dsn)).await.unwrap();
    assert_eq!(second.before, Some(20_260_918_000_002));
    assert_eq!(second.applied, [] as [i64; 0]);
    assert_eq!(second.after(), Some(20_260_918_000_002));
}

#[sqlx::test(migrations = false)]
async fn the_embedded_set_runs_on_an_empty_database(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let target = MIGRATOR.iter().map(|migration| migration.version).max();
    let result = migrate::run(&MIGRATOR, &options(&dsn)).await.unwrap();
    assert_eq!(result.applied.len(), MIGRATOR.iter().count());
    assert_eq!(result.after(), target);
    assert_eq!(
        applied_count(&pool).await,
        i64::try_from(result.applied.len()).unwrap()
    );

    let repeated = migrate::run(&MIGRATOR, &options(&dsn)).await.unwrap();
    assert_eq!(repeated.before, target);
    assert_eq!(repeated.applied, [] as [i64; 0]);
    let checked = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    assert_eq!(migrate::verify_history(&checked).await, Ok(()));

    let newer = target.unwrap_or(0) + 1;
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES ($1, 'later release', true, '\\x00'::bytea, 0)",
    )
    .bind(newer)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(migrate::verify_history(&checked).await, Ok(()));
    let rolled_back = migrate::run(&MIGRATOR, &options(&dsn)).await.unwrap();
    assert_eq!(rolled_back.applied, [] as [i64; 0]);
    assert_eq!(rolled_back.before, Some(newer));
}

#[sqlx::test(migrations = false)]
async fn history_admission_refuses_missing_bookkeeping_without_creating_it(pool: PgPool) {
    assert_eq!(
        migrate::verify_history(&infra_postgres::test_support::pool_from_sqlx(pool.clone())).await,
        Err(HistoryError::Pending)
    );
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        !exists,
        "history admission must not create migration bookkeeping"
    );
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn history_refusals_identify_permissions_deadlines_and_divergent_versions(pool: PgPool) {
    let events = PoolEvents::default();
    let subscriber = events.subscriber_for(&["migrate::history"]);
    let restricted = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(
            pool.connect_options()
                .as_ref()
                .clone()
                .options([("role", "pg_read_all_settings")]),
        )
        .await
        .unwrap();
    assert_eq!(
        migrate::verify_history(&infra_postgres::test_support::pool_from_sqlx(
            restricted.clone()
        ))
        .with_subscriber(subscriber.clone())
        .await,
        Err(HistoryError::Unavailable)
    );
    restricted.close().await;

    let mut lock = sqlx::PgConnection::connect_with(&pool.connect_options())
        .await
        .unwrap();
    let mut tx = lock.begin().await.unwrap();
    tx.execute("LOCK TABLE _sqlx_migrations IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let checked = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    assert_eq!(
        migrate::verify_history(&checked)
            .with_subscriber(subscriber.clone())
            .await,
        Err(HistoryError::Unavailable)
    );
    tx.rollback().await.unwrap();

    let captured = events.take();
    let [permission, deadline] = captured.as_slice() else {
        panic!("unexpected history diagnostics: {captured:?}");
    };
    for event in [permission, deadline] {
        assert_eq!(event["message"], "postgres_history_refused");
        assert_eq!(event["stage"], "\"read\"");
    }
    assert_eq!(permission["cause"], "\"database\"");
    assert_eq!(permission["sqlstate"], "\"42501\"");
    assert_eq!(permission.len(), 4);
    assert_eq!(deadline["cause"], "\"deadline\"");
    assert_eq!(deadline.len(), 3);

    if let Some(first) = MIGRATOR.iter().next() {
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ''::bytea WHERE version = $1")
            .bind(first.version)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            migrate::verify_history(&checked)
                .with_subscriber(subscriber)
                .await,
            Err(HistoryError::Mismatch)
        );
        let captured = events.take();
        let [event] = captured.as_slice() else {
            panic!("unexpected history diagnostics: {captured:?}");
        };
        assert_eq!(event["message"], "postgres_history_refused");
        assert_eq!(event["stage"], "\"compare\"");
        assert_eq!(event["cause"], "\"checksum_mismatch\"");
        assert_eq!(event["migration.version"], first.version.to_string());
        assert_eq!(event.len(), 4);
    }
}

#[sqlx::test(migrations = false)]
async fn history_admission_reports_a_table_outside_search_path(pool: PgPool) {
    pool.execute("CREATE SCHEMA hidden_history").await.unwrap();
    pool.execute(
        "CREATE TABLE hidden_history._sqlx_migrations (\
         version BIGINT PRIMARY KEY, checksum BYTEA NOT NULL)",
    )
    .await
    .unwrap();
    let url = url_for(&pool, DATABASE_URL).await;
    let hidden = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET search_path = public")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect(url.as_str())
        .await
        .unwrap();
    assert_eq!(
        migrate::verify_history(&infra_postgres::test_support::pool_from_sqlx(hidden)).await,
        Err(HistoryError::NotVisible)
    );
}

#[sqlx::test(migrations = false)]
async fn an_edited_applied_migration_fails_in_the_history_stage(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    migrate::run(&fixture("widgets").await, &options(&dsn))
        .await
        .unwrap();

    let err = migrate::run(&fixture("widgets_edited").await, &options(&dsn))
        .await
        .unwrap_err();
    assert_eq!(err.stage(), "history");
    std::assert_matches!(
        &err.error,
        RunError::Migrate(MigrateError::VersionMismatch(20_260_918_000_001)),
        "{err}"
    );
    assert_eq!(applied_count(&pool).await, 2, "nothing was re-applied");
}

#[sqlx::test(migrations = false)]
async fn an_older_release_admits_the_history_of_a_later_one(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    migrate::run(&fixture("widgets").await, &options(&dsn))
        .await
        .unwrap();

    let result = migrate::run(&fixture("widgets_partial").await, &options(&dsn))
        .await
        .unwrap();
    assert_eq!(result.applied, [] as [i64; 0]);
    assert_eq!(result.before, Some(20_260_918_000_002));
    assert_eq!(applied_count(&pool).await, 2);
}

#[sqlx::test(migrations = false)]
async fn a_held_session_lock_fails_in_the_lock_stage(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let mut holder = pool.acquire().await.unwrap();
    holder.lock().await.unwrap();

    let mut options = options(&dsn);
    options.lock_timeout = Duration::from_millis(500);
    let started = std::time::Instant::now();
    let err = migrate::run(&fixture("widgets").await, &options)
        .await
        .unwrap_err();
    assert_eq!(err.stage(), "lock", "{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
    holder.unlock().await.unwrap();

    migrate::run(&fixture("widgets").await, &options)
        .await
        .expect("the run succeeds once the lock is released");
}

#[sqlx::test(migrations = false)]
async fn the_deadline_drops_the_session_and_leaves_no_partial_history(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let mut options = options(&dsn);
    options.deadline = Duration::from_millis(700);
    let started = std::time::Instant::now();
    let err = migrate::run(&fixture("slow").await, &options)
        .await
        .unwrap_err();
    assert_eq!(err.stage(), "deadline", "{err}");
    assert!(started.elapsed() < Duration::from_secs(3));

    assert_eq!(applied_count(&pool).await, 0);
    let mut conn = pool.acquire().await.unwrap();
    conn.lock()
        .await
        .expect("the dropped session released the lock");
    conn.unlock().await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn the_deadline_reports_the_migration_that_already_applied(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let mut options = options(&dsn);
    options.deadline = Duration::from_secs(1);
    let err = migrate::run(&fixture("partial_deadline").await, &options)
        .await
        .unwrap_err();
    assert_eq!(err.stage(), "deadline", "{err}");
    assert_eq!(err.report.before, None);
    assert_eq!(err.report.applied, vec![20_260_918_000_001]);
    assert_eq!(applied_count(&pool).await, 1);
}

#[sqlx::test(migrations = false)]
async fn a_no_transaction_build_is_refused_until_its_invalid_index_is_dropped(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let fixture = fixture("concurrent_index").await;

    let err = migrate::run(&fixture, &options(&dsn)).await.unwrap_err();
    std::assert_matches!(
        &err.error,
        RunError::Migrate(MigrateError::ExecuteMigration(_, 20_260_918_000_002)),
        "{err}"
    );
    assert_eq!(applied_count(&pool).await, 1);

    let err = migrate::run(&fixture, &options(&dsn)).await.unwrap_err();
    assert_eq!(err.stage(), "execute", "{err}");
    std::assert_matches!(
            &err.error,
            RunError::InvalidIndexes { version: 20_260_918_000_002, indexes } if indexes == "widgets_sku"
        ,
        "{err}"
    );
    assert_eq!(applied_count(&pool).await, 1);

    pool.execute("DROP INDEX CONCURRENTLY widgets_sku")
        .await
        .unwrap();
    pool.execute("DELETE FROM widgets WHERE id = 2")
        .await
        .unwrap();

    let mut writer = pool.acquire().await.unwrap();
    writer.execute("BEGIN").await.unwrap();
    writer
        .execute("INSERT INTO widgets (id, sku) VALUES (3, 'c')")
        .await
        .unwrap();
    let commit = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        writer.execute("COMMIT").await.unwrap();
    });
    let mut waiting = options(&dsn);
    waiting.lock_timeout = Duration::from_millis(300);
    let started = Instant::now();
    let report = migrate::run(&fixture, &waiting).await.unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    commit.await.unwrap();
    assert_eq!(report.applied, vec![20_260_918_000_002]);
    let valid: bool = sqlx::query_scalar(
        "SELECT indisvalid FROM pg_index WHERE indexrelid = 'widgets_sku'::regclass",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(valid);
    let replay = migrate::run(&fixture, &options(&dsn)).await.unwrap();
    assert_eq!(replay.applied, [] as [i64; 0]);
}

#[sqlx::test(migrations = false)]
async fn a_no_transaction_migration_is_bounded_by_the_deadline_not_the_statement_budget(
    pool: PgPool,
) {
    let dsn = dsn_for(&pool).await;
    let mut options = options(&dsn);
    options.statement_timeout = Duration::from_millis(500);
    options.deadline = Duration::from_secs(20);
    let err = migrate::run(&fixture("no_transaction_budget").await, &options)
        .await
        .unwrap_err();

    let RunError::Migrate(MigrateError::ExecuteMigration(cause, 20_260_918_000_002)) = &err.error
    else {
        panic!("{err}");
    };
    assert_eq!(sqlstate(cause).as_deref(), Some("57014"), "{err}");
    assert_eq!(err.report.before, None);
    assert_eq!(err.report.applied, vec![20_260_918_000_001]);
    let applied: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(applied, vec![20_260_918_000_001]);
}

#[sqlx::test(migrations = false)]
async fn a_no_transaction_restore_keeps_a_nonzero_lock_budget(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let mut holder = pool.acquire().await.unwrap();
    holder
        .execute("CREATE TABLE widgets (id integer PRIMARY KEY)")
        .await
        .unwrap();
    holder.execute("BEGIN").await.unwrap();
    holder
        .execute("LOCK TABLE widgets IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let mut options = options(&dsn);
    options.statement_timeout = Duration::from_secs(5);
    options.lock_timeout = Duration::from_micros(500);
    options.deadline = Duration::from_secs(2);
    let started = Instant::now();
    let err = migrate::run(&fixture("no_transaction_lock_budget").await, &options)
        .await
        .unwrap_err();
    assert_eq!(err.stage(), "execute", "{err}");
    assert!(started.elapsed() < Duration::from_secs(1), "{err}");
    assert_eq!(err.report.applied, vec![20_260_918_000_001]);
    holder.execute("ROLLBACK").await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn a_migration_that_fails_is_the_execute_stage(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    pool.execute("CREATE TABLE widgets (id int)").await.unwrap();
    let err = migrate::run(&fixture("widgets").await, &options(&dsn))
        .await
        .unwrap_err();
    assert_eq!(err.stage(), "execute", "{err}");
    std::assert_matches!(
        &err.error,
        RunError::Migrate(MigrateError::ExecuteMigration(_, 20_260_918_000_001)),
        "{err}"
    );
    assert_eq!(applied_count(&pool).await, 0);
}

struct PostgresTlsRelay {
    cancel: CancellationToken,
    _cancel_guard: tokio_util::sync::DropGuard,
    task: tokio_util::task::AbortOnDropHandle<()>,
    server_names: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

struct PostgresTlsMaterial {
    root: String,
    acceptor: tokio_rustls::TlsAcceptor,
}

impl PostgresTlsMaterial {
    fn new() -> Self {
        let material = tls_test_support::TlsMaterial::new("localhost");
        let acceptor = material.acceptor();
        Self {
            root: material.root_pem,
            acceptor,
        }
    }
}

impl PostgresTlsRelay {
    fn start(
        listener: tokio::net::TcpListener,
        target: SocketAddr,
        acceptor: tokio_rustls::TlsAcceptor,
    ) -> Self {
        let cancel = CancellationToken::new();
        let cancel_guard = cancel.clone().drop_guard();
        let server_names = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let run_cancel = cancel.clone();
        let run_names = server_names.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    () = run_cancel.cancelled() => break,
                    Some(result) = connections.join_next(), if !connections.is_empty() => {
                        propagate_join_panic(result);
                    }
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.unwrap();
                        let acceptor = acceptor.clone();
                        let connection_cancel = run_cancel.child_token();
                        let names = run_names.clone();
                        connections.spawn(async move {
                            let _ = connection_cancel
                                .run_until_cancelled(Self::forward(socket, target, acceptor, names))
                                .await;
                        });
                    }
                }
            }
            while let Some(result) = connections.join_next().await {
                propagate_join_panic(result);
            }
        });
        Self {
            cancel,
            _cancel_guard: cancel_guard,
            task: tokio_util::task::AbortOnDropHandle::new(task),
            server_names,
        }
    }

    async fn forward(
        mut socket: tokio::net::TcpStream,
        target: SocketAddr,
        acceptor: tokio_rustls::TlsAcceptor,
        names: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> std::io::Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut ssl_request = [0; 8];
        socket.read_exact(&mut ssl_request).await?;
        assert_eq!(ssl_request, [0, 0, 0, 8, 4, 210, 22, 47]);
        socket.write_all(b"S").await?;
        let Ok(mut tls) = acceptor.accept(socket).await else {
            return Ok(());
        };
        names
            .lock()
            .unwrap()
            .push(tls.get_ref().1.server_name().unwrap().to_owned());
        let mut backend = tokio::net::TcpStream::connect(target).await?;
        tokio::io::copy_bidirectional(&mut tls, &mut backend).await?;
        Ok(())
    }

    async fn stop(mut self) {
        self.cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(3), &mut self.task).await;
        if result.is_err() {
            self.task.abort();
            let _ = (&mut self.task).await;
        }
        propagate_join_panic(result.expect("all TLS relay tasks terminate"));
        assert!(
            self.server_names
                .lock()
                .unwrap()
                .iter()
                .all(|name| name == "localhost")
        );
    }
}

#[sqlx::test(migrations = false)]
async fn same_pool_redials_replacement_ip_and_rereads_tls_roots(pool: PgPool) {
    use tokio_rustls::rustls::CertificateError;

    fn is_certificate_error(error: &sqlx::Error, expected: &CertificateError) -> bool {
        use std::error::Error;
        use tokio_rustls::rustls::Error as TlsError;

        let mut cause: &(dyn Error + 'static) = match error {
            sqlx::Error::Tls(cause) => cause.as_ref(),
            sqlx::Error::Io(cause) if cause.kind() == std::io::ErrorKind::InvalidData => {
                let Some(cause) = cause.get_ref() else {
                    return false;
                };
                cause
            }
            _ => return false,
        };
        loop {
            if matches!(
                cause.downcast_ref::<TlsError>(),
                Some(TlsError::InvalidCertificate(actual)) if actual == expected
            ) {
                return true;
            }
            let Some(source) = cause.source() else {
                return false;
            };
            cause = source;
        }
    }

    let target = server_address(&dsn_for(&pool).await).await;
    let first = PostgresTlsMaterial::new();
    let second = PostgresTlsMaterial::new();
    let dir = tempfile::tempdir().unwrap();
    let root_file = dir.path().join("root.pem");
    tokio::fs::write(&root_file, &first.root).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let localhost_addresses: std::collections::BTreeSet<_> =
        tokio::net::lookup_host(("localhost", port))
            .await
            .expect("localhost resolution is available")
            .map(|address| address.ip())
            .collect();
    assert!(
        localhost_addresses.contains(&std::net::Ipv4Addr::LOCALHOST.into())
            && localhost_addresses.contains(&std::net::Ipv6Addr::LOCALHOST.into()),
        "the fixture requires localhost to resolve to both loopbacks: {localhost_addresses:?}"
    );
    let first_relay = PostgresTlsRelay::start(listener, target, first.acceptor);
    let mut url = url_for(&pool, DATABASE_URL).await;
    url.set_host(Some("localhost")).unwrap();
    url.set_port(Some(port)).unwrap();
    url.query_pairs_mut()
        .clear()
        .append_pair("sslmode", "verify-full")
        .append_pair("sslrootcert", root_file.to_str().unwrap());
    let dsn = Dsn::admit(url.as_str()).unwrap();
    assert_eq!(dsn.host(), "localhost");
    let ours = template_pool(&dsn, 1).await;
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
        .await
        .unwrap();
    wait_for_idle(&ours).await;

    tokio::fs::write(&root_file, "invalid replacement")
        .await
        .unwrap();
    let retained_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
        .await
        .unwrap();
    assert_eq!(retained_pid, first_pid);
    infra_postgres::test_support::as_sqlx(&ours)
        .acquire()
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let invalid = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
        .await;
    assert!(
        invalid
            .as_ref()
            .is_err_and(|error| is_certificate_error(error, &CertificateError::UnknownIssuer)),
        "{invalid:?}"
    );
    first_relay.stop().await;

    let listener = tokio::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port))
        .await
        .unwrap();
    let replacement = PostgresTlsRelay::start(listener, target, second.acceptor);
    tokio::fs::write(&root_file, &first.root).await.unwrap();
    let untrusted = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
        .await;
    assert!(
        untrusted
            .as_ref()
            .is_err_and(|error| is_certificate_error(error, &CertificateError::BadSignature)),
        "{untrusted:?}"
    );
    tokio::fs::write(&root_file, &second.root).await.unwrap();
    let replacement_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(infra_postgres::test_support::as_sqlx(&ours))
        .await
        .unwrap();
    assert_ne!(replacement_pid, first_pid);
    assert_eq!(
        infra_postgres::test_support::as_sqlx(&ours)
            .options()
            .get_max_connections(),
        1
    );
    assert!(PostgresProbe::new(ours.clone()).check().await.is_ok());
    assert!(!replacement.server_names.lock().unwrap().is_empty());
    infra_postgres::test_support::as_sqlx(&ours).close().await;
    replacement.stop().await;
}
