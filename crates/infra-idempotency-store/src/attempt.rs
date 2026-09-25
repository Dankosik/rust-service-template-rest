//! Arbitration and the record-writing transaction.
//!
//! An attempt holds one explicit `READ COMMITTED` transaction. It first
//! verifies the writer, takes the scope's transaction-scoped advisory lock
//! without waiting, then reads a live record. A live record decides before
//! lock ownership; only a missing live record with the lock runs work.

use std::borrow::Cow;
use std::fmt;
use std::time::Duration;

use infra_postgres::{Tx, TxError, connection, in_tx_with};
use sqlx::Row;
use sqlx::postgres::{PgConnection, PgRow};

use crate::{READ_COMMITTED, Store};

const WRITER_CHECK_AND_LOCK: &str = "SELECT NOT pg_is_in_recovery() \
    AND current_setting('transaction_read_only') = 'off' AS writable, \
    CASE WHEN pg_is_in_recovery() OR current_setting('transaction_read_only') = 'on' \
    THEN false ELSE pg_try_advisory_xact_lock($1) END AS acquired";

const READ: &str = "SELECT fingerprint, status, headers, body \
    FROM http_idempotency_records \
    WHERE scope_key = $1 AND expires_at > statement_timestamp()";

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
    pub body: Vec<u8>,
}

/// What work asks the store to do with the provider-owned transaction.
#[derive(Debug)]
pub enum WorkOutput<T> {
    Commit(Record),
    Rollback(T),
}

/// The closed outcome of one [`Store::attempt`].
#[derive(Debug)]
pub enum Attempted<T> {
    /// A retry with the identical request/key may reach normal arbitration.
    Unavailable,
    /// A known database, query, or record-write fault. The work rolled back.
    Internal,
    /// A stored row could not be decoded. The work did not run.
    Integrity,
    /// A live record decided the attempt. The work did not run.
    Live { matched: bool, record: Record },
    /// Another transaction holds the matching advisory scope. The work did not run.
    InProgress,
    /// Work requested rollback.
    RolledBack(T),
    /// The transaction committed and its success record is durable.
    Committed(Record),
}

impl Store {
    /// Arbitrate `scope`, then run `work` with the same transaction that writes
    /// a storable success record. Work is never retried automatically.
    pub async fn attempt<W, T>(
        &self,
        scope: &ScopeKey,
        caller: &CallerIdentity,
        fingerprint: &Digest,
        work: W,
    ) -> Attempted<T>
    where
        W: AsyncFnOnce(&mut Tx<'_>) -> WorkOutput<T>,
    {
        let Some(inner) = &self.inner else {
            tracing::warn!(
                event = "http_idempotency_store_failure",
                phase = "acquire",
                failure_class = "unavailable",
                cause = "store_inactive"
            );
            return Attempted::Unavailable;
        };
        let retention = inner.retention;
        let executed = in_tx_with(
            &inner.pool,
            READ_COMMITTED,
            async |tx: &mut Tx<'_>| -> Result<Record, Stop<T>> {
                {
                    let conn = connection(tx);
                    arbitrate(conn, scope, fingerprint).await?;
                }
                let record = match work(tx).await {
                    WorkOutput::Commit(record) => record,
                    WorkOutput::Rollback(value) => {
                        return Err(Stop(Attempted::RolledBack(value)));
                    }
                };
                {
                    let conn = connection(tx);
                    write(conn, scope, caller, &record, retention).await?;
                }
                Ok(record)
            },
        )
        .await;
        match executed {
            Ok(record) => Attempted::Committed(record),
            Err(Stop(outcome)) => outcome,
        }
    }
}

fn lock_key(scope: &ScopeKey) -> i64 {
    let mut prefix = [0; 8];
    prefix.copy_from_slice(&scope.0[..8]);
    i64::from_be_bytes(prefix)
}

