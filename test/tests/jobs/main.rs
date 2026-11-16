#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a broken fixture or setup fails the test"
)]

#[allow(
    dead_code,
    reason = "the shared transport also supplies pending-BEGIN cancellation proof to the postgres integration target"
)]
#[path = "../support/commit_proxy.rs"]
mod commit_proxy;
mod enqueue;
mod execution;
mod operator;
mod process;
// template:begin jobs-reference:jobs-reading-counter-module
mod reliability;
// template:end jobs-reference:jobs-reading-counter-module
// template:begin jobs-http-idempotency:jobs-http-idempotency-module
mod http_idempotency;
// template:end jobs-http-idempotency:jobs-http-idempotency-module

use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt as _;
use infra_postgres::{Dsn, Isolation, PgPool};

pub(crate) use infra_postgres::sqlstate;

const APP: &str = "integration-tests-jobs";
const WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, derive_more::From, derive_more::Display)]
enum AttemptError {
    #[display("enqueue failed: {_0}")]
    Enqueue(infra_jobs::EnqueueError),
    #[display("query failed: {_0}")]
    Query(sqlx::Error),
    #[from(skip)]
    #[display("the caller rejected the transaction")]
    Rejected,
    #[display("transaction failed: {_0}")]
    Tx(infra_postgres::TxError),
}

pub(crate) async fn template_pool(dsn: &Dsn, max_connections: u32) -> PgPool {
    integration_tests::template_pool(dsn, APP, max_connections, Isolation::ReadCommitted).await
}

pub(crate) use integration_tests::close_pools;

pub(crate) async fn job_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM background_jobs")
        .fetch_one(pool)
        .await
        .expect("a job count")
}

pub(crate) async fn bounded<F: Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(WAIT, future).await.expect(what)
}

pub(crate) async fn wait_for_lock_waiter(pool: &PgPool) {
    integration_tests::poll_until("a lock waiter", WAIT, POLL, async || {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
             WHERE datname = current_database() \
               AND wait_event_type = 'Lock' \
               AND application_name = $1 \
               AND pid <> pg_backend_pid()",
        )
        .bind(APP)
        .fetch_one(pool)
        .await
        .expect("pg_stat_activity");
        (waiting > 0).then_some(())
    })
    .await;
}

pub(crate) async fn join_after_lock_wait<T>(
    observer: &PgPool,
    release: impl Future<Output = ()>,
    task: tokio::task::JoinHandle<T>,
) -> T {
    let saw = AssertUnwindSafe(async { wait_for_lock_waiter(observer).await })
        .catch_unwind()
        .await;
    release.await;
    let joined = bounded("the waiting task", task)
        .await
        .expect("the waiting task joins");
    if let Err(payload) = saw {
        std::panic::resume_unwind(payload);
    }
    joined
}
