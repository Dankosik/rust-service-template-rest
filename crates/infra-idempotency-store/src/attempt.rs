//! Arbitration and the record-writing transaction.
//!
//! An attempt holds one explicit `READ COMMITTED` transaction:
//!
//! 1. One statement checks the writer and reads a live record. Equal
//!    fingerprints replay, and different ones mismatch. Only a miss tries
//!    to take the scope's transaction-scoped advisory lock without waiting.
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
    Tx, TxError, failure_cause, in_tx_with, in_tx_with_acquire_deadline, is_transient, observed,
    sqlstate,
};

use crate::{READ_COMMITTED, Store};

pub type Digest = [u8; 32];

/// The durable primary key for a caller and decoded idempotency key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopeKey(Digest);

impl ScopeKey {
    #[must_use]
    pub const fn from_digest(digest: Digest) -> Self {
        Self(digest)
    }

    /// The digest as a non-secret scope identifier for diagnostics; it never
    /// reveals the raw key or caller.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.0
    }

    /// The advisory lock key: the digest's first eight bytes as a signed
    /// big-endian integer. A collision yields `InProgress` only when another
    /// scope holds the lock and arbitration finds no live record for this
    /// scope. A live record still decides replay or mismatch.
    fn lock_key(&self) -> i64 {
        let [b0, b1, b2, b3, b4, b5, b6, b7, ..] = self.0;
        i64::from_be_bytes([b0, b1, b2, b3, b4, b5, b6, b7])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallerKind {
    Subject,
    Client,
}

impl CallerKind {
    /// The stored `caller_kind`. The column's `CHECK` constraint in
    /// `migrations/20260923000001_create_http_idempotency_records.sql` admits
    /// only these strings, so a new variant needs a migration first.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Client => "client",
        }
    }
}

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

#[derive(Clone, Debug, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "http_idempotency_header_pair")]
pub struct HeaderPair {
    pub name: String,
    pub value: Vec<u8>,
}

/// One stored success. The store finds the row by [`ScopeKey`] and compares
/// its fingerprint with the request's to decide replay or mismatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
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
    /// No live record was found and another transaction holds the lock key.
    InProgress,
}

/// Why [`Store::attempt`] could not decide. Nothing it ran committed, except
/// perhaps after [`AttemptError::Unavailable`] for an uncertain commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AttemptError {
    /// A transient fault, an uncertain commit, or a session that cannot
    /// write. A retry with the identical request and key reaches normal
    /// arbitration again.
    #[error("unavailable")]
    Unavailable,
    /// A non-transient driver fault, a success record the write did not
    /// store, or a caller identity PostgreSQL cannot store. Nothing committed.
    #[error("internal")]
    Internal,
    /// A live record that does not decode. Replay is impossible until the
    /// record expires or an operator removes it.
    #[error("integrity")]
    Integrity,
}

