mod attempt;
mod maintenance;

use std::time::Duration;

use infra_postgres::{Isolation, TxOptions};
use sqlx::postgres::PgPool;

pub use attempt::{
    AttemptError, Attempted, CallerIdentity, CallerKind, Digest, HeaderPair, Record, ScopeKey,
};
pub use infra_postgres::CleanupError;
pub use maintenance::{CLEANUP_REMOVED_METRIC, CLEANUP_RUNS_METRIC, StartupError};

const READ_COMMITTED: TxOptions = TxOptions {
    isolation: Isolation::ReadCommitted,
    read_only: false,
};

const MAX_RETENTION_DAYS: u64 = 36_500;
const MAX_RETENTION: Duration = Duration::from_hours(MAX_RETENTION_DAYS * 24);

#[derive(Clone, Debug)]
pub struct Store {
    pool: PgPool,
    retention: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("retention exceeds the maximum of {MAX_RETENTION_DAYS} days")]
pub struct InvalidRetention;

impl Store {
    pub fn new(pool: PgPool, retention: Duration) -> Result<Self, InvalidRetention> {
        if retention > MAX_RETENTION {
            return Err(InvalidRetention);
        }
        Ok(Self {
            pool,
            retention: Duration::new(retention.as_secs(), retention.subsec_micros() * 1_000),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retention_is_bounded_before_the_store_can_run_work() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .expect("lazy pool does no I/O");
        let max = Duration::from_hours(36_500 * 24);
        assert!(Store::new(pool.clone(), max).is_ok());
        for too_long in [
            max + Duration::from_nanos(1),
            Duration::from_micros(i64::MAX.unsigned_abs()),
            Duration::from_secs(10_000_000_000_000),
        ] {
            let rejected = Store::new(pool.clone(), too_long);
            assert_eq!(rejected.unwrap_err(), InvalidRetention);
        }
        pool.close().await;
    }
}
