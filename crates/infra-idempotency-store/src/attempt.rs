use std::time::Duration;

use bytes::Bytes;
use infra_postgres::{
    Tx, TxError, failure_cause, in_tx_with, in_tx_with_acquire_deadline, is_transient, observed,
    sqlstate,
};

use crate::{READ_COMMITTED, Store};

pub type Digest = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopeKey(Digest);

impl ScopeKey {
    #[must_use]
    pub const fn from_digest(digest: Digest) -> Self {
        Self(digest)
    }

    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.0
    }

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

#[derive(Clone, Debug, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "http_idempotency_header_pair")]
pub struct HeaderPair {
    pub name: String,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub status: i16,
    pub headers: Vec<HeaderPair>,
    pub body: Bytes,
}

#[derive(Debug)]
pub enum Attempted<C, R> {
    Committed(C),
    RolledBack(R),
    Replay(Record),
    Mismatch,
    InProgress,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AttemptError {
    #[error("unavailable")]
    Unavailable,
    #[error("internal")]
    Internal,
    #[error("integrity")]
    Integrity,
}

impl Store {
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

enum Decision {
    Replay(Record),
    Mismatch,
    InProgress,
}

async fn arbitrate(
    tx: &mut Tx<'_>,
    scope: &ScopeKey,
    fingerprint: &Digest,
) -> Result<Option<Decision>, AttemptError> {
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

async fn write(
    tx: &mut Tx<'_>,
    scope: &ScopeKey,
    caller: &CallerIdentity,
    fingerprint: &Digest,
    record: &Record,
    retention: Duration,
) -> Result<(), AttemptError> {
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
        Self::Failed(classify_tx(&err))
    }
}

fn classify_tx(err: &TxError) -> AttemptError {
    let (phase, class, commit_outcome) = match err {
        TxError::Acquire(err) => ("acquire", classify(err), None),
        TxError::Begin(err) => ("begin", classify(err), None),
        TxError::CommitFailed(err) => ("commit", classify(err), Some("commit_failed")),
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
        assert_eq!(
            format!("{caller:?}"),
            r#"CallerIdentity { issuer: "[REDACTED]", kind: Subject, value: "[REDACTED]" }"#
        );
    }
}
