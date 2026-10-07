//! Helpers shared by the database-backed tests under `tests/`.
//!
//! `#[sqlx::test]` hands each test its own database created from
//! `DATABASE_URL`; the template's own entry points take an admitted
//! [`Dsn`], so the helpers here turn one into the other.

// A helper that cannot do its job must fail the test that called it; a
// `Result` here would only move the same panic into every test body.
#![allow(clippy::expect_used, clippy::panic)]

// template:begin jobs:test-lib-jobs-module
pub mod jobs;
// template:end jobs:test-lib-jobs-module

// template:begin jobs-reference:test-lib-reading-counter
pub mod reading_counter;
pub mod reading_counter_receiver;
// template:end jobs-reference:test-lib-reading-counter

use infra_postgres::Dsn;
use sqlx::PgPool;
use url::Url;

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
