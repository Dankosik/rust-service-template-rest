//! Arbitration and the record-writing transaction.
//!
//! An attempt holds one explicit `READ COMMITTED` transaction:
//!
//! 1. One statement checks the writer, takes the scope's transaction-scoped
//!    advisory lock without waiting, and reads a live record. A live record
//!    decides whatever the lock returned: equal fingerprints replay, different
//!    ones mismatch. No record and no lock means another attempt holds the
//!    scope.
//! 2. With the lock and no record, a second statement reads again.
//! 3. Only a record still missing in step 2 runs the work, then writes the
//!    success record in the same transaction.
//!
//! Correctness rests on two PostgreSQL facts. A commit becomes visible before
//! its transaction releases its locks, and under `READ COMMITTED` every
//! statement takes a new snapshot. Step 1 takes its snapshot before its lock,
//! so it can miss the record of a writer that released the lock in between;
//! step 2 takes its snapshot after the lock and sees that record. Skipping
//! step 2, or running under `REPEATABLE READ`, would let a duplicate run the
//! work a second time. A decision in step 1 alone runs no work, and a live
//! record is never replaced, so it needs no second read.

use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use infra_postgres::{
    Isolation, Tx, TxError, TxOptions, failure_cause, in_tx_with, sqlstate, transient,
};
use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::Store;

/// Explicit, because the server default may be stricter: see the module
/// documentation.
const READ_COMMITTED: TxOptions = TxOptions {
    isolation: Isolation::ReadCommitted,
    read_only: false,
};

/// Step 1. The lock is taken after the statement's snapshot, so a missing
/// record with the lock still needs step 2. Advisory locks work on a standby
/// too; a lock taken there ends with the refused transaction.
const LOCK_AND_READ: &str = "SELECT \
    NOT pg_is_in_recovery() AND current_setting('transaction_read_only') = 'off' AS writable, \
    pg_try_advisory_xact_lock($1) AS acquired, \
    r.fingerprint, r.status, r.headers, \
    CASE WHEN r.fingerprint = $3 THEN r.body END AS body \
    FROM (VALUES (1)) AS one LEFT JOIN http_idempotency_records AS r \
    ON r.scope_key = $2 AND r.expires_at > statement_timestamp()";

/// Step 2. It must stay a statement of its own after step 1.
const READ: &str = "SELECT fingerprint, status, headers, \
    CASE WHEN fingerprint = $2 THEN body END AS body \
    FROM http_idempotency_records \
    WHERE scope_key = $1 AND expires_at > statement_timestamp()";

/// Step 3. Replaces only an expired row; a live row leaves no row affected.
const WRITE: &str = "INSERT INTO http_idempotency_records AS r \
    (scope_key, fingerprint, status, headers, body, issuer, caller_kind, caller_value, expires_at) \
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, statement_timestamp() + $9) \
    ON CONFLICT (scope_key) DO UPDATE SET fingerprint = EXCLUDED.fingerprint, \
    status = EXCLUDED.status, headers = EXCLUDED.headers, body = EXCLUDED.body, \
    issuer = EXCLUDED.issuer, caller_kind = EXCLUDED.caller_kind, \
    caller_value = EXCLUDED.caller_value, expires_at = EXCLUDED.expires_at \
    WHERE r.expires_at <= statement_timestamp()";

/// A SHA-256 digest.
pub type Digest = [u8; 32];

/// The durable primary key for a caller and decoded idempotency key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopeKey(Digest);

impl ScopeKey {
    /// The scope with this digest.
    #[must_use]
    pub const fn from_digest(digest: Digest) -> Self {
        Self(digest)
    }

    /// The digest retained for diagnostics that never render its bytes.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.0
    }

    /// The advisory lock key: the digest's first eight bytes as a signed
    /// big-endian integer. A collision only serializes two scopes.
    fn lock_key(&self) -> i64 {
        let mut prefix = [0; 8];
        prefix.copy_from_slice(&self.0[..8]);
        i64::from_be_bytes(prefix)
    }
}

/// The verified identity form used to scope a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallerKind {
    Subject,
    Client,
}

impl CallerKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Client => "client",
        }
    }
}

/// Verified caller metadata persisted with a success record.
#[derive(Clone, PartialEq, Eq)]
pub struct CallerIdentity {
    pub issuer: String,
    pub kind: CallerKind,
    pub value: String,
}

impl fmt::Debug for CallerIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CallerIdentity")
            .field("issuer", &"[REDACTED]")
            .field("kind", &self.kind)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

/// One lossless replayable response header.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "http_idempotency_header_pair")]
pub struct HeaderPair {
    pub name: String,
    pub value: Vec<u8>,
}

