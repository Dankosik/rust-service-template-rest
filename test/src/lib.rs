// template:begin postgres:test-lib-allows
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a helper that cannot do its job fails the calling test; a `Result` would move the same panic into every test body"
)]

// template:end postgres:test-lib-allows
// template:begin jobs:test-lib-jobs-module
pub mod jobs;

// template:end jobs:test-lib-jobs-module
// template:begin jobs-reference:test-lib-reading-counter
pub mod reading_counter;

// template:end jobs-reference:test-lib-reading-counter
// template:begin postgres:test-lib-postgres-helpers
use std::num::NonZeroU32;
use std::time::Duration;

use infra_postgres::{CloseOutcome, Dsn, Isolation, Pool, PoolOptions, SessionSettingsSource};
use sqlx::PgPool;
use url::Url;

const CLOSE_BUDGET: Duration = Duration::from_secs(5);

pub const DATABASE_URL: &str = "DATABASE_URL";

pub const PGBOUNCER_DATABASE_URL: &str = "PGBOUNCER_DATABASE_URL";

#[must_use]
pub fn fixture_dir(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("migrations")
        .join(name)
}

pub async fn dsn_for(pool: &PgPool) -> Dsn {
    Dsn::admit_for_environment(url_for(pool, DATABASE_URL).await.as_str(), None, true)
        .expect("DATABASE_URL must be an admitted DSN")
}

pub async fn pooler_dsn_for(pool: &PgPool) -> Dsn {
    Dsn::admit_for_environment(
        url_for(pool, PGBOUNCER_DATABASE_URL).await.as_str(),
        None,
        true,
    )
    .expect("PGBOUNCER_DATABASE_URL must be an admitted DSN")
}

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

pub async fn template_pool(
    dsn: &Dsn,
    application_name: &str,
    max_connections: u32,
    isolation: Isolation,
) -> Pool {
    Pool::connect(
        dsn,
        &PoolOptions {
            max_connections: NonZeroU32::new(max_connections).expect("a pool size"),
            application_name,
            default_isolation: isolation,
            session_settings_source: SessionSettingsSource::Startup,
            wait_order: infra_postgres::WaitOrder::Arrival,
            notifications: true,
        },
    )
    .await
    .expect("the template pool connects")
}

pub async fn close_pools(pools: &[&Pool]) {
    for pool in pools {
        assert_eq!(pool.close(CLOSE_BUDGET).await, CloseOutcome::Complete);
    }
}

pub fn propagate_join_panic(result: Result<(), tokio::task::JoinError>) {
    if let Err(error) = result
        && let Ok(payload) = error.try_into_panic()
    {
        std::panic::resume_unwind(payload);
    }
}

pub async fn poll_until<T>(
    what: &str,
    bound: Duration,
    interval: Duration,
    mut ready: impl AsyncFnMut() -> Option<T>,
) -> T {
    tokio::time::timeout(bound, async {
        loop {
            if let Some(found) = ready().await {
                return found;
            }
            tokio::time::sleep(interval).await;
        }
    })
    .await
    .unwrap_or_else(|elapsed| panic!("{what} did not happen within {bound:?}: {elapsed}"))
}
// template:end postgres:test-lib-postgres-helpers