async fn arbitrate<T>(
    conn: &mut PgConnection,
    scope: &ScopeKey,
    fingerprint: &Digest,
) -> Result<(), Stop<T>> {
    let writer = sqlx::query(WRITER_CHECK_AND_LOCK)
        .bind(lock_key(scope))
        .fetch_one(&mut *conn)
        .await
        .map_err(|err| Stop(classify_sql(&err, "arbitrate", false)))?;
    let writable = writer
        .try_get::<bool, _>("writable")
        .map_err(|err| Stop(classify_sql(&err, "writer_check", false)))?;
    let acquired = writer
        .try_get::<bool, _>("acquired")
        .map_err(|err| Stop(classify_sql(&err, "writer_check", false)))?;
    if !writable {
        tracing::warn!(
            event = "http_idempotency_store_failure",
            phase = "writer_check",
            failure_class = "unavailable",
            cause = "writer_not_writable"
        );
        return Err(Stop(Attempted::Unavailable));
    }
    let live = sqlx::query(READ)
        .bind(scope.0)
        .fetch_optional(conn)
        .await
        .map_err(|err| Stop(classify_sql(&err, "read_record", false)))?;
    match live {
        Some(row) => {
            let record = decode_record(&row).map_err(|err| {
                log_sql_failure("decode_record", "integrity", &err);
                Stop(Attempted::Integrity)
            })?;
            Err(Stop(Attempted::Live {
                matched: &record.fingerprint == fingerprint,
                record,
            }))
        }
        None if acquired => Ok(()),
        None => Err(Stop(Attempted::InProgress)),
    }
}

async fn write<T>(
    conn: &mut PgConnection,
    scope: &ScopeKey,
    caller: &CallerIdentity,
    record: &Record,
    retention: Duration,
) -> Result<(), Stop<T>> {
    let written = sqlx::query(WRITE)
        .bind(scope.0)
        .bind(record.fingerprint)
        .bind(record.status)
        .bind(&record.headers)
        .bind(record.body.as_slice())
        .bind(&caller.issuer)
        .bind(caller.kind.as_str())
        .bind(&caller.value)
        .bind(retention)
        .execute(conn)
        .await
        .map_err(|err| Stop(classify_sql(&err, "write_record", false)))?;
    if written.rows_affected() == 1 {
        Ok(())
    } else {
        tracing::warn!(
            event = "http_idempotency_store_failure",
            phase = "write_record",
            failure_class = "internal",
            cause = "record_not_written"
        );
        Err(Stop(Attempted::Internal))
    }
}

fn decode_record(row: &PgRow) -> Result<Record, sqlx::Error> {
    Ok(Record {
        fingerprint: row.try_get("fingerprint")?,
        status: row.try_get("status")?,
        headers: row.try_get("headers")?,
        body: row.try_get("body")?,
    })
}

struct Stop<T>(Attempted<T>);

impl<T> From<TxError> for Stop<T> {
    fn from(err: TxError) -> Self {
        Self(classify_tx(err))
    }
}

fn classify_tx<T>(err: TxError) -> Attempted<T> {
    let (phase, unknown, err) = match err {
        TxError::Acquire(err) => ("acquire", false, err),
        TxError::Begin(err) => ("begin", false, err),
        TxError::CommitFailed(err) => ("commit", false, err),
        TxError::CommitUnknown(err) => ("commit", true, err),
    };
    classify_sql(&err, phase, unknown)
}

fn classify_sql<T>(err: &sqlx::Error, phase: &'static str, commit_unknown: bool) -> Attempted<T> {
    let unavailable = match sqlstate(err).as_deref() {
        Some(code) => transient_sqlstate(code),
        None => {
            matches!(
                err,
                sqlx::Error::PoolTimedOut
                    | sqlx::Error::PoolClosed
                    | sqlx::Error::Io(_)
                    | sqlx::Error::Tls(_)
            ) || (commit_unknown
                && matches!(err, sqlx::Error::Database(_) | sqlx::Error::WorkerCrashed))
        }
    };
    // A conservative provider wrapper cannot turn a known programming/data
    // fault (including Protocol or 25P02) into an availability failure.
    log_sql_failure(
        phase,
        if unavailable {
            "unavailable"
        } else {
            "internal"
        },
        err,
    );
    if unavailable {
        Attempted::Unavailable
    } else {
        Attempted::Internal
    }
}

fn transient_sqlstate(code: &str) -> bool {
    code.starts_with("08")
        || code.starts_with("53")
        || matches!(
            code,
            "40001" | "40003" | "40P01" | "57P01" | "57P02" | "57P03" | "57014" | "25006"
        )
}

fn sqlstate(err: &sqlx::Error) -> Option<Cow<'_, str>> {
    err.as_database_error()?.code().filter(|code| {
        code.len() == 5
            && code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    })
}