/// One stored success. The store moves values without applying HTTP policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub fingerprint: Digest,
    pub status: i16,
    pub headers: Vec<HeaderPair>,
    pub body: Bytes,
}

/// How [`Store::attempt`] decided.
#[derive(Debug)]
pub enum Attempted<C, R> {
    /// The work ran and committed together with its success record.
    Committed(C),
    /// The work ran and asked for rollback.
    RolledBack(R),
    /// A live record for the same request decided. The work did not run.
    Replay(Record),
    /// A live record for a different request decided. The work did not run.
    Mismatch,
    /// Another transaction holds the scope. The work did not run.
    InProgress,
}

/// Why [`Store::attempt`] could not decide. Nothing it ran committed, except
/// perhaps after [`AttemptError::Unavailable`] for an uncertain commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AttemptError {
    /// A transient fault or an uncertain commit. A retry with the identical
    /// request and key reaches normal arbitration again.
    #[error("unavailable")]
    Unavailable,
    /// A known database, query, or record-write fault.
    #[error("internal")]
    Internal,
    /// A stored record could not be decoded.
    #[error("integrity")]
    Integrity,
}

impl Store {
    /// Arbitrate `scope`, then run `work` in the transaction that also writes
    /// its success record. `Ok((record, value))` commits both and yields
    /// [`Attempted::Committed`] with `value`; `Err(value)` rolls back and
    /// yields [`Attempted::RolledBack`]. Work is never retried.
    ///
    /// # Errors
    ///
    /// [`AttemptError`] when the store could not decide; each is logged with
    /// its phase and a bounded cause.
    pub async fn attempt<W, C, R>(
        &self,
        scope: &ScopeKey,
        caller: &CallerIdentity,
        fingerprint: &Digest,
        work: W,
    ) -> Result<Attempted<C, R>, AttemptError>
    where
        W: AsyncFnOnce(&mut Tx<'_>) -> Result<(Record, C), R>,
    {
        let committed = in_tx_with(
            &self.pool,
            READ_COMMITTED,
            async |tx: &mut Tx<'_>| -> Result<C, Stop<C, R>> {
                if let Some(decided) = arbitrate(tx, scope, fingerprint).await? {
                    return Err(Stop(Ok(decided)));
                }
                let (record, value) = match work(tx).await {
                    Ok(done) => done,
                    Err(rollback) => return Err(Stop(Ok(Attempted::RolledBack(rollback)))),
                };
                write(tx, scope, caller, &record, self.retention).await?;
                Ok(value)
            },
        )
        .await;
        match committed {
            Ok(value) => Ok(Attempted::Committed(value)),
            Err(Stop(stopped)) => stopped,
        }
    }
}

/// Steps 1 and 2: `None` when the work may run.
async fn arbitrate<C, R>(
    tx: &mut Tx<'_>,
    scope: &ScopeKey,
    fingerprint: &Digest,
) -> Result<Option<Attempted<C, R>>, AttemptError> {
    let (writable, acquired, live): (bool, bool, _) = sqlx::query(LOCK_AND_READ)
        .bind(scope.lock_key())
        .bind(scope.0)
        .bind(fingerprint.as_slice())
        .try_map(|row: PgRow| {
            Ok((
                row.try_get("writable")?,
                row.try_get("acquired")?,
                live_record(&row, fingerprint)?,
            ))
        })
        .fetch_one(&mut *tx)
        .await
        .map_err(|err| read_failed(&err, "arbitrate"))?;
    if !writable {
        tracing::warn!(
            phase = "writer_check",
            failure_class = %AttemptError::Unavailable,
            cause = "not_writable",
            "http_idempotency_store_failed"
        );
        return Err(AttemptError::Unavailable);
    }
    let live = match live {
        Some(live) => Some(live),
        None if !acquired => return Ok(Some(Attempted::InProgress)),
        None => sqlx::query(READ)
            .bind(scope.0)
            .bind(fingerprint.as_slice())
            .try_map(|row: PgRow| live_record(&row, fingerprint))
            .fetch_optional(tx)
            .await
            .map_err(|err| read_failed(&err, "read_record"))?
            .flatten(),
    };
    Ok(live.map(|live| match live {
        Live::Same(record) => Attempted::Replay(record),
        Live::Different => Attempted::Mismatch,
    }))
}

/// A live record, by whether it was written for the same request.
enum Live {
    Same(Record),
    Different,
}

