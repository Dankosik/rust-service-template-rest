//! Database-backed proof for `infra-postgres` and `migrate`.
//!
//! Every test gets its own database from `#[sqlx::test]`; the template's
//! pool is opened on top of it through the admitted DSN so the session
//! defaults, the probe, the transaction seam, and the migration runner are
//! observed exactly as the service and the `migrate` binary use them.

#![cfg(feature = "integration")]
// Integration tests are test code; the workspace's production lint levels
// for unwrap/expect/panic do not apply to them.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

#[allow(
    dead_code,
    reason = "the shared transport also supplies commit-ack faults to the jobs integration target"
)]
#[path = "support/commit_proxy.rs"]
mod commit_proxy;

#[path = "postgres/operational_recovery.rs"]
mod operational_recovery;

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use commit_proxy::{CommitProxy, Fault, ReadyBoundary};
use health::Probe;
use infra_postgres::{
    ACQUIRE_TIMEOUT, ConnectError, Dsn, Isolation, PASSWORD_REFRESH_INTERVAL, PgPool, PoolOptions,
    PostgresProbe, SessionBudgets, TxError, TxOptions, connection, in_tx, in_tx_with,
    refresh_password_periodically, sqlstate,
};
use integration_tests::{DATABASE_URL, dsn_for, fixture_dir, pooler_dsn_for, url_for};
use migrate::{HistoryError, MIGRATOR, RunError, RunOptions};
use sqlx::migrate::{Migrate, MigrateError, Migrator};
use sqlx::{AssertSqlSafe, Connection, Executor};
use tokio_util::sync::CancellationToken;
use tracing::instrument::WithSubscriber;

const APP: &str = "integration-tests";

type EventFields = std::collections::BTreeMap<String, String>;

/// Captures only the two owners whose public diagnostic contract is under test.
#[derive(Clone, Default)]
struct PoolEvents(std::sync::Arc<std::sync::Mutex<Vec<EventFields>>>);

