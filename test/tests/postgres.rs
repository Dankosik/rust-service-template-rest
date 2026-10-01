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

use std::net::{Ipv4Addr, SocketAddr};
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use commit_proxy::CommitProxy;
use health::Probe;
use infra_postgres::{
    ACQUIRE_TIMEOUT, ConnectError, Dsn, Isolation, PASSWORD_REFRESH_INTERVAL, PgPool, PoolOptions,
    PostgresProbe, SessionBudgets, TxError, TxOptions, connection, in_tx, in_tx_with,
    refresh_password_periodically, retryable, sqlstate,
};
use integration_tests::{DATABASE_URL, dsn_for, fixture_dir, pooler_dsn_for, url_for};
use migrate::{HistoryError, MIGRATOR, RunError, RunOptions};
use sqlx::migrate::{Migrate, MigrateError, Migrator};
use sqlx::{AssertSqlSafe, Connection, Executor};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const APP: &str = "integration-tests";

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

/// A TCP relay that can stop relaying the connections it already carries
/// without closing them, which is what a client sees of a peer that vanished
/// without a reset. Connections accepted afterwards are relayed as usual.
async fn silenceable_relay(server: SocketAddr) -> (SocketAddr, watch::Sender<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (silence, _) = watch::channel(());
    let silenced = silence.clone();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let mut went_silent = silenced.subscribe();
            tokio::spawn(async move {
                let Ok(mut upstream) = TcpStream::connect(server).await else {
                    return;
                };
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {}
                    // Both sockets stay open and say nothing more.
                    _ = went_silent.changed() => std::future::pending::<()>().await,
                }
            });
        }
    });
    (address, silence)
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
    RunOptions::defaults(dsn, APP)
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
    let dsn = dsn_for(&pool).await;
    let (relay, silence) = silenceable_relay(server_address(&dsn).await).await;
    let ours = template_pool(&dsn_at(&pool, relay).await, 1).await;
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&ours)
        .await
        .unwrap();

    silence.send_replace(());
    // Past the pool's one-second idle threshold, so the next acquire pings.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let started = Instant::now();
    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&ours)
        .await
        .expect("the bounded idle ping discards the silent connection");
    assert_ne!(first_pid, second_pid);
    // The ping waited its own bound, not the rest of the acquire budget.
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert!(started.elapsed() < ACQUIRE_TIMEOUT);
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
async fn a_rotated_password_file_reaches_the_connections_opened_after_it(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    // Roles are cluster-wide; the per-test database name keeps this one apart.
    let role = format!("rotating{}", dsn.database());
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

#[sqlx::test(migrations = false)]
async fn probe_is_ready_and_fails_generically_when_the_pool_is_exhausted(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let ours = template_pool(&dsn, 1).await;
    let probe = PostgresProbe::new(ours.clone());
    assert_eq!(probe.name(), "postgres");
    probe.check().await.expect("ready");

    let _held = ours.acquire().await.unwrap();
    let started = std::time::Instant::now();
    let err = probe.check().await.unwrap_err();
    assert!(started.elapsed() + Duration::from_millis(200) >= ACQUIRE_TIMEOUT);
    assert_eq!(
        err.to_string(),
        "no connection available inside the acquire budget"
    );
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
        let _ = tx.execute("INSERT INTO t VALUES (1)").await;
        Ok(())
    })
    .await;
    assert!(
        matches!(result, Err(AppError::Tx(TxError::CommitFailed(_)))),
        "{result:?}"
    );

    // The same borrow with nothing failing commits, and so does a statement
    // through the handle after it.
    let borrowed: Result<(), AppError> = in_tx(&pool, async |tx| {
        tx.execute("INSERT INTO t VALUES (2)").await?;
        Ok(())
    })
    .await;
    assert!(borrowed.is_ok(), "{borrowed:?}");
    let through_the_handle: Result<(), AppError> = in_tx(&pool, async |tx| {
        tx.execute("INSERT INTO t VALUES (3)").await?;
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
async fn a_serialization_failure_is_retryable(pool: PgPool) {
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
            assert!(retryable(&err), "{err}");
            assert_eq!(err.as_database_error().unwrap().code().unwrap(), "40001");
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
    proxy.arm_begin_ready_hold();
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
        () = proxy.begin_ready_held() => {}
    }
    drop(pending);
    proxy.release_begin_ready();

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
    assert!(second.applied.is_empty());
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
    assert!(repeated.applied.is_empty());
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
    assert!(rolled_back.applied.is_empty());
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
    assert!(result.applied.is_empty());
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

    // The history table was created outside the migration transaction; the
    // migration's own row and its table are rolled back when the session is
    // dropped. `client_connection_check_interval` lets the server end it promptly.
    assert_eq!(applied_count(&pool).await, 0);
    let mut conn = pool.acquire().await.unwrap();
    conn.lock()
        .await
        .expect("the dropped session released the lock");
    conn.unlock().await.unwrap();
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
