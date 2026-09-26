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

use std::num::NonZeroU32;
use std::time::Duration;

use commit_proxy::CommitProxy;
use health::Probe;
use infra_postgres::{
    ACQUIRE_TIMEOUT, Dsn, Isolation, PgPool, PoolOptions, PostgresProbe, TxError, TxOptions,
    connection, in_tx, in_tx_with, retryable,
};
use integration_tests::{DATABASE_URL, dsn_for, fixture_dir};
use migrate::{HistoryError, MIGRATOR, RunError, RunOptions, Stage};
use sqlx::Executor;
use sqlx::migrate::{Migrate, MigrateError, Migrator};
use url::Url;

const APP: &str = "integration-tests";

async fn template_pool(dsn: &Dsn, max_connections: u32) -> PgPool {
    infra_postgres::connect(
        dsn,
        &PoolOptions {
            max_connections: NonZeroU32::new(max_connections).expect("pool size in tests"),
            application_name: APP,
        },
    )
    .await
    .expect("pool connects")
}

async fn proxied_pool(pool: &PgPool, max_connections: u32) -> (CommitProxy, PgPool) {
    let dsn = dsn_for(pool).await;
    assert_eq!(
        dsn.ssl_mode_name(),
        "disable",
        "the test proxy frames the plaintext protocol"
    );
    let host = dsn.host().trim_start_matches('[').trim_end_matches(']');
    let server = tokio::net::lookup_host((host, dsn.port()))
        .await
        .expect("the server address resolves")
        .next()
        .expect("the server has an address");
    let proxy = CommitProxy::start(server).await;
    let raw = std::env::var(DATABASE_URL).expect("DATABASE_URL is set");
    let mut url = Url::parse(&raw).expect("DATABASE_URL is a URL");
    url.set_path(dsn.database());
    url.set_ip_host(proxy.address().ip())
        .expect("the proxy address is a host");
    url.set_port(Some(proxy.address().port()))
        .expect("the proxy URL accepts a port");
    let proxied = Dsn::admit(url.as_str()).expect("the proxied DSN is admitted");
    (proxy, template_pool(&proxied, max_connections).await)
}

