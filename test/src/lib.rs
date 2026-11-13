//! Helpers shared by the database-backed tests under `tests/`.
//!
//! `#[sqlx::test]` hands each test its own database created from
//! `DATABASE_URL`; the template's own entry points take an admitted
//! [`Dsn`], so the helpers here turn one into the other.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a helper that cannot do its job fails the calling test; a `Result` would move the same panic into every test body"
)]

// template:begin jobs:test-lib-jobs-module
pub mod jobs;
// template:end jobs:test-lib-jobs-module

// template:begin jobs-reference:test-lib-reading-counter
pub mod reading_counter;
pub mod reading_counter_receiver;
// template:end jobs-reference:test-lib-reading-counter

use std::num::NonZeroU32;
use std::time::Duration;

use infra_postgres::{CloseOutcome, Dsn, Isolation, PoolOptions, SessionSettingsSource};
use sqlx::PgPool;
use url::Url;

/// How long [`close_pools`] waits for each pool's connections to return.
const CLOSE_BUDGET: Duration = Duration::from_secs(5);

/// The environment variable `#[sqlx::test]` reads; the script that owns the
/// compose lifecycle exports it in the admitted URL form.
pub const DATABASE_URL: &str = "DATABASE_URL";

/// The admitted URL of the PgBouncer the compose file puts in front of the
/// same server, in transaction pooling mode; exported by the same script.
pub const PGBOUNCER_DATABASE_URL: &str = "PGBOUNCER_DATABASE_URL";

/// Directory of the migration fixtures, one subdirectory per scenario.
#[must_use]
pub fn fixture_dir(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("migrations")
        .join(name)
}

/// The admitted DSN of the per-test database `pool` is connected to.
///
/// # Panics
///
/// When `DATABASE_URL` is unset or not admissible: the test cannot run
/// without it, and a silent skip would count as a pass.
pub async fn dsn_for(pool: &PgPool) -> Dsn {
    Dsn::admit(url_for(pool, DATABASE_URL).await.as_str())
        .expect("DATABASE_URL must be an admitted DSN")
}

/// The admitted DSN of the same per-test database, reached through the
/// pooler.
///
/// # Panics
///
/// When `PGBOUNCER_DATABASE_URL` is unset or not admissible, for the reason
/// [`dsn_for`] gives.
pub async fn pooler_dsn_for(pool: &PgPool) -> Dsn {
    Dsn::admit(url_for(pool, PGBOUNCER_DATABASE_URL).await.as_str())
        .expect("PGBOUNCER_DATABASE_URL must be an admitted DSN")
}

/// The URL in `variable`, pointed at the per-test database of `pool`.
///
/// # Panics
///
/// When `variable` is unset or not a URL.
pub async fn url_for(pool: &PgPool, variable: &str) -> Url {
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await
        .expect("current_database()");
    let raw = std::env::var(variable)
        .unwrap_or_else(|_| panic!("{variable} must be set for integration tests"));
    let mut url = Url::parse(&raw).unwrap_or_else(|_| panic!("{variable} is a URL"));
    url.set_path(&database);
    url
}

/// A pool on `dsn` opened the way the service opens its own: at most
/// `max_connections` sessions named `application_name`, with `isolation` as
/// the session default and the budgets set at startup.
///
/// # Panics
///
/// When `max_connections` is zero or the pool cannot connect.
pub async fn template_pool(
    dsn: &Dsn,
    application_name: &str,
    max_connections: u32,
    isolation: Isolation,
) -> PgPool {
    infra_postgres::connect(
        dsn,
        &PoolOptions {
            max_connections: NonZeroU32::new(max_connections).expect("a pool size"),
            application_name,
            default_isolation: isolation,
            session_settings_source: SessionSettingsSource::Startup,
        },
    )
    .await
    .expect("the template pool connects")
}

/// Closes each of `pools`.
///
/// # Panics
///
/// When a pool does not close completely within five seconds: a connection
/// the test leaked would hide in a partial close.
pub async fn close_pools(pools: &[&PgPool]) {
    for pool in pools {
        assert_eq!(
            infra_postgres::close(pool, CLOSE_BUDGET).await,
            CloseOutcome::Complete
        );
    }
}
