//! Safe inspection and version-fenced recovery of one failed job.
//!
//! All mutations borrow the caller's transaction. Their results are provisional
//! until that caller receives an acknowledged commit. No operation runs a
//! handler, republishes an event, or retries an uncertain transaction.

use std::{borrow::Cow, collections::BTreeSet, fmt, time::Duration};

use infra_postgres::{Tx, TxError, observed};
use sqlx::PgPool;

use crate::{JobId, StartupError, kind::is_valid_kind_name};

/// The existing job-operation ceiling, including transaction admission and commit.
pub const OPERATION_TIMEOUT: Duration = crate::engine::OPERATION_BACKSTOP;
/// The existing jobs startup-check ceiling.
pub const STARTUP_TIMEOUT: Duration = crate::maintenance::STARTUP_CHECK_BUDGET;
const MAX_PAGE: u16 = 500;
const MAX_HANDLED_KINDS: usize = 1024;
const MAX_HANDLED_BYTES: usize = MAX_HANDLED_KINDS * 65 - 1;

/// An invalid request field. Diagnostics never contain the rejected value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    #[error("id must be a non-nil UUID")]
    Id,
    #[error("kind must follow the 1..64-byte job-kind grammar")]
    Kind,
    #[error("version must be a nonnegative decimal integer within bigint")]
    Version,
    #[error("cursor must be v1 followed by a canonical non-nil UUID")]
    Cursor,
    #[error("limit must be between 1 and 500")]
    Limit,
    #[error("handled kinds must contain at most 1024 valid names and 66559 bytes")]
    HandledKinds,
}

/// Exactly one inspected identity and failed cycle; fields cannot bypass admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryTarget {
    id: JobId,
    kind: String,
    version: i64,
}

impl RecoveryTarget {
    /// Admit an operator-supplied identity before database access.
    ///
    /// # Errors
    /// Returns the invalid field, without its value.
    pub fn new(id: &str, kind: &str, version: &str) -> Result<Self, InputError> {
        let id = parse_id(id)?;
        if !is_valid_kind_name(kind) {
            return Err(InputError::Kind);
        }
        if version.is_empty() || !version.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(InputError::Version);
        }
        let version = version.parse().map_err(|_| InputError::Version)?;
        Ok(Self {
            id,
            kind: kind.to_owned(),
            version,
        })
    }

    /// The stable job identity.
    #[must_use]
    pub const fn id(&self) -> JobId {
        self.id
    }
    /// The admitted kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }
    /// The expected failed cycle; render as a decimal string in JSON.
    #[must_use]
    pub const fn version(&self) -> i64 {
        self.version
    }
}

fn parse_id(value: &str) -> Result<JobId, InputError> {
    let id = uuid::Uuid::try_parse(value).map_err(|_| InputError::Id)?;
    if id.is_nil() {
        return Err(InputError::Id);
    }
    Ok(JobId(id))
}

/// A bounded, admitted inspection request.
#[derive(Debug)]
pub struct Inspection(Request);

#[derive(Debug)]
enum Request {
    One(JobId),
    Page {
        after: Option<JobId>,
        limit: u16,
        filter: Filter,
    },
}

#[derive(Debug)]
enum Filter {
    Failed,
    Unhandled(BTreeSet<String>),
}

impl Inspection {
    /// Inspect one stable identity.
    ///
    /// # Errors
    /// Refuses an invalid or nil UUID.
    pub fn one(id: &str) -> Result<Self, InputError> {
        Ok(Self(Request::One(parse_id(id)?)))
    }

    /// Scan a bounded primary-key window and return its failed rows.
    ///
    /// # Errors
    /// Refuses malformed cursors and limits outside 1..=500.
    pub fn failed(after: Option<&str>, limit: u16) -> Result<Self, InputError> {
        Self::page(after, limit, Filter::Failed)
    }

