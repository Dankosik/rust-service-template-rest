//! Arbitration, the one execution transaction, and readback.
//!
//! An attempt is one explicit `READ COMMITTED` transaction on the writer.
//! Its first statement checks that the session is writable and only then
//! tries the scope's transaction-scoped advisory lock, without waiting. The
//! second reads the live record, which decides before the lock result: a
//! visible live record is replayed or refused whoever holds the key. Only an
//! attempt that sees no live record and took the key runs the work, and the
//! success record commits with the work or not at all. Every other outcome
//! is returned from the transaction as an error, so it rolls back, which
//! also releases the lock.

use std::time::Duration;

use infra_postgres::{TxError, in_tx_with, retryable};
use sqlx::Row;
use sqlx::postgres::{PgConnection, PgRow};

use crate::{READ_COMMITTED, Store};

/// Writer check and lock. A read-only or recovering session never tries the
/// lock, and `pg_try_advisory_xact_lock` never waits. `$1` is [`lock_key`].
const WRITER_CHECK_AND_LOCK: &str = "SELECT NOT pg_is_in_recovery() \
    AND current_setting('transaction_read_only') = 'off' AS writable, \
    CASE WHEN pg_is_in_recovery() OR current_setting('transaction_read_only') = 'on' \
    THEN false ELSE pg_try_advisory_xact_lock($1) END AS acquired";

/// The live record: not yet expired on the database clock.
const READ: &str = "SELECT fingerprint, format, status, headers, body \
    FROM http_idempotency_records \
    WHERE scope_key = $1 AND expires_at > statement_timestamp()";

/// The success record, after the work. It replaces only an expired record,
/// so a live one leaves zero rows affected.
const WRITE: &str = "INSERT INTO http_idempotency_records AS r \
    (scope_key, fingerprint, format, status, headers, body, expires_at) \
    VALUES ($1, $2, $3, $4, $5, $6, statement_timestamp() + $7) \
    ON CONFLICT (scope_key) DO UPDATE SET fingerprint = EXCLUDED.fingerprint, \
    format = EXCLUDED.format, status = EXCLUDED.status, headers = EXCLUDED.headers, \
    body = EXCLUDED.body, expires_at = EXCLUDED.expires_at \
    WHERE r.expires_at <= statement_timestamp()";

/// The writer check and the live record in one row, outside a transaction.
/// The record columns are null when no live record exists.
const READ_BACK: &str = "SELECT NOT pg_is_in_recovery() \
    AND current_setting('transaction_read_only') = 'off' AS writable, \
    r.fingerprint, r.format, r.status, r.headers, r.body \
    FROM (SELECT 1) AS one LEFT JOIN http_idempotency_records AS r \
    ON r.scope_key = $1 AND r.expires_at > statement_timestamp()";

/// A SHA-256 digest.
pub type Digest = [u8; 32];

/// The digest of one caller, operation, and key: the record's primary key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopeKey(Digest);

impl ScopeKey {
    /// The scope with this digest.
    #[must_use]
    pub const fn from_digest(digest: Digest) -> Self {
        Self(digest)
    }
}

/// One stored success, as the seam encodes it. The store only moves the
/// bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The fingerprint digest of the request that produced the success.
    pub fingerprint: Digest,
    /// The stored-success encoding.
    pub format: i16,
    /// The 2xx status.
    pub status: i16,
    /// The encoded replayable headers.
    pub headers: Vec<u8>,
    /// The exact body bytes.
    pub body: Vec<u8>,
}

/// What the work asks the store to do with its transaction.
#[derive(Debug)]
pub enum WorkOutput<T> {
    /// Write this success record and commit.
    Commit(Record),
    /// Roll back and hand this value back.
    Rollback(T),
}

/// The outcome of one [`Store::attempt`].
#[derive(Debug)]
pub enum Attempted<T> {
    /// The writer could not be used: acquire, begin, or statement failure,
    /// a read-only or recovering session, or an inert store. The work did
    /// not run.
    Unavailable,
    /// A live record decided the attempt; `matched` is whether its
    /// fingerprint is one of the accepted digests. The work did not run.
    Live { matched: bool, record: Record },
    /// No live record, and another attempt holds the key. The work did not
    /// run.
    InProgress,
    /// The work asked for rollback.
    RolledBack(T),
    /// Writing the success record failed, or wrote no row, before `COMMIT`
    /// was sent; the work rolled back.
    WriteFailed,
    /// The commit was acknowledged.
    Committed(Record),
    /// The server rejected the commit; `retryable` for a serialization
    /// failure or a deadlock.
    CommitRejected { retryable: bool },
    /// The commit outcome is unknown: [`Store::read_back`] can tell.
    CommitUnknown,
}

