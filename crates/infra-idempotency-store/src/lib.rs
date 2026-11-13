//! PostgreSQL record store for HTTP idempotency.
//!
//! Owns the one profile table, `http_idempotency_records`: arbitration of a
//! scoped key at the writer, the one execution transaction that commits the
//! work together with its success record, the startup check, and the bounded
//! cleanup of expired records. Its records have the shape of an HTTP success
//! (a 2xx status, header pairs, a body), so it serves no other transport, but
//! it applies no HTTP policy: the inbound seam in `infra_http::idempotency`
//! derives scopes and fingerprints, captures and decodes stored successes,
//! and maps [`Attempted`] to responses.
//!
//! No other production crate names the table; the integration tests seed and
//! corrupt it directly. A feature's persistence adapter reaches
//! the transaction's connection through the PostgreSQL provider, which the
//! seam never re-exports.

mod attempt;
mod maintenance;

use std::time::Duration;

use infra_postgres::{Isolation, TxOptions};
use sqlx::postgres::{PgPool, types::PgInterval};

pub use attempt::{
    AttemptError, Attempted, CallerIdentity, CallerKind, Digest, HeaderPair, Record, ScopeKey,
};
pub use infra_postgres::CleanupError;
pub use maintenance::{CLEANUP_REMOVED_METRIC, CLEANUP_RUNS_METRIC, StartupError};

/// Every transaction of the store names its isolation, because the server
/// default may be stricter. Arbitration needs a new snapshot per statement
/// (see [`attempt`]), and under a stricter level the cleanup's row lock fails
/// with a serialization error on a record replaced or removed since its
/// snapshot, where this level re-checks the row.
const READ_COMMITTED: TxOptions = TxOptions {
    isolation: Isolation::ReadCommitted,
    read_only: false,
};

#[derive(Clone, Debug)]
pub struct Store {
    pool: PgPool,
    retention: PgInterval,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("retention exceeds the PostgreSQL interval range in microseconds")]
pub struct InvalidRetention;

impl Store {
    /// A store over `pool` whose records stay live for `retention` after
    /// their write, measured on the database clock.
    ///
    /// Does no I/O. Keeps whole microseconds of `retention`.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRetention`] when the whole microseconds exceed the
    /// range of a PostgreSQL interval's time component.
    pub fn new(pool: PgPool, retention: Duration) -> Result<Self, InvalidRetention> {
        let microseconds = i64::try_from(retention.as_micros()).map_err(|_| InvalidRetention)?;
        Ok(Self {
            pool,
            retention: PgInterval {
                months: 0,
                days: 0,
                microseconds,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_retention_outside_the_postgres_interval_range_is_rejected() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .expect("lazy pool does no I/O");
        let max = Duration::from_micros(i64::MAX.unsigned_abs());
        assert!(Store::new(pool.clone(), max).is_ok());
        let rejected = Store::new(pool.clone(), max + Duration::from_micros(1));
        assert_eq!(rejected.unwrap_err(), InvalidRetention);
        pool.close().await;
    }
}