impl Store {
    /// Arbitrate `scope`, then run `work` in the transaction that also writes
    /// its success record. `Ok((record, value))` commits both and yields
    /// [`Attempted::Committed`] with `value`; `Err(value)` rolls back and
    /// yields [`Attempted::RolledBack`]. Work is never retried.
    ///
    /// Connection acquisition uses the pool's configured timeout.
    ///
    /// The store writes the success record under `scope`. It saves
    /// `fingerprint` for later arbitration and `caller` for diagnostics.
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
        self.attempt_in_tx(scope, caller, fingerprint, None, work)
            .await
    }

    /// [`Self::attempt`] for a request with its own deadline: the wait for a
    /// pooled connection is bounded by `deadline`. That deadline applies
    /// only to connection acquisition.
    ///
    /// # Errors
    ///
    /// As [`Self::attempt`].
    pub async fn attempt_with_acquire_deadline<W, C, R>(
        &self,
        scope: &ScopeKey,
        caller: &CallerIdentity,
        fingerprint: &Digest,
        deadline: tokio::time::Instant,
        work: W,
    ) -> Result<Attempted<C, R>, AttemptError>
    where
        W: AsyncFnOnce(&mut Tx<'_>) -> Result<(Record, C), R>,
    {
        self.attempt_in_tx(scope, caller, fingerprint, Some(deadline), work)
            .await
    }

    async fn attempt_in_tx<W, C, R>(
        &self,
        scope: &ScopeKey,
        caller: &CallerIdentity,
        fingerprint: &Digest,
        deadline: Option<tokio::time::Instant>,
        work: W,
    ) -> Result<Attempted<C, R>, AttemptError>
    where
        W: AsyncFnOnce(&mut Tx<'_>) -> Result<(Record, C), R>,
    {
        // PostgreSQL `text` cannot hold U+0000, so the success record of such a
        // caller can never be written. Refusing before the transaction keeps
        // the work from running only to be rolled back on every attempt.
        if caller.issuer.contains('\0') || caller.value.contains('\0') {
            return Err(refused(
                "caller_identity",
                AttemptError::Internal,
                "unstorable_caller",
            ));
        }
        let work_in_tx = async |tx: &mut Tx<'_>| -> Result<C, Stop<R>> {
            if let Some(decision) = arbitrate(tx, scope, fingerprint).await? {
                return Err(Stop::Decided(decision));
            }
            let (record, value) = work(tx).await.map_err(Stop::RolledBack)?;
            write(tx, scope, caller, fingerprint, &record, self.retention).await?;
            Ok(value)
        };
        let outcome = match deadline {
            Some(deadline) => {
                in_tx_with_acquire_deadline(&self.pool, deadline, READ_COMMITTED, work_in_tx).await
            }
            None => in_tx_with(&self.pool, READ_COMMITTED, work_in_tx).await,
        };
        match outcome {
            Ok(value) => Ok(Attempted::Committed(value)),
            Err(Stop::Decided(decision)) => Ok(match decision {
                Decision::Replay(record) => Attempted::Replay(record),
                Decision::Mismatch => Attempted::Mismatch,
                Decision::InProgress => Attempted::InProgress,
            }),
            Err(Stop::RolledBack(rollback)) => Ok(Attempted::RolledBack(rollback)),
            Err(Stop::Failed(err)) => Err(err),
        }
    }
}

/// What arbitration decided without running the work.
enum Decision {
    Replay(Record),
    Mismatch,
    InProgress,
}

/// Steps 1 and 2: [`None`] when the work may run.
async fn arbitrate(
    tx: &mut Tx<'_>,
    scope: &ScopeKey,
    fingerprint: &Digest,
) -> Result<Option<Decision>, AttemptError> {
    // Step 1. Advisory locks work on a standby too; a lock taken there ends
    // with the refused transaction.
    let row = observed(
        "lock and read idempotency record",
        sqlx::query!(
            "SELECT \
             NOT pg_is_in_recovery() AND current_setting('transaction_read_only') = 'off' \
                 AS \"writable!\", \
             CASE WHEN r.scope_key IS NULL THEN pg_try_advisory_xact_lock($1) \
                  ELSE FALSE END AS \"acquired!\", \
             r.fingerprint AS \"fingerprint?: Digest\", r.status AS \"status?\", \
             r.headers AS \"headers?: Vec<HeaderPair>\", \
             CASE WHEN r.fingerprint = $3 THEN r.body END AS body \
             FROM (VALUES (1)) AS one LEFT JOIN http_idempotency_records AS r \
             ON r.scope_key = $2 AND r.expires_at > statement_timestamp()",
            scope.lock_key(),
            scope.0.as_slice(),
            fingerprint.as_slice(),
        )
        .fetch_one(&mut *tx),
    )
    .await
    .map_err(|err| read_failed(&err, "arbitrate"))?;
    // The join returns the fingerprint, status, and headers together.
    // The body is selected separately, only for a matching fingerprint.
    let live = match (row.fingerprint, row.status, row.headers) {
        (Some(stored_fingerprint), Some(status), Some(headers)) => Some(decide_live(
            stored_fingerprint,
            status,
            headers,
            row.body,
            fingerprint,
        )?),
        _ => None,
    };
    if !row.writable {
        return Err(refused(
            "writer_check",
            AttemptError::Unavailable,
            "not_writable",
        ));
    }
    match live {
        Some(decision) => Ok(Some(decision)),
        None if !row.acquired => Ok(Some(Decision::InProgress)),
        // Step 2. It must stay a statement of its own after step 1.
        None => {
            let row = observed(
                "read idempotency record",
                sqlx::query!(
                    "SELECT fingerprint AS \"fingerprint: Digest\", status, \
                     headers AS \"headers: Vec<HeaderPair>\", \
                     CASE WHEN fingerprint = $2 THEN body END AS body \
                     FROM http_idempotency_records \
                     WHERE scope_key = $1 AND expires_at > statement_timestamp()",
                    scope.0.as_slice(),
                    fingerprint.as_slice(),
                )
                .fetch_optional(tx),
            )
            .await
            .map_err(|err| read_failed(&err, "read_record"))?;
            row.map(|row| {
                decide_live(
                    row.fingerprint,
                    row.status,
                    row.headers,
                    row.body,
                    fingerprint,
                )
            })
            .transpose()
        }
    }
}