/// The outcome of one [`Store::read_back`].
#[derive(Debug)]
pub enum ReadBack {
    /// A live record; `matched` is whether its fingerprint is one of the
    /// accepted digests.
    Found { matched: bool, record: Record },
    /// No live record.
    Absent,
    /// The session is read-only or recovering, so its view decides nothing.
    NotWritable,
    /// The readback did not complete: acquire, statement, or decode
    /// failure, or an inert store.
    Failed,
}

/// The execution transaction, handed to the work.
///
/// Opaque: it has no methods and no conversion to the connection. Only
/// [`connection`] reaches the connection, and only crates that depend on
/// this store can call it.
#[derive(Debug)]
pub struct Tx<'c> {
    conn: &'c mut PgConnection,
}

/// The connection inside the execution transaction, for a feature's
/// persistence adapter. Never end the transaction with transaction-control
/// SQL, and never name the profile table.
pub fn connection<'a>(tx: &'a mut Tx<'_>) -> &'a mut PgConnection {
    tx.conn
}

impl Store {
    /// Arbitrate `scope` at the writer and, when no live record decides and
    /// the key is free, run `work` inside the one transaction that also
    /// writes its success record.
    ///
    /// A live record matches when its fingerprint is in `accepted`. `work`
    /// runs at most once; nothing is retried. The future is `Send` whenever
    /// the work's future is.
    pub async fn attempt<W, T>(
        &self,
        scope: &ScopeKey,
        accepted: &[Digest],
        work: W,
    ) -> Attempted<T>
    where
        W: AsyncFnOnce(&mut Tx<'_>) -> WorkOutput<T>,
    {
        let Some(inner) = &self.inner else {
            return Attempted::Unavailable;
        };
        let retention = inner.retention;
        let executed = in_tx_with(
            &inner.pool,
            READ_COMMITTED,
            async |conn: &mut PgConnection| -> Result<Record, Stop<T>> {
                arbitrate(conn, scope, accepted).await?;
                let mut tx = Tx { conn: &mut *conn };
                let record = match work(&mut tx).await {
                    WorkOutput::Commit(record) => record,
                    WorkOutput::Rollback(value) => {
                        return Err(Stop(Attempted::RolledBack(value)));
                    }
                };
                write(conn, scope, &record, retention).await?;
                Ok(record)
            },
        )
        .await;
        match executed {
            Ok(record) => Attempted::Committed(record),
            Err(Stop(outcome)) => outcome,
        }
    }

    /// Read the live record for `scope` on a fresh pooled connection after
    /// an unknown commit outcome. A record matches when its fingerprint is
    /// in `accepted`. Adds no bound of its own; the caller bounds the wait.
    pub async fn read_back(&self, scope: &ScopeKey, accepted: &[Digest]) -> ReadBack {
        let Some(inner) = &self.inner else {
            return ReadBack::Failed;
        };
        sqlx::query(READ_BACK)
            .bind(scope.0)
            .fetch_one(&inner.pool)
            .await
            .and_then(|row| read_back_outcome(&row, accepted))
            .unwrap_or(ReadBack::Failed)
    }
}

/// The advisory lock key of `scope`: the digest's first eight bytes, read as
/// a big-endian `i64`.
fn lock_key(scope: &ScopeKey) -> i64 {
    let mut prefix = [0; 8];
    prefix.copy_from_slice(&scope.0[..8]);
    i64::from_be_bytes(prefix)
}

/// The writer check and lock, then the read. `Ok` only when no live record
/// is visible and this transaction took the key.
async fn arbitrate<T>(
    conn: &mut PgConnection,
    scope: &ScopeKey,
    accepted: &[Digest],
) -> Result<(), Stop<T>> {
    let writer = sqlx::query(WRITER_CHECK_AND_LOCK)
        .bind(lock_key(scope))
        .fetch_one(&mut *conn)
        .await
        .and_then(|row| {
            Ok((
                row.try_get::<bool, _>("writable")?,
                row.try_get::<bool, _>("acquired")?,
            ))
        });
    let Ok((writable, acquired)) = writer else {
        return Err(Stop(Attempted::Unavailable));
    };
    if !writable {
        return Err(Stop(Attempted::Unavailable));
    }
    let live = sqlx::query(READ)
        .bind(scope.0)
        .fetch_optional(conn)
        .await
        .and_then(|row| row.as_ref().map(decode_record).transpose());
    match live {
        Err(_) => Err(Stop(Attempted::Unavailable)),
        Ok(Some(record)) => Err(Stop(Attempted::Live {
            matched: accepted.contains(&record.fingerprint),
            record,
        })),
        Ok(None) if acquired => Ok(()),
        Ok(None) => Err(Stop(Attempted::InProgress)),
    }
}

/// Write the success record. Anything but exactly one row, inserted or
/// replacing an expired record, fails the attempt.
async fn write<T>(
    conn: &mut PgConnection,
    scope: &ScopeKey,
    record: &Record,
    retention: Duration,
) -> Result<(), Stop<T>> {
    let written = sqlx::query(WRITE)
        .bind(scope.0)
        .bind(record.fingerprint)
        .bind(record.format)
        .bind(record.status)
        .bind(record.headers.as_slice())
        .bind(record.body.as_slice())
        .bind(retention)
        .execute(conn)
        .await;
    if written.is_ok_and(|done| done.rows_affected() == 1) {
        Ok(())
    } else {
        Err(Stop(Attempted::WriteFailed))
    }
}

/// The record columns of the read, or of a readback that found one.
fn decode_record(row: &PgRow) -> Result<Record, sqlx::Error> {
    Ok(Record {
        fingerprint: row.try_get("fingerprint")?,
        format: row.try_get("format")?,
        status: row.try_get("status")?,
        headers: row.try_get("headers")?,
        body: row.try_get("body")?,
    })
}

/// The readback row: the writer check decides first, then the record, if
/// the join found one.
fn read_back_outcome(row: &PgRow, accepted: &[Digest]) -> Result<ReadBack, sqlx::Error> {
    if !row.try_get::<bool, _>("writable")? {
        return Ok(ReadBack::NotWritable);
    }
    if row.try_get::<Option<Digest>, _>("fingerprint")?.is_none() {
        return Ok(ReadBack::Absent);
    }
    let record = decode_record(row)?;
    Ok(ReadBack::Found {
        matched: accepted.contains(&record.fingerprint),
        record,
    })
}

/// An attempt that ends without a written record. It leaves the transaction
/// as an error, so `in_tx_with` rolls back.
struct Stop<T>(Attempted<T>);

impl<T> From<TxError> for Stop<T> {
    fn from(err: TxError) -> Self {
        Self(classify(err))
    }
}

/// The outcome of a transaction error: before the work, the writer could not
/// be used; after it, the commit was rejected or its outcome is unknown.
fn classify<T>(err: TxError) -> Attempted<T> {
    match err {
        TxError::Acquire(_) | TxError::Begin(_) => Attempted::Unavailable,
        TxError::CommitFailed(err) => Attempted::CommitRejected {
            retryable: retryable(&err),
        },
        TxError::CommitUnknown(_) => Attempted::CommitUnknown,
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::error::Error as StdError;
    use std::fmt;

    use sqlx::error::{DatabaseError, ErrorKind};

    use super::*;

    /// The scope digest of the design's pinned vector (issuer
    /// `https://issuer.example`, subject `fixture-subject`, `createWidget`,
    /// key `k-123`).
    const PINNED_SCOPE: &str = "e48d23569e751551fdcb4683dd3e6026c6f3f177b5b79e2c7ae9f4e4e36187d4";

    fn digest(hex: &str) -> Digest {
        assert_eq!(hex.len(), 64, "{hex}");
        let mut digest = [0; 32];
        for (byte, pair) in digest.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
            *byte = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
        }
        digest
    }

    /// A server error that carries only its SQLSTATE.
    #[derive(Debug)]
    struct Sqlstate(&'static str);

    impl fmt::Display for Sqlstate {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    impl StdError for Sqlstate {}

    impl DatabaseError for Sqlstate {
        fn message(&self) -> &str {
            self.0
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.0))
        }

        fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    fn rejected(code: &'static str) -> TxError {
        TxError::CommitFailed(sqlx::Error::Database(Box::new(Sqlstate(code))))
    }

    fn reset() -> sqlx::Error {
        sqlx::Error::Io(std::io::Error::other("reset"))
    }

    fn assert_send(_: &impl Send) {}

    #[test]
    fn the_lock_key_is_the_scope_prefix_read_big_endian() {
        let scope = ScopeKey::from_digest(digest(PINNED_SCOPE));
        assert_eq!(lock_key(&scope), -1_977_885_806_413_146_799);
    }

    #[test]
    fn transaction_errors_classify_into_attempt_outcomes() {
        for err in [
            TxError::Acquire(sqlx::Error::PoolTimedOut),
            TxError::Begin(reset()),
        ] {
            assert!(matches!(classify::<()>(err), Attempted::Unavailable));
        }
        for code in ["40001", "40P01"] {
            assert!(
                matches!(
                    classify::<()>(rejected(code)),
                    Attempted::CommitRejected { retryable: true }
                ),
                "{code}"
            );
        }
        assert!(matches!(
            classify::<()>(rejected("23505")),
            Attempted::CommitRejected { retryable: false }
        ));
        assert!(matches!(
            classify::<()>(TxError::CommitUnknown(reset())),
            Attempted::CommitUnknown
        ));
    }

    #[test]
    fn an_attempt_is_send_when_its_work_is() {
        let store = Store::inert();
        let scope = ScopeKey::from_digest(digest(PINNED_SCOPE));
        let attempt = store.attempt(&scope, &[], async |tx: &mut Tx<'_>| {
            let ran = sqlx::query("SELECT 1").execute(connection(tx)).await;
            WorkOutput::Rollback(ran.is_ok())
        });
        assert_send(&attempt);
    }
}
