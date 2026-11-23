use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use infra_postgres::{
    BudgetedTx, SavepointEnd, SavepointError, Tx, TxError, TxMode, failure_cause, is_transient,
    observed, sqlstate,
};
use sha2::{Digest as _, Sha256};
use sqlx::postgres::PgBindIterExt as _;

use crate::record::HeaderPair;
use crate::{READ_COMMITTED, Record, Store, SuccessRecord, TerminalRefusal};

pub type Digest = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestFingerprint(Digest);

impl RequestFingerprint {
    #[must_use]
    pub const fn from_bytes(bytes: Digest) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &Digest {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeKey {
    digest: Digest,
    caller: CallerIdentity,
}

impl ScopeKey {
    #[must_use]
    pub fn new(caller: CallerIdentity, key: &[u8]) -> Self {
        fn update_len_prefixed(hash: &mut Sha256, bytes: &[u8]) {
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        }

        let mut hash = Sha256::new();
        hash.update(b"http-idempotency/scope/v2\0");
        update_len_prefixed(&mut hash, caller.issuer.as_bytes());
        hash.update([match caller.kind {
            CallerKind::Subject => 1,
            CallerKind::Client => 2,
        }]);
        update_len_prefixed(&mut hash, caller.value.as_bytes());
        update_len_prefixed(&mut hash, key);
        Self {
            digest: hash.finalize().into(),
            caller,
        }
    }

    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    fn lock_key(&self) -> i64 {
        let [b0, b1, b2, b3, b4, b5, b6, b7, ..] = self.digest;
        i64::from_be_bytes([b0, b1, b2, b3, b4, b5, b6, b7])
    }

    fn validate_caller(&self) -> Result<(), AttemptError> {
        if self.caller.issuer.contains('\0') || self.caller.value.contains('\0') {
            return Err(log_failure(
                "caller_identity",
                AttemptError::Internal,
                "unstorable_caller",
            ));
        }
        Ok(())
    }
}

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

#[derive(Clone, PartialEq, Eq, derive_more::Debug)]
pub struct CallerIdentity {
    #[debug("\"[REDACTED]\"")]
    pub issuer: String,
    pub kind: CallerKind,
    #[debug("\"[REDACTED]\"")]
    pub value: String,
}

mod sealed {
    pub trait RefusalMode {}
}

pub trait RefusalMode: sealed::RefusalMode + Send {
    const NEEDS_SAVEPOINT: bool;

    fn into_record(self) -> Record;
}

impl sealed::RefusalMode for Infallible {}
impl RefusalMode for Infallible {
    const NEEDS_SAVEPOINT: bool = false;

    fn into_record(self) -> Record {
        match self {}
    }
}

impl sealed::RefusalMode for TerminalRefusal {}
impl RefusalMode for TerminalRefusal {
    const NEEDS_SAVEPOINT: bool = true;

    fn into_record(self) -> Record {
        Record::from(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOutcome<T = Infallible> {
    Success(SuccessRecord),
    TerminalRefusal(T),
}

impl WriteOutcome {
    #[must_use]
    pub const fn success(record: SuccessRecord) -> Self {
        Self::Success(record)
    }
}

#[derive(Debug)]
pub enum Attempted<R> {
    Committed(Record),
    TerminalRefusal(Record),
    RolledBack(R),
    Replay(Record),
    Mismatch,
    InProgress,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AttemptError {
    #[error("attempt deadline elapsed")]
    Deadline,
    #[error("unavailable")]
    Unavailable,
    #[error("internal")]
    Internal,
    #[error("integrity")]
    Integrity,
}

impl AttemptError {
    #[must_use]
    pub fn classify_sql(error: &sqlx::Error) -> Self {
        if is_transient(error) {
            Self::Unavailable
        } else {
            Self::Internal
        }
    }
}

enum Written {
    Success(Record),
    TerminalRefusal(Record),
}

impl<T: RefusalMode> From<WriteOutcome<T>> for Written {
    fn from(outcome: WriteOutcome<T>) -> Self {
        match outcome {
            WriteOutcome::Success(record) => Self::Success(record.into_record()),
            WriteOutcome::TerminalRefusal(refusal) => Self::TerminalRefusal(refusal.into_record()),
        }
    }
}

impl Written {
    fn into_savepoint_end<A>(self) -> SavepointEnd<Self, A> {
        match self {
            Self::Success(_) => SavepointEnd::Keep(self),
            Self::TerminalRefusal(_) => SavepointEnd::Rollback(self),
        }
    }

    const fn record(&self) -> &Record {
        match self {
            Self::Success(record) | Self::TerminalRefusal(record) => record,
        }
    }
}

impl Store {
    pub async fn attempt<T, W, R>(
        &self,
        scope: &ScopeKey,
        fingerprint: &RequestFingerprint,
        work: W,
    ) -> Result<Attempted<R>, AttemptError>
    where
        T: RefusalMode,
        W: AsyncFnOnce(&mut Tx<'_>) -> Result<WriteOutcome<T>, R>,
    {
        scope.validate_caller()?;
        settle(
            self.inner
                .pool
                .in_tx_with(READ_COMMITTED, async |tx| {
                    self.attempt_in_tx(tx, scope, fingerprint, work).await
                })
                .await,
        )
    }

    pub async fn attempt_with_deadline<T, W, R>(
        &self,
        scope: &ScopeKey,
        fingerprint: &RequestFingerprint,
        deadline: tokio::time::Instant,
        work: W,
    ) -> Result<Attempted<R>, AttemptError>
    where
        T: RefusalMode,
        W: AsyncFnOnce(&mut BudgetedTx<'_>) -> Result<WriteOutcome<T>, R>,
    {
        scope.validate_caller()?;
        settle(
            self.inner
                .pool
                .in_tx_with_deadline(deadline, READ_COMMITTED, async |tx| {
                    self.attempt_in_tx(tx, scope, fingerprint, work).await
                })
                .await,
        )
    }

    async fn attempt_in_tx<M: TxMode, T: RefusalMode, W, R>(
        &self,
        tx: &mut Tx<'_, M>,
        scope: &ScopeKey,
        fingerprint: &RequestFingerprint,
        work: W,
    ) -> Result<Written, Stop<R>>
    where
        W: AsyncFnOnce(&mut Tx<'_, M>) -> Result<WriteOutcome<T>, R>,
    {
        if let Some(decision) = arbitrate(tx, scope, fingerprint).await? {
            return Err(Stop::Decided(decision));
        }
        let written = if T::NEEDS_SAVEPOINT {
            tx.with_savepoint(async |tx: &mut Tx<'_, M>| match work(tx).await {
                Ok(outcome) => Written::from(outcome).into_savepoint_end(),
                Err(rollback) => SavepointEnd::AbortTransaction(rollback),
            })
            .await
            .map_err(|error| match error {
                SavepointError::Begin(err) => Stop::Failed(sql_failed(&err, "savepoint")),
                SavepointError::Rollback(err) => Stop::Failed(sql_failed(&err, "rollback_command")),
                SavepointError::Aborted(rollback) => Stop::RolledBack(rollback),
            })?
        } else {
            Written::from(work(tx).await.map_err(Stop::RolledBack)?)
        };
        write(
            tx,
            scope,
            fingerprint,
            written.record(),
            self.inner.retention,
        )
        .await?;
        Ok(written)
    }
}

fn settle<R>(outcome: Result<Written, Stop<R>>) -> Result<Attempted<R>, AttemptError> {
    match outcome {
        Ok(Written::Success(record)) => Ok(Attempted::Committed(record)),
        Ok(Written::TerminalRefusal(record)) => Ok(Attempted::TerminalRefusal(record)),
        Err(Stop::Decided(decision)) => Ok(match decision {
            Decision::Replay(record) => Attempted::Replay(*record),
            Decision::Mismatch => Attempted::Mismatch,
            Decision::InProgress => Attempted::InProgress,
        }),
        Err(Stop::RolledBack(rollback)) => Ok(Attempted::RolledBack(rollback)),
        Err(Stop::Failed(err)) => Err(err),
    }
}

enum Decision {
    Replay(Box<Record>),
    Mismatch,
    InProgress,
}

async fn arbitrate(
    tx: &mut Tx<'_, impl TxMode>,
    scope: &ScopeKey,
    fingerprint: &RequestFingerprint,
) -> Result<Option<Decision>, AttemptError> {
    let probe = observed!(
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
            scope.digest.as_slice(),
            fingerprint.as_bytes().as_slice(),
        )
        .fetch_one(&mut *tx),
    )
    .await
    .map_err(|err| read_failed(&err, "arbitrate"))?;
    if !probe.writable {
        return Err(log_failure(
            "writer_check",
            AttemptError::Unavailable,
            "not_writable",
        ));
    }
    if let (Some(stored_fingerprint), Some(status), Some(headers)) =
        (probe.fingerprint, probe.status, probe.headers)
    {
        let row = StoredRow {
            fingerprint: stored_fingerprint,
            status,
            headers,
            body: probe.body,
        };
        return decide_live(row, fingerprint).map(Some);
    }
    if !probe.acquired {
        return Ok(Some(Decision::InProgress));
    }
    observed!(
        "read idempotency record",
        sqlx::query_as!(
            StoredRow,
            "SELECT fingerprint AS \"fingerprint: Digest\", status, \
             headers AS \"headers: Vec<HeaderPair>\", \
             CASE WHEN fingerprint = $2 THEN body END AS body \
             FROM http_idempotency_records \
             WHERE scope_key = $1 AND expires_at > statement_timestamp()",
            scope.digest.as_slice(),
            fingerprint.as_bytes().as_slice(),
        )
        .fetch_optional(tx),
    )
    .await
    .map_err(|err| read_failed(&err, "read_record"))?
    .map(|row| decide_live(row, fingerprint))
    .transpose()
}

struct StoredRow {
    fingerprint: Digest,
    status: i16,
    headers: Vec<HeaderPair>,
    body: Option<Vec<u8>>,
}

fn decide_live(row: StoredRow, fingerprint: &RequestFingerprint) -> Result<Decision, AttemptError> {
    if &row.fingerprint != fingerprint.as_bytes() {
        return Ok(Decision::Mismatch);
    }
    let body = row
        .body
        .ok_or_else(|| log_failure("decode_record", AttemptError::Integrity, "decode"))?;
    Record::decode(row.status, row.headers, Bytes::from(body))
        .map(|record| Decision::Replay(Box::new(record)))
        .map_err(|err| log_failure("decode_record", AttemptError::Integrity, err.cause()))
}

fn read_failed(err: &sqlx::Error, phase: &'static str) -> AttemptError {
    match err {
        sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => {
            log_sql_failure(err, "decode_record", AttemptError::Integrity, None)
        }
        _ => sql_failed(err, phase),
    }
}

async fn write(
    tx: &mut Tx<'_, impl TxMode>,
    scope: &ScopeKey,
    fingerprint: &RequestFingerprint,
    record: &Record,
    retention: Duration,
) -> Result<(), AttemptError> {
    let inserted = observed!(
        "write idempotency record",
        sqlx::query!(
            "WITH locked AS MATERIALIZED ( \
                 SELECT expires_at FROM http_idempotency_records \
                 WHERE scope_key = $1 FOR UPDATE) \
             INSERT INTO http_idempotency_records AS r \
             (scope_key, fingerprint, status, headers, issuer, caller_kind, caller_value, \
              expires_at, body) \
             SELECT $1, $2, $3, $4, $5, $6, $7, clock_timestamp() + $8, $9 \
             WHERE NOT EXISTS (SELECT 1 FROM locked WHERE expires_at > statement_timestamp()) \
             ON CONFLICT (scope_key) DO UPDATE SET fingerprint = EXCLUDED.fingerprint, \
             status = EXCLUDED.status, headers = EXCLUDED.headers, body = EXCLUDED.body, \
             issuer = EXCLUDED.issuer, caller_kind = EXCLUDED.caller_kind, \
             caller_value = EXCLUDED.caller_value, expires_at = clock_timestamp() + $8 \
             WHERE r.expires_at <= statement_timestamp()",
            scope.digest.as_slice(),
            fingerprint.as_bytes().as_slice(),
            record.encode_status(),
            record.encode_headers().bind_iter() as _,
            scope.caller.issuer,
            scope.caller.kind.as_str(),
            scope.caller.value,
            retention as _,
            record.body(),
        )
        .execute(tx),
    )
    .await
    .map_err(|err| sql_failed(&err, "write_record"))?;
    if inserted.rows_affected() == 1 {
        Ok(())
    } else {
        Err(log_failure(
            "write_record",
            AttemptError::Internal,
            "record_not_written",
        ))
    }
}

enum Stop<R> {
    Decided(Decision),
    RolledBack(R),
    Failed(AttemptError),
}

impl<R> From<AttemptError> for Stop<R> {
    fn from(err: AttemptError) -> Self {
        Self::Failed(err)
    }
}

impl<R> From<TxError> for Stop<R> {
    fn from(err: TxError) -> Self {
        Self::Failed(tx_failed(&err))
    }
}

impl<R> From<infra_postgres::OperationDeadlineElapsed> for Stop<R> {
    fn from(_: infra_postgres::OperationDeadlineElapsed) -> Self {
        Self::Failed(AttemptError::Deadline)
    }
}

fn tx_failed(err: &TxError) -> AttemptError {
    let (phase, class, commit_outcome) = match err {
        TxError::Acquire(err) => ("acquire", AttemptError::classify_sql(err), None),
        TxError::Begin(err) => ("begin", AttemptError::classify_sql(err), None),
        TxError::CommitFailed(err) => (
            "commit",
            AttemptError::classify_sql(err),
            Some("commit_failed"),
        ),
        TxError::CommitUnknown(_) => ("commit", AttemptError::Unavailable, Some("commit_unknown")),
    };
    log_sql_failure(err.driver(), phase, class, commit_outcome)
}

fn sql_failed(err: &sqlx::Error, phase: &'static str) -> AttemptError {
    log_sql_failure(err, phase, AttemptError::classify_sql(err), None)
}

fn log_sql_failure(
    err: &sqlx::Error,
    phase: &'static str,
    class: AttemptError,
    commit_outcome: Option<&'static str>,
) -> AttemptError {
    emit_failure(
        phase,
        class,
        commit_outcome,
        sqlstate(err).as_deref(),
        failure_cause(err),
    )
}

fn log_failure(phase: &'static str, class: AttemptError, cause: &'static str) -> AttemptError {
    emit_failure(phase, class, None, None, cause)
}

fn emit_failure(
    phase: &'static str,
    class: AttemptError,
    commit_outcome: Option<&'static str>,
    sqlstate: Option<&str>,
    cause: &'static str,
) -> AttemptError {
    tracing::warn!(
        phase,
        failure_class = %class,
        commit_outcome,
        sqlstate,
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
        let caller = CallerIdentity {
            issuer: "https://issuer.example".into(),
            kind: CallerKind::Subject,
            value: "fixture-subject".into(),
        };
        assert_eq!(
            ScopeKey::new(caller, b"k-123").lock_key(),
            i64::from_be_bytes([0x98, 0xdf, 0x40, 0x43, 0xb9, 0xe7, 0x95, 0x01])
        );
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
                tx_failed(&TxError::CommitUnknown(err)),
                AttemptError::Unavailable
            );
        }
        assert_eq!(
            tx_failed(&TxError::CommitFailed(sqlx::Error::Protocol(
                "failure before COMMIT".to_owned()
            ))),
            AttemptError::Internal
        );
        assert_eq!(
            tx_failed(&TxError::Begin(sqlx::Error::WorkerCrashed)),
            AttemptError::Internal
        );
    }

    #[tokio::test]
    async fn a_caller_with_a_nul_is_refused_before_the_work_runs() {
        let pool =
            infra_postgres::test_support::connect_lazy("postgres://localhost/unused").unwrap();
        let store = Store::try_new(pool.clone(), Duration::from_mins(1)).unwrap();
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
                    &ScopeKey::new(caller, b"k"),
                    &RequestFingerprint::from_bytes([0; 32]),
                    async |_: &mut Tx<'_>| -> Result<WriteOutcome, ()> {
                        ran = true;
                        Err(())
                    },
                )
                .await;
            std::assert_matches!(attempted, Err(AttemptError::Internal));
            assert!(!ran);
        }
        let _ = pool.close(Duration::from_secs(1)).await;
    }

    #[test]
    fn caller_debug_redacts_verified_values() {
        let caller = CallerIdentity {
            issuer: "https://issuer.example".into(),
            kind: CallerKind::Subject,
            value: "secret-subject".into(),
        };
        assert_eq!(
            format!("{caller:?}"),
            r#"CallerIdentity { issuer: "[REDACTED]", kind: Subject, value: "[REDACTED]" }"#
        );
    }
}
