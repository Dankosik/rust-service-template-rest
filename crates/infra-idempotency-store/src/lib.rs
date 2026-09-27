//! PostgreSQL record store for HTTP idempotency.
//!
//! Owns the one profile table, `http_idempotency_records`: arbitration of a
//! scoped key at the writer, the one execution transaction that commits the
//! work together with its success record, the startup check, and the bounded
//! cleanup of expired records. It knows nothing about HTTP: the inbound seam
//! in `infra_http::idempotency` derives scopes and fingerprints, captures and
//! decodes stored successes, and maps [`Attempted`] to responses.
//!
//! No other crate names the table. A feature's persistence adapter reaches
//! the transaction's connection through the PostgreSQL provider, which the
//! seam never re-exports.

mod attempt;
mod maintenance;

use std::time::Duration;

use sqlx::postgres::PgPool;

pub use attempt::{
    AttemptError, Attempted, CallerIdentity, CallerKind, Digest, HeaderPair, Record, ScopeKey,
};
pub use maintenance::{CleanupError, StartupError};

/// Handle on the record store. Cloning shares the pool.
#[derive(Clone, Debug)]
pub struct Store {
    pool: PgPool,
    /// Whole microseconds ([`whole_micros`]).
    retention: Duration,
}

impl Store {
    /// A store over `pool` whose records stay live for `retention` after
    /// their write, measured on the database clock.
    ///
    /// Does no I/O. Keeps whole microseconds of `retention`.
    #[must_use]
    pub fn new(pool: PgPool, retention: Duration) -> Self {
        Self {
            pool,
            retention: whole_micros(retention),
        }
    }
}

/// `duration` without its sub-microsecond part. `sqlx-postgres` refuses to
/// bind a `Duration` with one as an `interval`, and configuration accepts
/// nanosecond units; the database clock resolves 1 µs, so dropping the part
/// once changes no expiry observably.
fn whole_micros(duration: Duration) -> Duration {
    Duration::new(duration.as_secs(), duration.subsec_micros() * 1_000)
}

#[cfg(test)]
mod tests {
    use sqlx::postgres::types::PgInterval;

    use super::*;

    #[test]
    fn retention_keeps_whole_microseconds() {
        let kept = whole_micros(Duration::new(3_600, 123_456_789));
        assert_eq!(kept, Duration::new(3_600, 123_456_000));
        assert!(PgInterval::try_from(kept).is_ok());

        assert_eq!(
            whole_micros(Duration::from_secs(60)),
            Duration::from_secs(60)
        );
        assert_eq!(
            whole_micros(Duration::new(59, 999_999_999)),
            Duration::new(59, 999_999_000)
        );
        assert_eq!(whole_micros(Duration::from_nanos(999)), Duration::ZERO);
    }
}