    /// Scan a window for live rows outside the explicitly supplied fleet kinds.
    /// An empty string explicitly declares that no kinds are handled.
    ///
    /// # Errors
    /// Refuses malformed cursor, limit, or bounded comma-separated kind list.
    pub fn unhandled(
        handled_kinds: &str,
        after: Option<&str>,
        limit: u16,
    ) -> Result<Self, InputError> {
        if handled_kinds.len() > MAX_HANDLED_BYTES
            || (!handled_kinds.is_empty()
                && handled_kinds.bytes().filter(|byte| *byte == b',').count() >= MAX_HANDLED_KINDS)
        {
            return Err(InputError::HandledKinds);
        }
        let mut kinds = BTreeSet::new();
        if !handled_kinds.is_empty() {
            for kind in handled_kinds.split(',') {
                if !is_valid_kind_name(kind) {
                    return Err(InputError::HandledKinds);
                }
                kinds.insert(kind.to_owned());
            }
        }
        Self::page(after, limit, Filter::Unhandled(kinds))
    }

    fn page(after: Option<&str>, limit: u16, filter: Filter) -> Result<Self, InputError> {
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(InputError::Limit);
        }
        let after = after
            .map(|cursor| {
                if cursor.len() != 39 {
                    return Err(InputError::Cursor);
                }
                let text = cursor.strip_prefix("v1:").ok_or(InputError::Cursor)?;
                let id = parse_id(text).map_err(|_| InputError::Cursor)?;
                if id.to_string() != text {
                    return Err(InputError::Cursor);
                }
                Ok(id)
            })
            .transpose()?;
        Ok(Self(Request::Page {
            after,
            limit,
            filter,
        }))
    }
}

/// The durable queue state, without an open-ended string in the public DTO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Pending,
    Running,
    Completed,
    Failed,
}
impl JobState {
    /// The stable wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// The reason automatic execution stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureReason {
    Permanent,
    Exhausted,
}
impl FailureReason {
    /// The stable wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Permanent => "permanent",
            Self::Exhausted => "exhausted",
        }
    }
}

/// Payload-free queue observation. No field holds stored error or trace content.
#[derive(Debug)]
pub struct JobSnapshot {
    pub id: JobId,
    pub kind: String,
    pub state: JobState,
    pub version: String,
    pub attempts: i32,
    pub failure_reason: Option<FailureReason>,
    pub created_at: String,
    pub not_before: String,
    pub claim_expires_at: Option<String>,
    pub finished_at: Option<String>,
    pub recovery_count: i32,
}

/// An observation is complete only when fewer than the admitted limit were scanned.
#[derive(Debug)]
pub enum InspectionResult {
    One {
        observed_at: String,
        item: Option<JobSnapshot>,
    },
    Page {
        observed_at: String,
        scanned: u16,
        items: Vec<JobSnapshot>,
        complete: bool,
        next_cursor: Option<String>,
    },
}

/// A provisional redrive. Only an acknowledged caller commit establishes it.
#[derive(Debug)]
pub struct Redriven {
    pub target: RecoveryTarget,
    pub new_version: String,
}
/// A provisional discard. Only an acknowledged caller commit establishes deletion.
#[derive(Debug)]
pub struct Discarded {
    pub target: RecoveryTarget,
}

/// Recovery refusal or database failure. Formatting deliberately omits raw causes.
pub enum OperatorError {
    Missing,
    Stale,
    Conflict,
    Database(sqlx::Error),
    Transaction(TxError),
    InvalidStoredState,
}

