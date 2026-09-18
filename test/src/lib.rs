//! Helpers shared by the database-backed tests under `tests/`.
//!
//! `#[sqlx::test]` hands each test its own database created from
//! `DATABASE_URL`; the template's own entry points take an admitted
//! [`Dsn`], so the helpers here turn one into the other.

// A helper that cannot do its job must fail the test that called it; a
// `Result` here would only move the same panic into every test body.
#![allow(clippy::expect_used)]

use infra_postgres::Dsn;
use sqlx::PgPool;
use url::Url;

/// The environment variable `#[sqlx::test]` reads; the script that owns the
/// compose lifecycle exports it in the admitted URL form.
pub const DATABASE_URL: &str = "DATABASE_URL";

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
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await
        .expect("current_database()");
    let raw = std::env::var(DATABASE_URL).expect("DATABASE_URL must be set for integration tests");
    let mut url = Url::parse(&raw).expect("DATABASE_URL is a URL");
    url.set_path(&database);
    Dsn::parse(url.as_str()).expect("DATABASE_URL must be an admitted DSN")
}