fn log_sql_failure(phase: &'static str, failure_class: &'static str, err: &sqlx::Error) {
    if let Some(code) = sqlstate(err) {
        tracing::warn!(
            event = "http_idempotency_store_failure",
            phase,
            failure_class,
            sqlstate = code.as_ref()
        );
    } else {
        let cause = match err {
            sqlx::Error::Database(_) => "database",
            sqlx::Error::PoolTimedOut => "pool_timeout",
            sqlx::Error::PoolClosed => "pool_closed",
            sqlx::Error::Io(_) => "io",
            sqlx::Error::Tls(_) => "tls",
            sqlx::Error::Protocol(_) => "protocol",
            sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => "decode",
            sqlx::Error::WorkerCrashed => "worker_crashed",
            _ => "driver",
        };
        tracing::warn!(
            event = "http_idempotency_store_failure",
            phase,
            failure_class,
            cause
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::error::Error as StdError;
    use std::sync::{Arc, Mutex};

    use sqlx::error::{DatabaseError, ErrorKind};

    use super::*;

    #[derive(Debug)]
    struct Sqlstate(&'static str);

    impl fmt::Display for Sqlstate {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.message())
        }
    }

    impl StdError for Sqlstate {}

    impl DatabaseError for Sqlstate {
        fn message(&self) -> &'static str {
            "sensitive SQL statement and bound value"
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

    fn database(code: &'static str) -> sqlx::Error {
        sqlx::Error::Database(Box::new(Sqlstate(code)))
    }

    #[test]
    fn sqlstates_have_closed_availability_classification() {
        for code in [
            "08006", "53000", "40001", "40003", "40P01", "57P01", "25006",
        ] {
            assert!(
                matches!(
                    classify_sql::<()>(&database(code), "write_record", false),
                    Attempted::Unavailable
                ),
                "{code}"
            );
        }
        for code in ["25P02", "23505", "42P01", "XX000"] {
            assert!(
                matches!(
                    classify_sql::<()>(&database(code), "write_record", false),
                    Attempted::Internal
                ),
                "{code}"
            );
        }
    }

    #[test]
    fn unknown_commit_preserves_uncertainty_and_known_nontransient_faults() {
        for err in [
            database("40003"),
            database("40001"),
            sqlx::Error::Io(std::io::Error::other("connection reset")),
        ] {
            assert!(matches!(
                classify_tx::<()>(TxError::CommitUnknown(err)),
                Attempted::Unavailable
            ));
        }
        for err in [
            database("25P02"),
            database("23505"),
            database("42P01"),
            sqlx::Error::Protocol("driver misuse".to_owned()),
        ] {
            assert!(matches!(
                classify_tx::<()>(TxError::CommitUnknown(err)),
                Attempted::Internal
            ));
        }
    }

    type DiagnosticFields = BTreeMap<&'static str, String>;

    #[derive(Clone, Default)]
    struct Diagnostics(Arc<Mutex<Vec<DiagnosticFields>>>);

    impl tracing::Subscriber for Diagnostics {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut fields = BTreeMap::new();
            event.record(
                &mut |field: &tracing::field::Field, value: &dyn fmt::Debug| {
                    fields.insert(field.name(), format!("{value:?}"));
                },
            );
            self.0.lock().expect("diagnostic lock").push(fields);
        }
    }

    #[test]
    fn transaction_failures_emit_only_bounded_diagnostics() {
        let captured = Diagnostics::default();
        tracing::subscriber::with_default(captured.clone(), || {
            let _ = classify_tx::<()>(TxError::CommitUnknown(database("40003")));
            let _ = classify_tx::<()>(TxError::CommitUnknown(sqlx::Error::Protocol(
                "sensitive SQL statement and bound value".to_owned(),
            )));
            let _ = classify_tx::<()>(TxError::Begin(database("08secret\nvalue")));
        });
        let events = captured.0.lock().expect("diagnostic lock");
        let expected = [
            ("commit", "unavailable", "sqlstate", "40003"),
            ("commit", "internal", "cause", "protocol"),
            ("begin", "internal", "cause", "database"),
        ];
        assert_eq!(events.len(), expected.len());
        for (event, (phase, class, detail_field, detail)) in events.iter().zip(expected) {
            assert_eq!(
                event,
                &BTreeMap::from([
                    ("event", "\"http_idempotency_store_failure\"".to_owned()),
                    ("phase", format!("{phase:?}")),
                    ("failure_class", format!("{class:?}")),
                    (detail_field, format!("{detail:?}")),
                ])
            );
        }
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