impl OperatorError {
    /// Whether a caller must reconcile an unknown commit against the same identity.
    #[must_use]
    pub const fn is_commit_unknown(&self) -> bool {
        matches!(self, Self::Transaction(TxError::CommitUnknown(_)))
    }
    /// A bounded database code; no DETAIL, query, value, or connection string.
    #[must_use]
    pub fn sqlstate(&self) -> Option<Cow<'_, str>> {
        let error = match self {
            Self::Database(error)
            | Self::Transaction(
                TxError::Acquire(error)
                | TxError::Begin(error)
                | TxError::CommitFailed(error)
                | TxError::CommitUnknown(error),
            ) => error,
            _ => return None,
        };
        infra_postgres::sqlstate(error)
    }
    /// Closed safe diagnostic class.
    #[must_use]
    pub const fn cause(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Stale => "stale",
            Self::Conflict => "conflict",
            Self::Database(_) => "database",
            Self::InvalidStoredState => "invalid_stored_state",
            Self::Transaction(TxError::Acquire(_)) => "acquire",
            Self::Transaction(TxError::Begin(_)) => "begin",
            Self::Transaction(TxError::CommitFailed(_)) => "commit_failed",
            Self::Transaction(TxError::CommitUnknown(_)) => "commit_unknown",
        }
    }
}
impl fmt::Display for OperatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "jobs operator: {}", self.cause())?;
        if let Some(code) = self.sqlstate() {
            write!(f, " ({code})")?;
        }
        Ok(())
    }
}
impl fmt::Debug for OperatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for OperatorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Transaction(error) => Some(error),
            _ => None,
        }
    }
}
impl From<sqlx::Error> for OperatorError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}
impl From<TxError> for OperatorError {
    fn from(error: TxError) -> Self {
        Self::Transaction(error)
    }
}

/// Share the worker's UTF8/isolation checks without creating an engine.
///
/// # Errors
/// Refuses an unavailable or incompatible session; writable admission is optional.
pub async fn check_startup(pool: &PgPool, require_writable: bool) -> Result<(), StartupError> {
    crate::maintenance::check_startup(pool, require_writable).await
}

struct SnapshotRow {
    id: uuid::Uuid,
    kind: String,
    state: String,
    version: String,
    attempts: i16,
    failure_reason: Option<String>,
    created_at: String,
    not_before: String,
    claim_expires_at: Option<String>,
    finished_at: Option<String>,
    recovery_count: i32,
}

impl TryFrom<SnapshotRow> for JobSnapshot {
    type Error = OperatorError;
    fn try_from(row: SnapshotRow) -> Result<Self, Self::Error> {
        let state = match row.state.as_str() {
            "pending" => JobState::Pending,
            "running" => JobState::Running,
            "completed" => JobState::Completed,
            "failed" => JobState::Failed,
            _ => return Err(OperatorError::InvalidStoredState),
        };
        let failure_reason = match row.failure_reason.as_deref() {
            None => None,
            Some("permanent") => Some(FailureReason::Permanent),
            Some("exhausted") => Some(FailureReason::Exhausted),
            Some(_) => return Err(OperatorError::InvalidStoredState),
        };
        Ok(Self {
            id: JobId(row.id),
            kind: row.kind,
            state,
            version: row.version,
            attempts: i32::from(row.attempts),
            failure_reason,
            created_at: row.created_at,
            not_before: row.not_before,
            claim_expires_at: row.claim_expires_at,
            finished_at: row.finished_at,
            recovery_count: row.recovery_count,
        })
    }
}

