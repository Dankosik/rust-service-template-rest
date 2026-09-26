//! Real-PostgreSQL proof for durable background jobs.
//!
//! Every test gets its own database from `#[sqlx::test]`, migrated with the
//! embedded set except the missing-schema case. Waits are bounded and every
//! spawned task is joined.
//!
//! `enqueue` proves enqueue through `in_tx`: the committed row, rollback and a
//! rejected commit leaving nothing, validation and database failures, and every
//! uniqueness outcome. `http_idempotency` proves the joint boundary: a
//! committed attempt enqueues once, and a replayed, rolled-back, or in-progress
//! attempt enqueues nothing.

#![cfg(feature = "integration")]
// Integration tests are test code; the workspace's production lint levels
// for unwrap/expect/panic do not apply to them.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

#[path = "../support/commit_proxy.rs"]
mod commit_proxy;
mod enqueue;
mod execution;
mod migration;
mod process;
// template:begin jobs-http-idempotency:jobs-http-idempotency-module
mod http_idempotency;
// template:end jobs-http-idempotency:jobs-http-idempotency-module

use std::borrow::Cow;
use std::future::Future;
use std::num::NonZeroU32;
use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt as _;
use infra_postgres::{Closed, Dsn, PgPool, PoolOptions};

const APP: &str = "integration-tests-jobs";
/// Bound on every wait in this suite.
const WAIT: Duration = Duration::from_secs(10);
/// Pause between polls for a lock waiter.
const POLL: Duration = Duration::from_millis(20);
/// Bound on closing a pool.
const CLOSE_BUDGET: Duration = Duration::from_secs(5);

/// The template pool on `dsn`: the service's session defaults and budgets.
pub(crate) async fn template_pool(dsn: &Dsn, max_connections: u32) -> PgPool {
    infra_postgres::connect(
        dsn,
        &PoolOptions {
            max_connections: NonZeroU32::new(max_connections).expect("a pool size"),
            application_name: APP,
        },
    )
    .await
    .expect("the template pool connects")
}

/// Close each pool within its budget.
pub(crate) async fn close(pools: &[&PgPool]) {
    for pool in pools {
        assert_eq!(
            infra_postgres::close(pool, CLOSE_BUDGET).await,
            Closed::Complete
        );
    }
}

/// How many jobs are committed.
pub(crate) async fn job_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM background_jobs")
        .fetch_one(pool)
        .await
        .expect("a job count")
}

/// SQLSTATE of a database error, when the driver reported one.
pub(crate) fn sqlstate(err: &sqlx::Error) -> Option<Cow<'_, str>> {
    err.as_database_error()?.code()
}

/// `future`, which must finish within [`WAIT`].
pub(crate) async fn bounded<F: Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(WAIT, future).await.expect(what)
}

/// Poll until a template-pool backend on this database waits on a lock.
pub(crate) async fn wait_for_lock_waiter(pool: &PgPool) {
    bounded("a lock waiter", async {
        loop {
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
            if waiting > 0 {
                return;
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
}

/// See a lock waiter, run `release`, then join `task`. A missed waiter still
/// releases and joins before the test fails, so the task cannot stay parked.
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