/// The live record in `row`, if any. Every column but the body is validated
/// before mismatch; the statement returns the body only for replay, so a
/// mismatch neither detoasts nor transfers it.
fn live_record(row: &PgRow, fingerprint: &Digest) -> Result<Option<Live>, sqlx::Error> {
    let Some(stored_fingerprint) = row.try_get::<Option<Digest>, _>("fingerprint")? else {
        return Ok(None);
    };
    let status: i16 = row.try_get("status")?;
    let headers: Vec<HeaderPair> = row.try_get("headers")?;
    if stored_fingerprint != *fingerprint {
        return Ok(Some(Live::Different));
    }
    let body: &[u8] = row.try_get("body")?;
    Ok(Some(Live::Same(Record {
        fingerprint: stored_fingerprint,
        status,
        headers,
        body: Bytes::copy_from_slice(body),
    })))
}

fn read_failed(err: &sqlx::Error, phase: &'static str) -> AttemptError {
    match err {
        sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => {
            failed(err, "decode_record", AttemptError::Integrity)
        }
        _ => failed(err, phase, classify(err)),
    }
}

/// Step 3.
async fn write(
    tx: &mut Tx<'_>,
    scope: &ScopeKey,
    caller: &CallerIdentity,
    record: &Record,
    retention: Duration,
) -> Result<(), AttemptError> {
    let written = sqlx::query(WRITE)
        .bind(scope.0)
        .bind(record.fingerprint)
        .bind(record.status)
        .bind(&record.headers)
        .bind(record.body.as_ref())
        .bind(&caller.issuer)
        .bind(caller.kind.as_str())
        .bind(&caller.value)
        .bind(retention)
        .execute(tx)
        .await
        .map_err(|err| failed(&err, "write_record", classify(&err)))?;
    if written.rows_affected() == 1 {
        Ok(())
    } else {
        tracing::warn!(
            phase = "write_record",
            failure_class = %AttemptError::Internal,
            cause = "record_not_written",
            "http_idempotency_store_failed"
        );
        Err(AttemptError::Internal)
    }
}

/// Ends the transaction without a commit, carrying what the attempt returns:
/// a decision that ran no work or rolled it back, or a failure.
struct Stop<C, R>(Result<Attempted<C, R>, AttemptError>);

impl<C, R> From<AttemptError> for Stop<C, R> {
    fn from(err: AttemptError) -> Self {
        Self(Err(err))
    }
}

impl<C, R> From<TxError> for Stop<C, R> {
    fn from(err: TxError) -> Self {
        Self(Err(classify_tx(&err)))
    }
}

fn classify_tx(err: &TxError) -> AttemptError {
    match err {
        TxError::Acquire(err) => failed(err, "acquire", classify(err)),
        TxError::Begin(err) => failed(err, "begin", classify(err)),
        TxError::CommitFailed(err) => failed(err, "commit", classify(err)),
        // The server may have committed: a known programming or data fault
        // stays internal, anything else is uncertain and so retryable.
        TxError::CommitUnknown(err) => {
            let uncertain = transient(err)
                || (sqlstate(err).is_none()
                    && matches!(err, sqlx::Error::Database(_) | sqlx::Error::WorkerCrashed));
            let class = if uncertain {
                AttemptError::Unavailable
            } else {
                AttemptError::Internal
            };
            failed(err, "commit", class)
        }
    }
}

fn classify(err: &sqlx::Error) -> AttemptError {
    if transient(err) {
        AttemptError::Unavailable
    } else {
        AttemptError::Internal
    }
}

/// Log a failure with only bounded fields, never driver text, and return its
/// class.
fn failed(err: &sqlx::Error, phase: &'static str, class: AttemptError) -> AttemptError {
    tracing::warn!(
        phase,
        failure_class = %class,
        sqlstate = sqlstate(err).as_deref(),
        cause = failure_cause(err),
        "http_idempotency_store_failed"
    );
    class
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_commit_is_unavailable_unless_the_fault_is_known() {
        for err in [
            sqlx::Error::Io(std::io::Error::other("connection reset")),
            sqlx::Error::WorkerCrashed,
        ] {
            assert_eq!(
                classify_tx(&TxError::CommitUnknown(err)),
                AttemptError::Unavailable
            );
        }
        assert_eq!(
            classify_tx(&TxError::CommitUnknown(sqlx::Error::Protocol(
                "driver misuse".to_owned()
            ))),
            AttemptError::Internal
        );
        assert_eq!(
            classify_tx(&TxError::Begin(sqlx::Error::WorkerCrashed)),
            AttemptError::Internal
        );
    }

    #[test]
    fn caller_debug_redacts_verified_values() {
        let caller = CallerIdentity {
            issuer: "https://issuer.example".into(),
            kind: CallerKind::Subject,
            value: "secret-subject".into(),
        };
        let debug = format!("{caller:?}");
        assert!(!debug.contains("issuer.example"));
        assert!(!debug.contains("secret-subject"));
    }
}
