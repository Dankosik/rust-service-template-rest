mod attempt;
mod maintenance;
mod record;

use std::time::Duration;

use infra_postgres::{Isolation, TxOptions};
use sqlx::postgres::PgPool;

pub use attempt::{
    AttemptError, Attempted, CallerIdentity, CallerKind, Digest, RequestFingerprint, ScopeKey,
};
pub use infra_postgres::CleanupError;
pub use maintenance::{CLEANUP_REMOVED_METRIC, CLEANUP_RUNS_METRIC, StartupError};
pub use record::{CaptureError, MAX_STORED_BODY_BYTES, Record, is_replayable_header_name};

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
    terminal_refusals: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("retention must be between 1 microsecond and {MAX_RETENTION_DAYS} days")]
pub struct InvalidRetention;

impl Store {
    pub fn new(pool: PgPool, retention: Duration) -> Result<Self, InvalidRetention> {
        if !(Duration::from_micros(1)..=MAX_RETENTION).contains(&retention) {
            return Err(InvalidRetention);
        }
        Ok(Self {
            pool,
            retention: Duration::new(retention.as_secs(), retention.subsec_micros() * 1_000),
            terminal_refusals: false,
        })
    }

    #[must_use]
    pub fn with_terminal_refusals(mut self, enabled: bool) -> Self {
        self.terminal_refusals = enabled;
        self
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
        assert!(Store::new(pool.clone(), Duration::from_micros(1)).is_ok());
        for invalid in [
            Duration::ZERO,
            Duration::from_nanos(999),
            max + Duration::from_nanos(1),
            Duration::from_micros(i64::MAX.unsigned_abs()),
            Duration::from_secs(10_000_000_000_000),
        ] {
            let rejected = Store::new(pool.clone(), invalid);
            assert_eq!(rejected.unwrap_err(), InvalidRetention);
        }
        pool.close().await;
    }
}