impl PoolEvents {
    fn take(&self) -> Vec<EventFields> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl tracing::Subscriber for PoolEvents {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        matches!(metadata.target(), "infra_postgres::observe" | "health")
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
        let mut fields = Fields(EventFields::new());
        event.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

async fn template_pool(dsn: &Dsn, max_connections: u32) -> PgPool {
    infra_postgres::connect(
        dsn,
        &PoolOptions {
            max_connections: NonZeroU32::new(max_connections).expect("pool size in tests"),
            application_name: APP,
            default_isolation: Isolation::ServerDefault,
            session_budgets: SessionBudgets::Startup,
        },
    )
    .await
    .expect("pool connects")
}

// The real database remains in the existing harness. Only its local TCP
// entry point changes, so this observes admitted IPv6 through pool startup,
// authentication, session verification and an actual query.
#[sqlx::test(migrations = "../migrations")]
async fn ipv6_literal_reaches_the_admitted_database(pool: PgPool) {
    let mut url = url_for(&pool, DATABASE_URL).await;
    let target = (url.host_str().unwrap().to_owned(), url.port().unwrap());
    let listener = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
    url.set_host(Some("[::1]")).unwrap();
    url.set_port(Some(listener.local_addr().unwrap().port()))
        .unwrap();
    let dsn = Dsn::admit(url.as_str()).unwrap();
    let work = async {
        let connected = template_pool(&dsn, 1).await;
        let result = sqlx::query_scalar::<_, String>("SELECT current_database()")
            .fetch_one(&connected)
            .await;
        connected.close().await;
        result
    };
    let relay = async {
        let (mut incoming, _) = listener.accept().await.unwrap();
        let mut outgoing = tokio::net::TcpStream::connect(target).await.unwrap();
        tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await
    };
    // Both futures are owned by this one bounded wait, including on failure.
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
) -> PgPool {
    infra_postgres::connect(
        dsn,
        &PoolOptions {
            max_connections: NonZeroU32::new(max_connections).expect("pool size in tests"),
            application_name: APP,
            default_isolation,
            session_budgets: SessionBudgets::Startup,
        },
    )
    .await
    .expect("pool connects")
}

/// A pool that publishes nothing and expects the server to carry the
/// budgets and, when one is named, the default isolation.
async fn pool_with_server_budgets(
    dsn: &Dsn,
    default_isolation: Isolation,
) -> Result<PgPool, ConnectError> {
    infra_postgres::connect(
        dsn,
        &PoolOptions {
            max_connections: NonZeroU32::MIN,
            application_name: APP,
            default_isolation,
            session_budgets: SessionBudgets::Server,
        },
    )
    .await
}

/// Set a default on the per-test database; sessions opened later carry it.
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

/// The per-test database of `pool`, reached at `address` instead.
async fn dsn_at(pool: &PgPool, address: SocketAddr) -> Dsn {
    let mut url = url_for(pool, DATABASE_URL).await;
    url.set_ip_host(address.ip())
        .expect("the relay address is a host");
    url.set_port(Some(address.port()))
        .expect("the relay URL accepts a port");
    Dsn::admit(url.as_str()).expect("the relayed DSN is admitted")
}

async fn proxied_pool(pool: &PgPool, max_connections: u32) -> (CommitProxy, PgPool) {
    let dsn = dsn_for(pool).await;
    assert_eq!(
        dsn.ssl_mode_name(),
        "disable",
        "the test proxy frames the plaintext protocol"
    );
    let proxy = CommitProxy::start(server_address(&dsn).await).await;
    let proxied = dsn_at(pool, proxy.address()).await;
    (proxy, template_pool(&proxied, max_connections).await)
}

/// Scheduling allowance around the dependency's five-second cleanup budget.
const RETURN_OBSERVATION_BUDGET: Duration = Duration::from_secs(7);

async fn wait_for_idle(pool: &PgPool) {
    tokio::time::timeout(RETURN_OBSERVATION_BUDGET, async {
        while pool.num_idle() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a healthy return reaches the idle queue");
}

/// Observe local capacity before attempting replacement work. A three-second
/// acquire may legitimately time out during the five-second native cleanup.
async fn wait_for_slot_reclamation(pool: &PgPool) {
    tokio::time::timeout(RETURN_OBSERVATION_BUDGET, async {
        while pool.size() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the silent connection releases its local pool slot without a reply");
}

async fn show(pool: &PgPool, setting: &'static str) -> String {
    let sql = match setting {
        "statement_timeout" => "SHOW statement_timeout",
        "idle_in_transaction_session_timeout" => "SHOW idle_in_transaction_session_timeout",
        "application_name" => "SHOW application_name",
        "default_transaction_isolation" => "SHOW default_transaction_isolation",
        other => panic!("unexpected setting {other}"),
    };
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

async fn fixture(name: &str) -> Migrator {
    Migrator::new(fixture_dir(name).as_path())
        .await
        .expect("fixture resolves")
}

fn options(dsn: &Dsn) -> RunOptions<'_> {
    RunOptions::defaults(dsn, APP, Duration::from_secs(300))
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
    Query(sqlx::Error),
    #[from(skip)]
    Business,
}

#[sqlx::test(migrations = false)]
async fn silent_session_readback_rejects_admission_before_the_relay_is_released(pool: PgPool) {
    let direct = dsn_for(&pool).await;
    assert_eq!(direct.ssl_mode_name(), "disable");
    for session_budgets in [SessionBudgets::Startup, SessionBudgets::Server] {
        let proxy = CommitProxy::start(server_address(&direct).await).await;
        let dsn = dsn_at(&pool, proxy.address()).await;
        proxy.arm_autocommit(Fault::ForwardThenSilence, "pg_settings");
        let options = PoolOptions {
            max_connections: NonZeroU32::MIN,
            application_name: APP,
            default_isolation: Isolation::ServerDefault,
            session_budgets,
        };
        let started = Instant::now();
        let mut admission =
            tokio::spawn(async move { infra_postgres::connect(&dsn, &options).await });
        proxy.silenced().await;
        // Keep both sockets silent through the observed admission result. Fixture
        // shutdown must not supply the response/EOF that lets admission finish.
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
        assert!(
            matches!(error, ConnectError::SessionVerificationTimeout { budget } if budget == Duration::from_secs(5)),
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
        let dsn = dsn_for(&pool).await;
        let proxy = CommitProxy::start(server_address(&dsn).await).await;
        let proxied = dsn_at(&pool, proxy.address()).await;
        let options = PoolOptions {
            max_connections: NonZeroU32::MIN,
            application_name: APP,
            default_isolation: Isolation::ServerDefault,
            session_budgets: SessionBudgets::Startup,
        };
        let ours = infra_postgres::prepare_pool(&proxied, &options);
        proxy.arm_autocommit(Fault::ForwardThenSilence, "pg_settings");
        let started = Instant::now();
        let mut admission = Box::pin(infra_postgres::admit_pool(&ours, &options));
        tokio::select! {
            result = &mut admission => panic!("session admission completed before its held reply: {result:?}"),
            () = proxy.silenced() => {}
        }
        if expire {
            let refused = tokio::time::timeout(Duration::from_secs(7), &mut admission)
                .await
                .expect("session admission must have its own client deadline")
                .unwrap_err();
            assert!(
                matches!(refused, ConnectError::SessionVerificationTimeout { budget } if budget == Duration::from_secs(5)),
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
            infra_postgres::close(&ours, RETURN_OBSERVATION_BUDGET).await,
            infra_postgres::Closed::Complete,
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
    // No recorder is installed here; the gauges must still be harmless.
    infra_postgres::record_metrics(&ours);
    assert_eq!(
        infra_postgres::close(&ours, Duration::from_secs(5)).await,
        infra_postgres::Closed::Complete
    );
}

#[sqlx::test(migrations = false)]
async fn pool_default_isolation_survives_replacement_and_explicit_transactions_override_it(
    pool: PgPool,
) {
    let dsn = dsn_for(&pool).await;
    let ours = pool_with_default_isolation(&dsn, 1, Isolation::ReadCommitted).await;

    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&ours)
        .await
        .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the fixture forces physical session replacement"
    )]
    let physical_connection = ours.acquire().await.unwrap();
    physical_connection.close().await.unwrap();

    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&ours)
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

    let explicit: Result<String, AppError> = in_tx_with(
        &ours,
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
        .fetch_one(&ours)
        .await
        .unwrap();

    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(first_pid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(terminated);
    // Past the pool's one-second idle threshold, so the next acquire pings.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&ours)
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
        .fetch_one(&ours)
        .await
        .unwrap();

    // The release ping of the first query is itself a round trip; silence the
    // peer only once the connection is back in the pool.
    wait_for_idle(&ours).await;
    proxy.silence_connection().await;
    // Past the pool's one-second idle threshold, so the next acquire pings.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let started = Instant::now();
    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&ours)
        .await
        .expect("the bounded idle ping discards the silent connection");
    assert_ne!(first_pid, second_pid);
    // The ping waited its own bound, not the rest of the acquire budget.
    assert!(started.elapsed() >= Duration::from_millis(1900));
    assert!(started.elapsed() < ACQUIRE_TIMEOUT);
    ours.close().await;
    proxy.shutdown().await;
}

/// Each path leaves different driver state: a pooled statement, an open
/// transaction, pre-commit verification, or a COMMIT whose durable result is
/// hidden. All must release native capacity even though the old relay never
/// delivers another byte. Cancellation itself makes no finality claim.
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
            .fetch_one(&ours)
            .await
            .unwrap();
        let mut pending = Box::pin(async {
            if boundary == "pooled statement" {
                proxy.arm_autocommit(Fault::ForwardThenSilence, "pool_return_finality");
                sqlx::query("INSERT INTO pool_return_finality VALUES ($1)")
                    .bind(boundary)
                    .execute(&ours)
                    .await?;
                return Ok::<(), AppError>(());
            }
            if boundary == "transaction statement" {
                proxy.arm_autocommit(Fault::ForwardThenSilence, "pool_return_finality");
            } else if boundary == "commit" {
                proxy.arm(Fault::ForwardThenSilence);
            }
            in_tx(&ours, async |tx| {
                sqlx::query("INSERT INTO pool_return_finality VALUES ($1)")
                    .bind(boundary)
                    .execute(&mut *tx)
                    .await?;
                if boundary == "pre-commit" {
                    // Borrowing withdraws the seam's statement-success proof,
                    // so it verifies with SELECT 1 before sending COMMIT.
                    let _ = connection(tx);
                    proxy.arm_autocommit(Fault::ForwardThenSilence, "SELECT 1");
                }
                Ok::<(), AppError>(())
            })
            .await
        });
        tokio::select! {
            result = &mut pending => panic!("{boundary} completed before its held reply: {result:?}"),
            () = proxy.silenced() => {}
        }
        drop(pending);

        wait_for_slot_reclamation(&ours).await;
        let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&ours)
            .await
            .expect("later ordinary work succeeds with the same one-slot pool");
        assert_ne!(first_pid, second_pid, "{boundary}");
        let reused_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&ours)
            .await
            .unwrap();
        assert_eq!(
            reused_pid, second_pid,
            "healthy replacement remains reusable"
        );
        assert_eq!(ours.options().get_max_connections(), 1);
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
        ours.close().await;
        proxy.shutdown().await;
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the fixture observes raw native pool ownership and acquisition"
)]
#[sqlx::test(migrations = false)]
async fn a_successful_statement_still_has_a_bounded_silent_return(pool: PgPool) {
    let (proxy, ours) = proxied_pool(&pool, 1).await;
    // Repeating on the same pool catches cumulative permit loss. Each old
    // socket remains silent through replacement work and the next cycle.
    for _ in 0..2 {
        let mut conn = ours.acquire().await.unwrap();
        let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        proxy.silence_connection().await;
        drop(conn);
        wait_for_slot_reclamation(&ours).await;
        let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&ours)
            .await
            .unwrap();
        assert_ne!(first_pid, second_pid);
        assert_eq!(ours.options().get_max_connections(), 1);
        wait_for_idle(&ours).await;
    }
    ours.close().await;
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
            .fetch_one(&ours)
            .await
            .unwrap(),
        42
    );
    ours.close().await;
    proxy.shutdown().await;
}

#[expect(
    clippy::disallowed_methods,
    reason = "the fixture observes raw native pool ownership and acquisition"
)]
#[sqlx::test(migrations = false)]
async fn cancelling_an_acquire_wait_preserves_capacity_and_healthy_reuse(pool: PgPool) {
    let ours = template_pool(&dsn_for(&pool).await, 1).await;
    let mut held = ours.acquire().await.unwrap();
    let before: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), ours.acquire())
            .await
            .is_err()
    );
    drop(held);
    let after: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&ours)
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "a cancelled waiter does not discard a healthy return"
    );
    ours.close().await;
}