async fn show(pool: &PgPool, setting: &'static str) -> String {
    let sql = match setting {
        "statement_timeout" => "SHOW statement_timeout",
        "idle_in_transaction_session_timeout" => "SHOW idle_in_transaction_session_timeout",
        "application_name" => "SHOW application_name",
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
    assert_eq!(err.0, "no connection available inside the acquire budget");
}

#[sqlx::test(migrations = false)]
async fn in_tx_commits_on_ok_and_rolls_back_on_err(pool: PgPool) {
    pool.execute("CREATE TABLE t (id int PRIMARY KEY)")
        .await
        .unwrap();

    let inserted: Result<u64, AppError> = in_tx(&pool, async |tx| {
        Ok(connection(tx)
            .execute("INSERT INTO t VALUES (1)")
            .await?
            .rows_affected())
    })
    .await;
    assert_eq!(inserted.unwrap(), 1);

    let failed: Result<(), AppError> = in_tx(&pool, async |tx| {
        connection(tx).execute("INSERT INTO t VALUES (2)").await?;
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
async fn a_commit_the_server_rejects_is_commit_failed(pool: PgPool) {
    pool.execute(
        "CREATE TABLE t (id int PRIMARY KEY, other int, \
         CONSTRAINT t_other UNIQUE (other) DEFERRABLE INITIALLY DEFERRED)",
    )
    .await
    .unwrap();
    let result: Result<(), AppError> = in_tx(&pool, async |tx| {
        connection(tx)
            .execute("INSERT INTO t VALUES (1, 1), (2, 1)")
            .await?;
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
                .fetch_one(&mut *connection(tx))
                .await?;
            // A second writer commits between our read and our write.
            let concurrent: Result<(), AppError> = in_tx_with(
                &other,
                TxOptions {
                    isolation: Isolation::Serializable,
                    read_only: false,
                },
                async |tx| {
                    connection(tx)
                        .execute("UPDATE counters SET n = n + 1 WHERE id = 1")
                        .await?;
                    Ok(())
                },
            )
            .await;
            concurrent.expect("the concurrent writer commits first");
            sqlx::query("UPDATE counters SET n = $1 WHERE id = 1")
                .bind(seen + 1)
                .execute(&mut *connection(tx))
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
            connection(tx).execute("INSERT INTO t VALUES (1)").await?;
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
                .fetch_one(&mut *connection(tx))
                .await?;
            let read_only = sqlx::query_scalar("SHOW transaction_read_only")
                .fetch_one(&mut *connection(tx))
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
    assert_eq!(first.target, Some(20_260_918_000_002));
    assert_eq!(first.after, Some(20_260_918_000_002));
    assert_eq!(first.applied, 2);
    assert_eq!(first.outcome().as_str(), "success");
    assert_eq!(applied_count(&pool).await, 2);
    pool.execute("INSERT INTO widgets (id, name, sku) VALUES (1, 'w', 's')")
        .await
        .unwrap();

    let second = migrate::run(&widgets, &options(&dsn)).await.unwrap();
    assert_eq!(second.before, Some(20_260_918_000_002));
    assert_eq!(second.after, Some(20_260_918_000_002));
    assert_eq!(second.applied, 0);
    assert_eq!(second.outcome().as_str(), "no_change");
}

#[sqlx::test(migrations = false)]
async fn the_embedded_set_runs_on_an_empty_database(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let result = migrate::run(&MIGRATOR, &options(&dsn)).await.unwrap();
    assert_eq!(result.applied, MIGRATOR.iter().count());
    assert_eq!(result.target, MIGRATOR.iter().map(|m| m.version).max());
    assert_eq!(result.after, result.target);
    assert_eq!(
        applied_count(&pool).await,
        i64::try_from(result.applied).unwrap()
    );

    let repeated = migrate::run(&MIGRATOR, &options(&dsn)).await.unwrap();
    assert_eq!(repeated.before, result.target);
    assert_eq!(repeated.target, result.target);
    assert_eq!(repeated.after, result.target);
    assert_eq!(repeated.applied, 0);
    assert_eq!(repeated.outcome().as_str(), "no_change");
    assert_eq!(migrate::verify_history(&pool).await, Ok(()));
}

#[sqlx::test(migrations = false)]
async fn history_admission_refuses_missing_bookkeeping_without_creating_it(pool: PgPool) {
    assert_eq!(
        migrate::verify_history(&pool).await,
        Err(HistoryError::Unavailable)
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
async fn an_edited_applied_migration_fails_in_the_state_stage(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    migrate::run(&fixture("widgets").await, &options(&dsn))
        .await
        .unwrap();

    let err = migrate::run(&fixture("widgets_edited").await, &options(&dsn))
        .await
        .unwrap_err();
    assert_eq!(err.stage(), Stage::History);
    assert!(
        matches!(
            *err.error,
            RunError::Migrate {
                source: MigrateError::VersionMismatch(20_260_918_000_001),
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(err.observed.before, Some(20_260_918_000_002));
    assert_eq!(err.observed.target, Some(20_260_918_000_002));
    assert_eq!(applied_count(&pool).await, 2, "nothing was re-applied");
}

#[sqlx::test(migrations = false)]
async fn a_removed_applied_migration_fails_in_the_state_stage(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    migrate::run(&fixture("widgets").await, &options(&dsn))
        .await
        .unwrap();

    let err = migrate::run(&fixture("widgets_partial").await, &options(&dsn))
        .await
        .unwrap_err();
    assert_eq!(err.stage(), Stage::History);
    assert!(
        matches!(
            *err.error,
            RunError::Migrate {
                source: MigrateError::VersionMissing(20_260_918_000_002),
                ..
            }
        ),
        "{err}"
    );
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
    assert_eq!(err.stage(), Stage::Lock, "{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        err.observed.before, None,
        "the history was not read without the lock"
    );
    assert_eq!(err.observed.target, Some(20_260_918_000_002));
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
    assert_eq!(err.stage(), Stage::Deadline, "{err}");
    assert!(started.elapsed() < Duration::from_secs(3));

    // The history table was created outside the migration transaction; the
    // migration's own row and its table are rolled back with the dropped
    // session once the server notices.
    assert_eq!(applied_count(&pool).await, 0);
    let mut conn = pool.acquire().await.unwrap();
    conn.lock()
        .await
        .expect("the dropped session released the lock");
    conn.unlock().await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn a_source_rule_violation_fails_before_connecting(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let err = migrate::run(&fixture("no_tx").await, &options(&dsn))
        .await
        .unwrap_err();
    assert_eq!(err.stage(), Stage::Source);
    assert!(err.to_string().contains("no-transaction"), "{err}");
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        !exists,
        "no connection was made, so no history table exists"
    );
}

#[sqlx::test(migrations = false)]
async fn an_unreachable_target_fails_in_the_connect_stage(pool: PgPool) {
    let _ = pool;
    let dsn = Dsn::admit("postgres://app:pw@127.0.0.1:1/app?sslmode=disable").unwrap();
    let err = migrate::run(&fixture("widgets").await, &options(&dsn))
        .await
        .unwrap_err();
    assert_eq!(err.stage(), Stage::Connect, "{err}");
    assert!(!err.to_string().contains("pw"), "{err}");
}

#[sqlx::test(migrations = false)]
async fn a_migration_that_fails_is_the_execute_stage(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    pool.execute("CREATE TABLE widgets (id int)").await.unwrap();
    let err = migrate::run(&fixture("widgets").await, &options(&dsn))
        .await
        .unwrap_err();
    assert_eq!(err.stage(), Stage::SqlExecute, "{err}");
    assert_eq!(applied_count(&pool).await, 0);
}