/// A live record, compared with the request's fingerprint. Both reads return
/// the body only for an equal fingerprint, so a mismatch neither detoasts nor
/// transfers it; a replay whose body is missing is a corrupt record.
fn decide_live(
    stored_fingerprint: Digest,
    status: i16,
    headers: Vec<HeaderPair>,
    body: Option<Vec<u8>>,
    fingerprint: &Digest,
) -> Result<Decision, AttemptError> {
    if stored_fingerprint != *fingerprint {
        return Ok(Decision::Mismatch);
    }
    let body = body.ok_or_else(|| refused("decode_record", AttemptError::Integrity, "decode"))?;
    Ok(Decision::Replay(Record {
        status,
        headers,
        body: Bytes::from(body),
    }))
}

fn read_failed(err: &sqlx::Error, phase: &'static str) -> AttemptError {
    match err {
        sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => {
            failed(err, "decode_record", AttemptError::Integrity, None)
        }
        _ => failed(err, phase, classify(err), None),
    }
}

/// Step 3.
async fn write(
    tx: &mut Tx<'_>,
    scope: &ScopeKey,
    caller: &CallerIdentity,
    fingerprint: &Digest,
    record: &Record,
    retention: Duration,
) -> Result<(), AttemptError> {
    // Replaces only an expired row; a live row leaves no row affected.
    let written = observed(
        "write idempotency record",
        sqlx::query!(
            "INSERT INTO http_idempotency_records AS r \
             (scope_key, fingerprint, status, headers, body, issuer, caller_kind, caller_value, \
              expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, statement_timestamp() + $9) \
             ON CONFLICT (scope_key) DO UPDATE SET fingerprint = EXCLUDED.fingerprint, \
             status = EXCLUDED.status, headers = EXCLUDED.headers, body = EXCLUDED.body, \
             issuer = EXCLUDED.issuer, caller_kind = EXCLUDED.caller_kind, \
             caller_value = EXCLUDED.caller_value, expires_at = EXCLUDED.expires_at \
             WHERE r.expires_at <= statement_timestamp()",
            scope.0.as_slice(),
            fingerprint.as_slice(),
            record.status,
            &record.headers as _,
            record.body.as_ref(),
            caller.issuer,
            caller.kind.as_str(),
            caller.value,
            retention as _,
        )
        .execute(tx),
    )
    .await
    .map_err(|err| failed(&err, "write_record", classify(&err), None))?;
    if written.rows_affected() == 1 {
        Ok(())
    } else {
        Err(refused(
            "write_record",
            AttemptError::Internal,
            "record_not_written",
        ))
    }
}

/// The [`Err`] side of the transaction boundary. An [`Err`] from the closure
/// asks the provider to roll back. The provider can also return its own error.
enum Stop<R> {
    /// Arbitration decided without running the work.
    Decided(Decision),
    /// The work ran and asked for rollback.
    RolledBack(R),
    /// An attempt failure inside the closure or a provider failure converted
    /// from [`TxError`]. With [`TxError::CommitUnknown`], the work may have committed.
    Failed(AttemptError),
}