#[sqlx::test(migrations = false)]
async fn budgets_the_server_carries_are_verified_when_the_pool_opens(pool: PgPool) {
    let dsn = dsn_for(&pool).await;

    // The compose server sets no session timeout of its own.
    let unlimited = pool_with_server_budgets(&dsn, Isolation::ServerDefault)
        .await
        .unwrap_err();
    assert!(
        matches!(
            unlimited,
            ConnectError::SessionBudget {
                setting: "statement_timeout",
                found_ms: 0,
                ..
            }
        ),
        "{unlimited}"
    );

    alter_database(&pool, "statement_timeout = '30s'").await;
    alter_database(&pool, "idle_in_transaction_session_timeout = '8s'").await;
    let looser = pool_with_server_budgets(&dsn, Isolation::ServerDefault)
        .await
        .unwrap_err();
    assert!(
        matches!(
            looser,
            ConnectError::SessionBudget {
                setting: "statement_timeout",
                found_ms: 30_000,
                ..
            }
        ),
        "{looser}"
    );

    // A stricter limit than the template's is the operator's to choose.
    alter_database(&pool, "statement_timeout = '5s'").await;
    let ours = pool_with_server_budgets(&dsn, Isolation::ServerDefault)
        .await
        .expect("the database carries both budgets");
    assert_eq!(show(&ours, "statement_timeout").await, "5s");
    assert_eq!(
        show(&ours, "idle_in_transaction_session_timeout").await,
        "8s"
    );
    ours.close().await;

    alter_database(&pool, "default_transaction_isolation = 'serializable'").await;
    let isolation = pool_with_server_budgets(&dsn, Isolation::ReadCommitted)
        .await
        .unwrap_err();
    assert!(
        matches!(
            isolation,
            ConnectError::SessionIsolation {
                expected: "READ COMMITTED",
                ..
            }
        ),
        "{isolation}"
    );
}