/// Observe safe columns with a two-second statement backstop inside the caller's
/// read-only transaction. A page bounds scanned rows before filtering.
///
/// # Errors
/// Propagates unavailable observations; never turns a timeout into an empty page.
pub async fn inspect(
    tx: &mut Tx<'_>,
    inspection: &Inspection,
) -> Result<InspectionResult, OperatorError> {
    observed(
        "set statement timeout",
        sqlx::query!("SET LOCAL statement_timeout = '2000ms'").execute(&mut *tx),
    )
    .await?;
    let observed_at = observed("observe jobs time", sqlx::query_scalar!(
        r#"SELECT to_char(statement_timestamp() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "observed_at!""#
    ).fetch_one(&mut *tx)).await?;
    match &inspection.0 {
        Request::One(id) => {
            let item = observed("inspect jobs", sqlx::query_as!(SnapshotRow,
r#"SELECT id, kind, state, claim_generation::text AS "version!", attempts, failure_reason,
        to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "created_at!",
        to_char(not_before AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "not_before!",
        to_char(claim_expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "claim_expires_at?",
        to_char(finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "finished_at?",
        jsonb_array_length(recovery_history) AS "recovery_count!"
        FROM background_jobs WHERE id = $1"#, id.0).fetch_optional(&mut *tx)).await?
                .map(JobSnapshot::try_from).transpose()?;
            Ok(InspectionResult::One { observed_at, item })
        }
        Request::Page {
            after,
            limit,
            filter,
        } => {
            let rows = if let Some(after) = after {
                observed("inspect jobs", sqlx::query_as!(SnapshotRow,
r#"SELECT id, kind, state, claim_generation::text AS "version!", attempts, failure_reason,
        to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "created_at!",
        to_char(not_before AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "not_before!",
        to_char(claim_expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "claim_expires_at?",
        to_char(finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "finished_at?",
        jsonb_array_length(recovery_history) AS "recovery_count!"
        FROM background_jobs WHERE id > $1 ORDER BY id LIMIT $2"#, after.0, i64::from(*limit)).fetch_all(&mut *tx)).await?
            } else {
                observed("inspect jobs", sqlx::query_as!(SnapshotRow,
r#"SELECT id, kind, state, claim_generation::text AS "version!", attempts, failure_reason,
        to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "created_at!",
        to_char(not_before AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "not_before!",
        to_char(claim_expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "claim_expires_at?",
        to_char(finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "finished_at?",
        jsonb_array_length(recovery_history) AS "recovery_count!"
        FROM background_jobs ORDER BY id LIMIT $1"#, i64::from(*limit)).fetch_all(&mut *tx)).await?
            };
            let scanned =
                u16::try_from(rows.len()).map_err(|_| OperatorError::InvalidStoredState)?;
            let complete = scanned < *limit;
            let next_cursor = if complete {
                None
            } else {
                rows.last().map(|row| format!("v1:{}", row.id))
            };
            let mut items = Vec::new();
            for row in rows {
                let item = JobSnapshot::try_from(row)?;
                let include = match filter {
                    Filter::Failed => item.state == JobState::Failed,
                    Filter::Unhandled(kinds) => {
                        matches!(item.state, JobState::Pending | JobState::Running)
                            && !kinds.contains(&item.kind)
                    }
                };
                if include {
                    items.push(item);
                }
            }
            Ok(InspectionResult::Page {
                observed_at,
                scanned,
                items,
                complete,
                next_cursor,
            })
        }
    }
}

async fn lock_failed(tx: &mut Tx<'_>, target: &RecoveryTarget) -> Result<(), OperatorError> {
    let row = observed(
        "lock failed job",
        sqlx::query!(
            "SELECT kind, state, claim_generation FROM background_jobs WHERE id = $1 FOR UPDATE",
            target.id.0
        )
        .fetch_optional(&mut *tx),
    )
    .await?
    .ok_or(OperatorError::Missing)?;
    if row.kind != target.kind || row.state != "failed" || row.claim_generation != target.version {
        return Err(OperatorError::Stale);
    }
    Ok(())
}

/// Redrive the same failed identity, archiving its previous cycle atomically.
/// The result remains provisional until the caller's commit is acknowledged.
///
/// # Errors
/// Refuses missing/stale targets or a conflicting live key; all failures must
/// propagate out of the caller's transaction so that recovery is rolled back.
pub async fn redrive(tx: &mut Tx<'_>, target: &RecoveryTarget) -> Result<Redriven, OperatorError> {
    lock_failed(tx, target).await?;
    let row = observed(
        "redrive failed job",
        sqlx::query!(
            r#"
        UPDATE background_jobs SET
            recovery_history = recovery_history || jsonb_build_array(jsonb_build_object(
                'version', claim_generation::text, 'attempts', attempts,
                'failure_reason', failure_reason, 'finished_at', finished_at,
                'attempted_by', attempted_by, 'error_summary', error_summary,
                'errors', errors, 'redriven_at', statement_timestamp())),
            state = 'pending', not_before = statement_timestamp(), attempts = 0,
            claim_generation = nextval('background_jobs_claim_generation'),
            claim_expires_at = NULL, attempted_by = NULL, finished_at = NULL,
            failure_reason = NULL, error_summary = NULL, errors = '[]'
        WHERE id = $1 AND kind = $2 AND claim_generation = $3 AND state = 'failed'
        RETURNING claim_generation::text AS "version!"
        "#,
            target.id.0,
            target.kind,
            target.version
        )
        .fetch_optional(&mut *tx),
    )
    .await
    .map_err(|error| {
        if error.as_database_error().is_some_and(|db| {
            db.code().as_deref() == Some("23505")
                && db.constraint() == Some("background_jobs_live_unique_key")
        }) {
            OperatorError::Conflict
        } else {
            OperatorError::Database(error)
        }
    })?
    .ok_or(OperatorError::Stale)?;
    Ok(Redriven {
        target: target.clone(),
        new_version: row.version,
    })
}

/// Permanently abandon one exact failed cycle. An unpublished event may be lost.
/// The result remains provisional until the caller's commit is acknowledged.
///
/// # Errors
/// Refuses missing/stale targets and propagates database failures.
pub async fn discard(tx: &mut Tx<'_>, target: &RecoveryTarget) -> Result<Discarded, OperatorError> {
    lock_failed(tx, target).await?;
    let result = observed("discard failed job", sqlx::query!(
        "DELETE FROM background_jobs WHERE id = $1 AND kind = $2 AND claim_generation = $3 AND state = 'failed'",
        target.id.0, target.kind, target.version
    ).execute(&mut *tx)).await?;
    if result.rows_affected() != 1 {
        return Err(OperatorError::Stale);
    }
    Ok(Discarded {
        target: target.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn recovery_inputs_reject_values_before_database_access() {
        assert_eq!(
            RecoveryTarget::new("00000000-0000-0000-0000-000000000000", "a", "0"),
            Err(InputError::Id)
        );
        assert_eq!(
            RecoveryTarget::new(ID, "bad kind", "0"),
            Err(InputError::Kind)
        );
        for version in ["", "-1", "+1", " 1", "1 ", "9223372036854775808", "1e2"] {
            assert_eq!(
                RecoveryTarget::new(ID, "a", version),
                Err(InputError::Version)
            );
        }
        assert_eq!(
            RecoveryTarget::new(ID, "a", "9223372036854775807")
                .unwrap()
                .version(),
            i64::MAX
        );
        assert_eq!(RecoveryTarget::new(ID, "a", "0").unwrap().version(), 0);
    }

    #[test]
    fn inspection_admits_only_bounded_explicit_requests() {
        assert!(Inspection::unhandled("", None, 1).is_ok());
        assert!(Inspection::unhandled("a,a,b", None, 500).is_ok());
        for list in ["a,", "a, b", "A", ","] {
            assert!(matches!(
                Inspection::unhandled(list, None, 100),
                Err(InputError::HandledKinds)
            ));
        }
        assert!(Inspection::unhandled(&vec!["a"; 1024].join(","), None, 100).is_ok());
        assert!(matches!(
            Inspection::unhandled(&vec!["a"; 1025].join(","), None, 100),
            Err(InputError::HandledKinds)
        ));
        assert!(matches!(
            Inspection::failed(None, 0),
            Err(InputError::Limit)
        ));
        assert!(matches!(
            Inspection::failed(None, 501),
            Err(InputError::Limit)
        ));
        assert!(Inspection::failed(Some(&format!("v1:{ID}")), 100).is_ok());
        for cursor in [
            format!("v2:{ID}"),
            format!("v1:{}", ID.to_uppercase()),
            format!("v1:{}", ID.replace('-', "")),
        ] {
            assert!(matches!(
                Inspection::failed(Some(&cursor), 100),
                Err(InputError::Cursor)
            ));
        }
    }

    #[test]
    fn diagnostic_formatting_never_exposes_raw_database_causes() {
        let error = OperatorError::Transaction(TxError::CommitUnknown(sqlx::Error::Protocol(
            "secret-payload-key-dsn".to_owned(),
        )));
        assert!(error.is_commit_unknown());
        assert_eq!(error.to_string(), "jobs operator: commit_unknown");
        assert_eq!(format!("{error:?}"), "jobs operator: commit_unknown");
        assert_eq!(error.sqlstate(), None);
    }
}