impl<R> From<AttemptError> for Stop<R> {
    fn from(err: AttemptError) -> Self {
        Self::Failed(err)
    }
}

impl<R> From<TxError> for Stop<R> {
    fn from(err: TxError) -> Self {
        Self::Failed(classify_tx(&err))
    }
}

fn classify_tx(err: &TxError) -> AttemptError {
    let (phase, class, commit_outcome) = match err {
        TxError::Acquire(err) => ("acquire", classify(err), None),
        TxError::Begin(err) => ("begin", classify(err), None),
        TxError::CommitFailed(err) => ("commit", classify(err), Some("commit_failed")),
        // The transaction owner could not establish rollback. Even a permanent
        // protocol fault may follow a successful COMMIT, so preserve uncertainty
        // and let the identical same-key retry reconcile the durable outcome.
        TxError::CommitUnknown(_) => ("commit", AttemptError::Unavailable, Some("commit_unknown")),
    };
    failed(err.driver(), phase, class, commit_outcome)
}

fn classify(err: &sqlx::Error) -> AttemptError {
    if is_transient(err) {
        AttemptError::Unavailable
    } else {
        AttemptError::Internal
    }
}

/// Log a failure with only bounded fields, never driver text, and return its
/// class.
fn failed(
    err: &sqlx::Error,
    phase: &'static str,
    class: AttemptError,
    commit_outcome: Option<&'static str>,
) -> AttemptError {
    tracing::warn!(
        phase,
        failure_class = %class,
        commit_outcome,
        sqlstate = sqlstate(err).as_deref(),
        cause = failure_cause(err),
        "http_idempotency_store_failed"
    );
    class
}

fn refused(phase: &'static str, class: AttemptError, cause: &'static str) -> AttemptError {
    tracing::warn!(
        phase,
        failure_class = %class,
        cause,
        "http_idempotency_store_failed"
    );
    class
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_key_is_the_digest_prefix_as_big_endian() {
        let mut digest = [0xff; 32];
        digest[..8].copy_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
        assert_eq!(
            ScopeKey::from_digest(digest).lock_key(),
            0x0102_0304_0506_0708
        );
        assert_eq!(ScopeKey::from_digest([0xff; 32]).lock_key(), -1);
    }

    #[test]
    fn an_unknown_commit_is_unavailable_even_for_a_non_transient_fault() {
        for err in [
            sqlx::Error::Io(std::io::Error::other("connection reset")),
            sqlx::Error::WorkerCrashed,
            sqlx::Error::Protocol("invalid COMMIT acknowledgement".to_owned()),
            sqlx::Error::Decode("invalid COMMIT response".into()),
        ] {
            assert_eq!(
                classify_tx(&TxError::CommitUnknown(err)),
                AttemptError::Unavailable
            );
        }
        assert_eq!(
            classify_tx(&TxError::CommitFailed(sqlx::Error::Protocol(
                "failure before COMMIT".to_owned()
            ))),
            AttemptError::Internal
        );
        assert_eq!(
            classify_tx(&TxError::Begin(sqlx::Error::WorkerCrashed)),
            AttemptError::Internal
        );
    }

    #[tokio::test]
    async fn a_caller_with_a_nul_is_refused_before_the_work_runs() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let store = Store::new(pool.clone(), Duration::from_secs(60)).unwrap();
        for (issuer, value) in [
            ("https://issuer.example", "sub\0ject"),
            ("iss\0uer", "subject"),
        ] {
            let caller = CallerIdentity {
                issuer: issuer.into(),
                kind: CallerKind::Subject,
                value: value.into(),
            };
            let mut ran = false;
            let attempted = store
                .attempt(
                    &ScopeKey::from_digest([0; 32]),
                    &caller,
                    &[0; 32],
                    async |_: &mut Tx<'_>| -> Result<(Record, ()), ()> {
                        ran = true;
                        Err(())
                    },
                )
                .await;
            std::assert_matches!(attempted, Err(AttemptError::Internal));
            assert!(!ran);
        }
        pool.close().await;
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
