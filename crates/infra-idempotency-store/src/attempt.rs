use std::time::Duration;

use bytes::Bytes;
use infra_postgres::{
    BudgetedTx, Tx, TxError, TxMode, failure_cause, in_tx_with, in_tx_with_acquire_deadline,
    in_tx_with_deadline, is_transient, observed, sqlstate,
};
use sha2::{Digest as _, Sha256};

use crate::record::HeaderPair;
use crate::{READ_COMMITTED, Record, Store};

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

#[derive(Debug)]
pub enum Attempted<R> {
    Committed(Record),
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

impl Store {
    pub async fn attempt<W, R>(
        &self,
        scope: &ScopeKey,
        fingerprint: &RequestFingerprint,
        work: W,
    ) -> Result<Attempted<R>, AttemptError>
    where
        W: AsyncFnOnce(&mut Tx<'_>) -> Result<Record, R>,
    {
        scope.validate_caller()?;
        attempted(
            in_tx_with(&self.pool, READ_COMMITTED, async |tx| {
                self.work_in_tx(tx, scope, fingerprint, work).await
            })
            .await,
        )
    }

    pub async fn attempt_with_acquire_deadline<W, R>(
        &self,
        scope: &ScopeKey,
        fingerprint: &RequestFingerprint,
        deadline: tokio::time::Instant,
        work: W,
    ) -> Result<Attempted<R>, AttemptError>
    where
        W: AsyncFnOnce(&mut Tx<'_>) -> Result<Record, R>,
    {
        scope.validate_caller()?;
        attempted(
            in_tx_with_acquire_deadline(&self.pool, deadline, READ_COMMITTED, async |tx| {
                self.work_in_tx(tx, scope, fingerprint, work).await
            })
            .await,
        )
    }

    pub async fn attempt_with_deadline<W, R>(
        &self,
        scope: &ScopeKey,
        fingerprint: &RequestFingerprint,
        deadline: tokio::time::Instant,
        work: W,
    ) -> Result<Attempted<R>, AttemptError>
    where
        W: AsyncFnOnce(&mut BudgetedTx<'_>) -> Result<Record, R>,
    {
        scope.validate_caller()?;
        attempted(
            in_tx_with_deadline(&self.pool, deadline, READ_COMMITTED, async |tx| {
                self.work_in_tx(tx, scope, fingerprint, work).await
            })
            .await,
        )
    }

    async fn work_in_tx<M: TxMode, W, R>(
        &self,
        tx: &mut Tx<'_, M>,
        scope: &ScopeKey,
        fingerprint: &RequestFingerprint,
        work: W,
    ) -> Result<Record, Stop<R>>
    where
        W: AsyncFnOnce(&mut Tx<'_, M>) -> Result<Record, R>,
    {
        if let Some(decision) = arbitrate(tx, scope, fingerprint).await? {
            return Err(Stop::Decided(decision));
        }
        if self.terminal_refusals {
            tx.begin_command_savepoint()
                .await
                .map_err(|err| sql_failed(&err, "savepoint"))?;
        }
        let record = work(tx).await.map_err(Stop::RolledBack)?;
        if record.is_terminal_refusal() {
            if !self.terminal_refusals {
                return Err(Stop::Failed(log_failure(
                    "command",
                    AttemptError::Internal,
                    "undeclared_terminal_refusal",
                )));
            }
            tx.rollback_command_savepoint()
                .await
                .map_err(|err| sql_failed(&err, "rollback_command"))?;
        }
        write(tx, scope, fingerprint, &record, self.retention).await?;
        Ok(record)
    }
}

fn attempted<R>(outcome: Result<Record, Stop<R>>) -> Result<Attempted<R>, AttemptError> {
    match outcome {
        Ok(value) => Ok(Attempted::Committed(value)),
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
    let probe = observed(
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
        return decide_live(stored_fingerprint, status, headers, probe.body, fingerprint).map(Some);
    }
    if !probe.acquired {
        return Ok(Some(Decision::InProgress));
    }
    observed(
        "read idempotency record",
        sqlx::query!(
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
    .map(|record| {
        decide_live(
            record.fingerprint,
            record.status,
            record.headers,
            record.body,
            fingerprint,
        )
    })
    .transpose()
}

fn decide_live(
    stored_fingerprint: Digest,
    status: i16,
    headers: Vec<HeaderPair>,
    body: Option<Vec<u8>>,
    fingerprint: &RequestFingerprint,
) -> Result<Decision, AttemptError> {
    if &stored_fingerprint != fingerprint.as_bytes() {
        return Ok(Decision::Mismatch);
    }
    let body =
        body.ok_or_else(|| log_failure("decode_record", AttemptError::Integrity, "decode"))?;
    Record::decode(status, headers, Bytes::from(body))
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
    let headers = record.encode_headers();
    let written = observed(
        "write idempotency record",
        sqlx::query!(
            "WITH locked AS MATERIALIZED ( \
                 SELECT expires_at FROM http_idempotency_records \
                 WHERE scope_key = $1 FOR UPDATE) \
             INSERT INTO http_idempotency_records AS r \
             (scope_key, fingerprint, status, headers, body, issuer, caller_kind, caller_value, \
              expires_at) \
             SELECT $1, $2, $3, $4, $5, $6, $7, $8, clock_timestamp() + $9 \
             WHERE NOT EXISTS (SELECT 1 FROM locked WHERE expires_at > statement_timestamp()) \
             ON CONFLICT (scope_key) DO UPDATE SET fingerprint = EXCLUDED.fingerprint, \
             status = EXCLUDED.status, headers = EXCLUDED.headers, body = EXCLUDED.body, \
             issuer = EXCLUDED.issuer, caller_kind = EXCLUDED.caller_kind, \
             caller_value = EXCLUDED.caller_value, expires_at = clock_timestamp() + $9 \
             WHERE r.expires_at <= statement_timestamp()",
            scope.digest.as_slice(),
            fingerprint.as_bytes().as_slice(),
            record.encode_status(),
            &headers as _,
            record.body(),
            scope.caller.issuer,
            scope.caller.kind.as_str(),
            scope.caller.value,
            retention as _,
        )
        .execute(tx),
    )
    .await
    .map_err(|err| sql_failed(&err, "write_record"))?;
    if written.rows_affected() == 1 {
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

fn log_failure(phase: &'static str, class: AttemptError, cause: &'static str) -> AttemptError {
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
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let store = Store::new(pool.clone(), Duration::from_mins(1)).unwrap();
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
                    async |_: &mut Tx<'_>| -> Result<Record, ()> {
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
        assert_eq!(
            format!("{caller:?}"),
            r#"CallerIdentity { issuer: "[REDACTED]", kind: Subject, value: "[REDACTED]" }"#
        );
    }
}