#[sqlx::test(migrations = false)]
async fn through_a_transaction_pooler_the_published_budgets_reach_every_transaction(pool: PgPool) {
    sqlx::query("CREATE TABLE pooled_items (id int PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    // Opening the pool already verified the budgets through the pooler.
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

    // Two transactions at a time, so the pooler hands the same prepared
    // statement to more than one server connection.
    let insert = async |id: i32| -> Result<(), AppError> {
        in_tx(&ours, async |tx| {
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
    let rolled_back: Result<(), AppError> = in_tx(&ours, async |tx| {
        sqlx::query("INSERT INTO pooled_items (id) VALUES (100)")
            .execute(&mut *tx)
            .await?;
        Err(AppError::Business)
    })
    .await;
    assert!(matches!(rolled_back, Err(AppError::Business)));
    let swallowed: Result<(), AppError> = in_tx(&ours, async |tx| {
        let duplicate = sqlx::query("INSERT INTO pooled_items (id) VALUES (0)")
            .execute(&mut *tx)
            .await;
        assert!(duplicate.is_err());
        Ok(())
    })
    .await;
    assert!(
        matches!(swallowed, Err(AppError::Tx(TxError::CommitFailed(_)))),
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
    // Before the pooler opens its first server connection to this database:
    // a session keeps the defaults it started with.
    alter_database(&pool, "statement_timeout = '8s'").await;
    alter_database(&pool, "idle_in_transaction_session_timeout = '8s'").await;
    let ours = pool_with_server_budgets(&pooler_dsn_for(&pool).await, Isolation::ReadCommitted)
        .await
        .expect("the database carries both budgets");
    assert_eq!(show(&ours, "statement_timeout").await, "8s");
    let committed: Result<i32, AppError> = in_tx(&ours, async |tx| {
        Ok(sqlx::query_scalar("SELECT 1").fetch_one(&mut *tx).await?)
    })
    .await;
    assert_eq!(committed.unwrap(), 1);
}

#[sqlx::test(migrations = false)]
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
async fn a_rotated_password_file_reaches_the_connections_opened_after_it(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    // Roles are cluster-wide; the tail of the per-test database name keeps this
    // one apart and the role inside PostgreSQL's 63-byte identifier limit.
    let database = dsn.database();
    let role = format!(
        "rotating_{}",
        &database[database.len().saturating_sub(24)..]
    );
    sqlx::query(AssertSqlSafe(format!(
        "CREATE ROLE {role:?} LOGIN PASSWORD 'first'"
    )))
    .execute(&pool)
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("password");
    std::fs::write(&file, "first\n").unwrap();
    let mut url = url_for(&pool, DATABASE_URL).await;
    url.set_username(&role).unwrap();
    url.set_password(None).unwrap();
    let rotating = Dsn::admit_with(url.as_str(), Some(&file)).expect("the file is the password");
    let ours = template_pool(&rotating, 1).await;
    let cancel = CancellationToken::new();
    let refresh = tokio::spawn(refresh_password_periodically(
        ours.clone(),
        rotating,
        cancel.clone(),
    ));
    let current_user = async || {
        sqlx::query_scalar::<_, String>("SELECT current_user")
            .fetch_one(&ours)
            .await
    };
    assert_eq!(current_user().await.unwrap(), role);

    sqlx::query(AssertSqlSafe(format!(
        "ALTER ROLE {role:?} PASSWORD 'second'"
    )))
    .execute(&pool)
    .await
    .unwrap();
    // The open session stays authenticated; a new connection is refused
    // while the file still holds the old password.
    #[expect(
        clippy::disallowed_methods,
        reason = "the rotation fixture forces a new physical session"
    )]
    ours.acquire().await.unwrap().close().await.unwrap();
    let refused = current_user().await.unwrap_err();
    assert_eq!(sqlstate(&refused).as_deref(), Some("28P01"), "{refused}");

    std::fs::write(&file, "second\n").unwrap();
    let deadline = Instant::now() + PASSWORD_REFRESH_INTERVAL + Duration::from_secs(5);
    let user = loop {
        match current_user().await {
            Ok(user) => break user,
            Err(err) if Instant::now() < deadline => {
                assert_eq!(sqlstate(&err).as_deref(), Some("28P01"), "{err}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(err) => panic!("the pool never picked up the rotated password: {err}"),
        }
    };
    assert_eq!(user, role);

    cancel.cancel();
    refresh.await.unwrap();
    ours.close().await;
    let _ = sqlx::query(AssertSqlSafe(format!("DROP ROLE {role:?}")))
        .execute(&pool)
        .await;
}

#[expect(
    clippy::disallowed_methods,
    reason = "the fixture observes raw native pool ownership and acquisition"
)]
#[sqlx::test(migrations = false)]
async fn acquisition_diagnostics_cover_transactions_history_and_readiness(pool: PgPool) {
    let ours = template_pool(&dsn_for(&pool).await, 1).await;
    let probe = PostgresProbe::new(ours.clone());
    let events = PoolEvents::default();
    // Interest must not depend on another test's thread-local subscriber.
    let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    async {
        let held = ours.acquire().await.unwrap();
        let started = Instant::now();
        let (transaction, history, readiness) = tokio::join!(
            in_tx(&ours, async |_tx| Ok::<_, AppError>(())),
            migrate::verify_history(&ours),
            probe.check(),
        );
        assert!(matches!(
            transaction,
            Err(AppError::Tx(TxError::Acquire(sqlx::Error::PoolTimedOut)))
        ));
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
            infra_postgres::acquire(&ours, "cancelled diagnostic"),
        )
        .await;
        assert!(cancelled.is_err());
        assert!(
            events.take().is_empty(),
            "unfinished acquire has no invented outcome"
        );
        drop(held);
        wait_for_idle(&ours).await;

        let held = ours.acquire().await.unwrap();
        let release = async {
            tokio::time::sleep(Duration::from_millis(1100)).await;
            drop(held);
        };
        let ((), transaction, history, readiness) = tokio::join!(
            release,
            in_tx(&ours, async |tx| {
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

        let execution: Result<i32, _> = infra_postgres::observed("diagnostic division", async {
            let mut connection = infra_postgres::acquire(&ours, "diagnostic division").await?;
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
        ours.close().await;
        assert!(matches!(
            infra_postgres::acquire(&ours, "closed diagnostic").await,
            Err(sqlx::Error::PoolClosed)
        ));
        assert!(
            events.take().is_empty(),
            "pool closure is not an acquire timeout"
        );
    }
    .with_subscriber(events.clone())
    .await;
}

#[expect(
    clippy::disallowed_methods,
    clippy::print_stdout,
    reason = "hold raw pool capacity and print observed recovery timing with --nocapture"
)]
#[sqlx::test(migrations = false)]
async fn responsive_saturation_recovers_work_and_readiness_under_current_policy(pool: PgPool) {
    let ours = template_pool(&dsn_for(&pool).await, 1).await;
    // The shipped defaults: retain both the refresher cadence and its failure
    // threshold; health's focused tests own threshold and staleness arithmetic.
    let policy = health::RefreshPolicy {
        interval: Duration::from_secs(2),
        probe_budget: Duration::from_secs(4),
        failure_threshold: 3,
    };
    let readiness =
        health::Readiness::new(vec![Box::new(PostgresProbe::new(ours.clone()))], policy);
    readiness.refresh().await;
    let reader = readiness.reader();
    assert!(reader.verdict().is_ok());
    let mut held = ours.acquire().await.unwrap();
    let events = PoolEvents::default();
    let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let cancel = CancellationToken::new();
    let observe = async {
        let saturated = Instant::now();
        tokio::time::timeout(Duration::from_secs(20), async {
            while reader.verdict().is_ok() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("current policy withdraws readiness after bounded saturation");
        let verdict = reader.verdict();
        assert!(
            matches!(
                verdict,
                Err(health::NotReady::ProbeFailed {
                    probe: "postgres",
                    ..
                })
            ),
            "{verdict:?}"
        );
        // The server still answers through the held slot: this is pool
        // saturation, not the separate silent-network return scenario.
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
        let value: i32 = in_tx(&ours, async |tx| {
            Ok::<i32, AppError>(sqlx::query_scalar("SELECT 42").fetch_one(&mut *tx).await?)
        })
        .await
        .unwrap();
        assert_eq!(value, 42);
        tokio::time::timeout(
            policy.interval + policy.probe_budget + Duration::from_secs(1),
            async {
                while reader.verdict().is_err() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            },
        )
        .await
        .expect("next successful refresh restores readiness");
        let recovery = events.take();
        assert!(
            recovery
                .iter()
                .any(|event| event["message"] == "readiness_recovered")
        );
        assert_eq!(ours.options().get_max_connections(), 1);
        println!(
            "responsive saturation: readiness lost after {lost_after:?}; useful work and readiness recovered {:?} after release; pool max=1",
            released.elapsed()
        );
        cancel.cancel();
    };
    async {
        tokio::join!(readiness.refresh_until(cancel.clone()), observe);
    }
    .with_subscriber(events.clone())
    .await;
    ours.close().await;
}

#[sqlx::test(migrations = false)]
async fn in_tx_commits_on_ok_and_rolls_back_on_err(pool: PgPool) {
    pool.execute("CREATE TABLE t (id int PRIMARY KEY)")
        .await
        .unwrap();

    let inserted: Result<u64, AppError> = in_tx(&pool, async |tx| {
        Ok(tx
            .execute("INSERT INTO t VALUES (1)")
            .await?
            .rows_affected())
    })
    .await;
    assert_eq!(inserted.unwrap(), 1);

    let failed: Result<(), AppError> = in_tx(&pool, async |tx| {
        tx.execute("INSERT INTO t VALUES (2)").await?;
        Err(AppError::Business)
    })
    .await;
    assert!(matches!(failed, Err(AppError::Business)), "{failed:?}");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM t")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "the failed closure's insert was rolled back");
}

#[sqlx::test(migrations = false)]
async fn a_finished_transaction_returns_its_connection_outside_any_transaction(pool: PgPool) {
    pool.execute("CREATE TABLE t (id int PRIMARY KEY)")
        .await
        .unwrap();
    // One connection, so every step below reuses the one the previous
    // transaction returned.
    let ours = template_pool(&dsn_for(&pool).await, 1).await;
    let serializable = TxOptions {
        isolation: Isolation::Serializable,
        read_only: false,
    };

    let committed: Result<(), AppError> = in_tx(&ours, async |tx| {
        tx.execute("INSERT INTO t VALUES (1)").await?;
        Ok(())
    })
    .await;
    assert!(committed.is_ok(), "{committed:?}");
    // An explicit `BEGIN` is refused inside a transaction sqlx still tracks.
    let explicit: Result<(), AppError> = in_tx_with(&ours, serializable, async |tx| {
        tx.execute("INSERT INTO t VALUES (2)").await?;
        Ok(())
    })
    .await;
    assert!(explicit.is_ok(), "{explicit:?}");

    let failed: Result<(), AppError> = in_tx_with(&ours, serializable, async |tx| {
        tx.execute("INSERT INTO t VALUES (3)").await?;
        Err(AppError::Business)
    })
    .await;
    assert!(matches!(failed, Err(AppError::Business)), "{failed:?}");
    // Autocommit on the same connection, visible at once to another one:
    // neither the commit nor the queued rollback left it inside a
    // transaction.
    ours.execute("INSERT INTO t VALUES (4)").await.unwrap();
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
    let result: Result<(), AppError> = in_tx(&pool, async |tx| {
        tx.execute("INSERT INTO t VALUES (1)").await?;
        let mut savepoint = connection(tx).begin().await?;
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
    let result: Result<(), AppError> = in_tx(&pool, async |tx| {
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
    let result: Result<(), AppError> = in_tx(&pool, async |tx| {
        tx.execute("INSERT INTO t VALUES (1)").await?;
        // The duplicate aborts the transaction; the closure ignores that.
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
    // The boundary saw a statement succeed, then lent the connection out and
    // cannot know what happened on it.
    let result: Result<(), AppError> = in_tx(&pool, async |tx| {
        tx.execute("INSERT INTO t VALUES (1)").await?;
        let _ = connection(tx).execute("INSERT INTO t VALUES (1)").await;
        Ok(())
    })
    .await;
    match result {
        Err(AppError::Tx(TxError::CommitFailed(err))) => {
            assert_eq!(sqlstate(&err).as_deref(), Some("25P02"));
        }
        other => panic!("expected CommitFailed from the borrowed connection, got {other:?}"),
    }

    // The same borrow with nothing failing commits, and so does a statement
    // through the handle after it.
    let borrowed: Result<(), AppError> = in_tx(&pool, async |tx| {
        connection(tx).execute("INSERT INTO t VALUES (2)").await?;
        Ok(())
    })
    .await;
    assert!(borrowed.is_ok(), "{borrowed:?}");
    let through_the_handle: Result<(), AppError> = in_tx(&pool, async |tx| {
        connection(tx).execute("INSERT INTO t VALUES (3)").await?;
        tx.execute("INSERT INTO t VALUES (4)").await?;
        Ok(())
    })
    .await;
    assert!(through_the_handle.is_ok(), "{through_the_handle:?}");
    let empty: Result<(), AppError> = in_tx(&pool, async |_tx| Ok(())).await;
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

    let other = pool.clone();
    let result: Result<(), AppError> = in_tx_with(
        &pool,
        TxOptions {
            isolation: Isolation::Serializable,
            read_only: false,
        },
        async |tx| {
            let seen: i32 = sqlx::query_scalar("SELECT n FROM counters WHERE id = 1")
                .fetch_one(&mut *tx)
                .await?;
            // A second writer commits between our read and our write.
            let concurrent: Result<(), AppError> = in_tx_with(
                &other,
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
async fn a_read_only_transaction_refuses_writes(pool: PgPool) {
    pool.execute("CREATE TABLE t (id int)").await.unwrap();
    let result: Result<(), AppError> = in_tx_with(
        &pool,
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
    // One pool connection makes the negative control discriminating: without
    // the pending-BEGIN guard, the next statement reuses a server session that
    // has entered the held transaction but has not delivered ReadyForQuery.
    let (proxy, proxied) = proxied_pool(&pool, 1).await;
    proxy.arm_ready_hold(ReadyBoundary::Begin);
    let mut pending = Box::pin(in_tx_with(
        &proxied,
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
    // Keep BEGIN unanswered until local disposal and useful replacement work
    // complete; the fixture has no timer that could release capacity for us.
    wait_for_slot_reclamation(&proxied).await;

    sqlx::query("INSERT INTO pending_begin_visibility VALUES (1)")
        .execute(&proxied)
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
        .fetch_one(&proxied)
        .await
        .unwrap();
    let options: Result<(String, String), AppError> = in_tx_with(
        &proxied,
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
        .fetch_one(&proxied)
        .await
        .unwrap();
    assert_eq!(
        session_before, session_after,
        "completed transactions retain normal pool reuse"
    );
    assert_eq!(
        infra_postgres::close(&proxied, Duration::from_secs(5)).await,
        infra_postgres::Closed::Complete
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
        let result = in_tx(&ours, async |tx| -> Result<i32, AppError> {
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
        // Foreground completion preserves its result before native cleanup runs.
        proxy.ready_held().await;
        assert_eq!(ours.size(), 1, "cleanup still owns the connection");
        if fail_return {
            // Stop the transport after COMMIT was acknowledged, during return.
            proxy.cancel();
        }
        wait_for_slot_reclamation(&ours).await;
        if !fail_return {
            proxy.release_ready();
        }
        if commit {
            assert!(matches!(result, Ok(value) if value == id), "{result:?}");
        } else {
            assert!(matches!(result, Err(AppError::Business)), "{result:?}");
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
        ours.close().await;
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
    assert_eq!(migrate::verify_history(&pool).await, Ok(()));

    // A later release that already migrated this database keeps an older
    // binary admissible: its version lies above the newest embedded one.
    let newer = target.unwrap_or(0) + 1;
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES ($1, 'later release', true, '\\x00'::bytea, 0)",
    )
    .bind(newer)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(migrate::verify_history(&pool).await, Ok(()));
    // A rolled-back release's migrate job also admits the later history.
    let rolled_back = migrate::run(&MIGRATOR, &options(&dsn)).await.unwrap();
    assert_eq!(rolled_back.applied, [] as [i64; 0]);
    assert_eq!(rolled_back.before, Some(newer));
}

#[sqlx::test(migrations = false)]
async fn history_admission_refuses_missing_bookkeeping_without_creating_it(pool: PgPool) {
    assert_eq!(
        migrate::verify_history(&pool).await,
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
    assert!(
        matches!(
            &err,
            RunError::Migrate(MigrateError::VersionMismatch(20_260_918_000_001))
        ),
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

    // An unknown version inside the embedded range is still refused (unit test).
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
    #[expect(
        clippy::disallowed_methods,
        reason = "the migration fixture holds an advisory lock"
    )]
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

    // The history table was created outside the migration transaction; the
    // migration's own row and its table are rolled back when the session is
    // dropped. `client_connection_check_interval` lets the server end it promptly.
    assert_eq!(applied_count(&pool).await, 0);
    #[expect(
        clippy::disallowed_methods,
        reason = "the migration fixture inspects a dedicated raw session"
    )]
    let mut conn = pool.acquire().await.unwrap();
    conn.lock()
        .await
        .expect("the dropped session released the lock");
    conn.unlock().await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn a_no_transaction_build_is_refused_until_its_invalid_index_is_dropped(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let fixture = fixture("concurrent_index").await;

    // The table migration commits; the unique build then fails on the
    // duplicate and leaves its index behind, invalid, with no history row.
    let err = migrate::run(&fixture, &options(&dsn)).await.unwrap_err();
    assert!(
        matches!(
            &err,
            RunError::Migrate(MigrateError::ExecuteMigration(_, 20_260_918_000_002))
        ),
        "{err}"
    );
    assert_eq!(applied_count(&pool).await, 1);

    // `IF NOT EXISTS` alone would now record the invalid index as built.
    let err = migrate::run(&fixture, &options(&dsn)).await.unwrap_err();
    assert_eq!(err.stage(), "execute", "{err}");
    assert!(
        matches!(
            &err,
            RunError::InvalidIndexes { version: 20_260_918_000_002, indexes } if indexes == "widgets_sku"
        ),
        "{err}"
    );
    assert_eq!(applied_count(&pool).await, 1);

    pool.execute("DROP INDEX CONCURRENTLY widgets_sku")
        .await
        .unwrap();
    pool.execute("DELETE FROM widgets WHERE id = 2")
        .await
        .unwrap();

    // A concurrent build waits for every transaction that wrote to the table
    // before it; that wait outlives the session `lock_timeout` here.
    #[expect(
        clippy::disallowed_methods,
        reason = "the concurrent-index fixture retains a blocking writer"
    )]
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

    // The first file slept past the statement budget and was applied; the
    // second ran under the restored budget and was cancelled by the server.
    let RunError::Migrate(MigrateError::ExecuteMigration(cause, 20_260_918_000_002)) = &err else {
        panic!("{err}");
    };
    assert_eq!(sqlstate(cause).as_deref(), Some("57014"), "{err}");
    let applied: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(applied, vec![20_260_918_000_001]);
}

#[sqlx::test(migrations = false)]
async fn an_unreachable_target_fails_in_the_connect_stage(pool: PgPool) {
    let _ = pool;
    let dsn = Dsn::admit("postgres://app:pw@127.0.0.1:1/app?sslmode=disable").unwrap();
    let err = migrate::run(&fixture("widgets").await, &options(&dsn))
        .await
        .unwrap_err();
    assert_eq!(err.stage(), "connect", "{err}");
    assert!(!err.to_string().contains("pw"), "{err}");
}

#[sqlx::test(migrations = false)]
async fn a_migration_that_fails_is_the_execute_stage(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    pool.execute("CREATE TABLE widgets (id int)").await.unwrap();
    let err = migrate::run(&fixture("widgets").await, &options(&dsn))
        .await
        .unwrap_err();
    assert_eq!(err.stage(), "execute", "{err}");
    assert!(
        matches!(
            &err,
            RunError::Migrate(MigrateError::ExecuteMigration(_, 20_260_918_000_001))
        ),
        "{err}"
    );
    assert_eq!(applied_count(&pool).await, 0);
}

#[path = "../fixtures/tls.rs"]
mod postgres_tls_material;

// Existing real database; this fixture changes only its local transport entry.
// Keep it in this target so the pool, DSN and native TLS path remain the owners.
struct PostgresTlsRelay {
    cancel: CancellationToken,
    tasks: tokio_util::task::TaskTracker,
    server_names: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

struct PostgresTlsMaterial {
    root: String,
    acceptor: tokio_rustls::TlsAcceptor,
}

impl PostgresTlsMaterial {
    fn new() -> Self {
        use base64::Engine as _;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        use tokio_rustls::rustls;

        let material = postgres_tls_material::TlsMaterial::new("localhost");
        let root = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            base64::engine::general_purpose::STANDARD.encode(&material.root),
        );
        let tls = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(material.cert)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(material.key)),
        )
        .unwrap();
        Self {
            root,
            acceptor: tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(tls)),
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
        let tasks = tokio_util::task::TaskTracker::new();
        let server_names = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let run_cancel = cancel.clone();
        let run_tasks = tasks.clone();
        let run_names = server_names.clone();
        tasks.spawn(async move {
            while let Some(accepted) = run_cancel.run_until_cancelled(listener.accept()).await {
                let (socket, _) = accepted.unwrap();
                let acceptor = acceptor.clone();
                let connection_cancel = run_cancel.child_token();
                let names = run_names.clone();
                run_tasks.spawn(async move {
                    let _ = connection_cancel
                        .run_until_cancelled(Self::forward(socket, target, acceptor, names))
                        .await;
                });
            }
        });
        Self {
            cancel,
            tasks,
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
        // Verification failures are expected in the invalid-root phases.
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

    async fn stop(self) {
        self.cancel.cancel();
        self.tasks.close();
        tokio::time::timeout(Duration::from_secs(3), self.tasks.wait())
            .await
            .expect("all TLS relay tasks terminate");
        assert!(
            self.server_names
                .lock()
                .unwrap()
                .iter()
                .all(|name| name == "localhost")
        );
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the rotation fixture closes its native session to require fresh authentication"
)]
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
        .fetch_one(&ours)
        .await
        .unwrap();
    wait_for_idle(&ours).await;

    // A file change does not reauthenticate an already authenticated session.
    tokio::fs::write(&root_file, "invalid replacement")
        .await
        .unwrap();
    let retained_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&ours)
        .await
        .unwrap();
    assert_eq!(retained_pid, first_pid);
    ours.acquire().await.unwrap().close().await.unwrap();
    let invalid = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&ours)
        .await;
    assert!(
        invalid
            .as_ref()
            .is_err_and(|error| is_certificate_error(error, &CertificateError::UnknownIssuer)),
        "{invalid:?}"
    );
    first_relay.stop().await;

    // The same hostname and pool now need the other localhost address. Neither
    // process DNS nor the client's destination/SNI policy is rewritten.
    let listener = tokio::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port))
        .await
        .unwrap();
    let replacement = PostgresTlsRelay::start(listener, target, second.acceptor);
    tokio::fs::write(&root_file, &first.root).await.unwrap();
    let untrusted = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&ours)
        .await;
    // Both fixture CAs have the same issuer name but different signing keys.
    assert!(
        untrusted
            .as_ref()
            .is_err_and(|error| is_certificate_error(error, &CertificateError::BadSignature)),
        "{untrusted:?}"
    );
    tokio::fs::write(&root_file, &second.root).await.unwrap();
    let replacement_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&ours)
        .await
        .unwrap();
    assert_ne!(replacement_pid, first_pid);
    assert_eq!(ours.options().get_max_connections(), 1);
    assert!(PostgresProbe::new(ours.clone()).check().await.is_ok());
    assert!(!replacement.server_names.lock().unwrap().is_empty());
    ours.close().await;
    replacement.stop().await;
}
