//! PostgreSQL record store for HTTP idempotency.
//!
//! Owns the one profile table, `http_idempotency_records`: arbitration of a
//! scoped key at the writer, the one execution transaction that commits the
//! work together with its success record, the readback after an unknown
//! commit outcome, the startup check, and the bounded cleanup of expired
//! records. It knows nothing about HTTP: the inbound seam in
//! `infra_http::idempotency` derives scopes and fingerprints, captures and
//! decodes stored successes, and maps [`Attempted`] to responses.
//!
//! No other crate names the table. A feature's persistence adapter reaches
//! the transaction's connection only through [`connection`], which the seam
//! never re-exports.

mod attempt;
mod maintenance;

use std::sync::Arc;
use std::time::Duration;

use infra_postgres::{Isolation, TxOptions};
use sqlx::postgres::PgPool;

pub use attempt::{Attempted, Digest, ReadBack, Record, ScopeKey, Tx, WorkOutput, connection};
pub use maintenance::{CleanupError, StartupError};

/// Every transaction that checks the writer: explicit read committed and no
/// access mode, so `transaction_read_only` reports the session's default
/// rather than a mode the store chose.
const READ_COMMITTED: TxOptions = TxOptions {
    isolation: Isolation::ReadCommitted,
    read_only: false,
};

/// Handle on the record store. Cloning shares the pool.
///
/// An inert store ([`Store::inert`]) holds no pool and never does I/O.
#[derive(Clone, Debug)]
pub struct Store {
    inner: Option<Arc<Inner>>,
}

#[derive(Debug)]
struct Inner {
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
            inner: Some(Arc::new(Inner {
                pool,
                retention: whole_micros(retention),
            })),
        }
    }

    /// A store without a pool: every operation reports unavailability and
    /// performs no I/O.
    #[must_use]
    pub fn inert() -> Self {
        Self { inner: None }
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
