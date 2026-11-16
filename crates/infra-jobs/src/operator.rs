use std::{borrow::Cow, collections::BTreeSet, fmt, time::Duration};

use infra_postgres::{Tx, TxError, observed};
use sqlx::PgPool;

use crate::kind::{MAX_KIND_LEN, is_valid_kind_name};
use crate::{JobId, StartupError};

pub use crate::state::{FailureReason, JobState};

pub const OPERATION_TIMEOUT: Duration = crate::engine::OPERATION_BACKSTOP;
pub const STARTUP_TIMEOUT: Duration = crate::session::STARTUP_CHECK_BUDGET;
const MAX_PAGE_LIMIT: u16 = 500;
const MAX_HANDLED_KINDS: usize = 1024;
const MAX_HANDLED_BYTES: usize = MAX_HANDLED_KINDS * (MAX_KIND_LEN + 1) - 1;
const CURSOR_PREFIX: &str = "v1:";
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    #[error("id must be a non-nil UUID")]
    Id,
    #[error("kind must follow the 1..{max}-byte job-kind grammar", max = MAX_KIND_LEN)]
    Kind,
    #[error("version must be a nonnegative decimal integer within bigint")]
    Version,
    #[error(
        "cursor must be {prefix} followed by a canonical non-nil UUID",
        prefix = CURSOR_PREFIX
    )]
    Cursor,
    #[error("limit must be between 1 and {max}", max = MAX_PAGE_LIMIT)]
    Limit,
    #[error(
        "handled kinds must contain at most {kinds} valid names and {bytes} bytes",
        kinds = MAX_HANDLED_KINDS,
        bytes = MAX_HANDLED_BYTES
    )]
    HandledKinds,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryTarget {
    id: JobId,
    kind: String,
    version: i64,
}

impl RecoveryTarget {
    pub fn try_new(id: &str, kind: &str, version: &str) -> Result<Self, InputError> {
        let id = parse_id(id)?;
        if !is_valid_kind_name(kind) {
            return Err(InputError::Kind);
        }
        if !version.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(InputError::Version);
        }
        let version = version.parse().map_err(|_| InputError::Version)?;
        Ok(Self {
            id,
            kind: kind.to_owned(),
            version,
        })
    }

    #[must_use]
    pub const fn id(&self) -> JobId {
        self.id
    }
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }
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

fn parse_cursor(cursor: &str) -> Result<JobId, InputError> {
    if cursor.len() != CURSOR_PREFIX.len() + uuid::fmt::Hyphenated::LENGTH {
        return Err(InputError::Cursor);
    }
    let text = cursor
        .strip_prefix(CURSOR_PREFIX)
        .ok_or(InputError::Cursor)?;
    let id = parse_id(text).map_err(|_| InputError::Cursor)?;
    if text.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(InputError::Cursor);
    }
    Ok(id)
}

fn encode_cursor(id: JobId) -> String {
    format!("{CURSOR_PREFIX}{id}")
}

fn parse_handled_kinds(handled_kinds: &str) -> Result<BTreeSet<String>, InputError> {
    if handled_kinds.is_empty() {
        return Ok(BTreeSet::new());
    }
    if handled_kinds.len() > MAX_HANDLED_BYTES
        || handled_kinds.bytes().filter(|byte| *byte == b',').count() >= MAX_HANDLED_KINDS
    {
        return Err(InputError::HandledKinds);
    }
    let mut kinds = BTreeSet::new();
    for kind in handled_kinds.split(',') {
        if !is_valid_kind_name(kind) {
            return Err(InputError::HandledKinds);
        }
        kinds.insert(kind.to_owned());
    }
    Ok(kinds)
}

#[derive(Debug)]
pub struct Inspection(Selection);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectionMode {
    One,
    Failed,
    Unhandled,
}

#[derive(Debug)]
enum Selection {
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

impl Filter {
    fn admits(&self, item: &JobSnapshot) -> bool {
        match self {
            Self::Failed => item.state == JobState::Failed,
            Self::Unhandled(kinds) => item.state.is_live() && !kinds.contains(&item.kind),
        }
    }
}

impl Inspection {
    pub fn one(id: &str) -> Result<Self, InputError> {
        Ok(Self(Selection::One(parse_id(id)?)))
    }

    pub fn failed(after: Option<&str>, limit: u16) -> Result<Self, InputError> {
        Self::page(after, limit, Filter::Failed)
    }

    pub fn unhandled(
        handled_kinds: &str,
        after: Option<&str>,
        limit: u16,
    ) -> Result<Self, InputError> {
        let kinds = parse_handled_kinds(handled_kinds)?;
        Self::page(after, limit, Filter::Unhandled(kinds))
    }

    #[must_use]
    pub const fn mode(&self) -> InspectionMode {
        match &self.0 {
            Selection::One(_) => InspectionMode::One,
            Selection::Page { filter, .. } => match filter {
                Filter::Failed => InspectionMode::Failed,
                Filter::Unhandled(_) => InspectionMode::Unhandled,
            },
        }
    }

    #[must_use]
    pub const fn job_id(&self) -> Option<JobId> {
        match &self.0 {
            Selection::One(id) => Some(*id),
            Selection::Page { .. } => None,
        }
    }

    #[must_use]
    pub const fn handled_kinds(&self) -> Option<&BTreeSet<String>> {
        match &self.0 {
            Selection::Page {
                filter: Filter::Unhandled(kinds),
                ..
            } => Some(kinds),
            Selection::One(_)
            | Selection::Page {
                filter: Filter::Failed,
                ..
            } => None,
        }
    }

    fn page(after: Option<&str>, limit: u16, filter: Filter) -> Result<Self, InputError> {
        if !(1..=MAX_PAGE_LIMIT).contains(&limit) {
            return Err(InputError::Limit);
        }
        let after = after.map(parse_cursor).transpose()?;
        Ok(Self(Selection::Page {
            after,
            limit,
            filter,
        }))
    }
}

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
        next_cursor: Option<String>,
    },
}

#[derive(Debug)]
pub struct Redriven {
    pub new_version: String,
}

#[derive(thiserror::Error)]
pub enum OperatorError {
    Missing,
    Stale,
    Conflict,
    Database(#[from] sqlx::Error),
    Transaction(#[from] TxError),
    InvalidStoredState,
}

impl OperatorError {
    #[must_use]
    pub const fn is_commit_unknown(&self) -> bool {
        matches!(self, Self::Transaction(TxError::CommitUnknown(_)))
    }
    #[must_use]
    pub fn sqlstate(&self) -> Option<Cow<'_, str>> {
        let error = match self {
            Self::Database(error) => error,
            Self::Transaction(error) => error.driver(),
            Self::Missing | Self::Stale | Self::Conflict | Self::InvalidStoredState => return None,
        };
        infra_postgres::sqlstate(error)
    }
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

pub async fn check_startup(pool: &PgPool) -> Result<(), StartupError> {
    crate::session::check_startup(pool).await
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
        let state = JobState::parse(&row.state).ok_or(OperatorError::InvalidStoredState)?;
        let failure_reason = row
            .failure_reason
            .as_deref()
            .map(|reason| FailureReason::parse(reason).ok_or(OperatorError::InvalidStoredState))
            .transpose()?;
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

async fn apply_statement_timeout(tx: &mut Tx<'_>) -> Result<(), OperatorError> {
    Ok(infra_postgres::set_statement_timeout(tx, STATEMENT_TIMEOUT).await?)
}

macro_rules! snapshot_query {
    ($tail:tt $(, $arg:expr)*) => {
        sqlx::query_as!(
            SnapshotRow,
            r#"SELECT id, kind, state, claim_generation::text AS "version!", attempts, failure_reason,
        to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "created_at!",
        to_char(not_before AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "not_before!",
        to_char(claim_expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "claim_expires_at?",
        to_char(finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "finished_at?",
        jsonb_array_length(recovery_history) AS "recovery_count!"
        FROM background_jobs "# + $tail
            $(, $arg)*
        )
    };
}

pub async fn inspect(
    tx: &mut Tx<'_>,
    inspection: &Inspection,
) -> Result<InspectionResult, OperatorError> {
    apply_statement_timeout(tx).await?;
    let observed_at = observed("observe jobs time", sqlx::query_scalar!(
        r#"SELECT to_char(statement_timestamp() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS "observed_at!""#
    ).fetch_one(&mut *tx)).await?;
    match &inspection.0 {
        Selection::One(id) => {
            let row = observed(
                "inspect jobs",
                snapshot_query!("WHERE id = $1", id.0).fetch_optional(&mut *tx),
            )
            .await?;
            let item = row.map(JobSnapshot::try_from).transpose()?;
            Ok(InspectionResult::One { observed_at, item })
        }
        Selection::Page {
            after,
            limit,
            filter,
        } => {
            let after = after.map_or(uuid::Uuid::nil(), |id| id.0);
            let rows = observed(
                "inspect jobs",
                snapshot_query!(
                    "WHERE id > $1 ORDER BY id LIMIT $2",
                    after,
                    i64::from(*limit)
                )
                .fetch_all(&mut *tx),
            )
            .await?;
            page_result(rows, *limit, filter, observed_at)
        }
    }
}

fn page_result(
    rows: Vec<SnapshotRow>,
    limit: u16,
    filter: &Filter,
    observed_at: String,
) -> Result<InspectionResult, OperatorError> {
    let scanned = u16::try_from(rows.len()).map_err(|_| OperatorError::InvalidStoredState)?;
    let next_cursor = if scanned < limit {
        None
    } else {
        rows.last().map(|row| encode_cursor(JobId(row.id)))
    };
    let mut items = Vec::new();
    for row in rows {
        let item = JobSnapshot::try_from(row)?;
        if filter.admits(&item) {
            items.push(item);
        }
    }
    Ok(InspectionResult::Page {
        observed_at,
        scanned,
        items,
        next_cursor,
    })
}

async fn lock_job_row(tx: &mut Tx<'_>, id: JobId) -> Result<(), OperatorError> {
    apply_statement_timeout(tx).await?;
    observed(
        "lock failed job",
        sqlx::query!(
            "SELECT id FROM background_jobs WHERE id = $1 FOR UPDATE",
            id.0
        )
        .fetch_optional(&mut *tx),
    )
    .await?
    .ok_or(OperatorError::Missing)?;
    Ok(())
}

fn conflict_or_database(error: sqlx::Error) -> OperatorError {
    let live_key_conflict = error.as_database_error().is_some_and(|database| {
        database.is_unique_violation()
            && database.constraint() == Some("background_jobs_live_unique_key")
    });
    if live_key_conflict {
        OperatorError::Conflict
    } else {
        OperatorError::Database(error)
    }
}

pub async fn redrive(tx: &mut Tx<'_>, target: &RecoveryTarget) -> Result<Redriven, OperatorError> {
    lock_job_row(tx, target.id).await?;
    observed(
        "redrive failed job",
        sqlx::query_as!(
            Redriven,
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
        RETURNING claim_generation::text AS "new_version!: String"
        "#,
            target.id.0,
            target.kind,
            target.version
        )
        .fetch_optional(&mut *tx),
    )
    .await
    .map_err(conflict_or_database)?
    .ok_or(OperatorError::Stale)
}

pub async fn discard(tx: &mut Tx<'_>, target: &RecoveryTarget) -> Result<(), OperatorError> {
    lock_job_row(tx, target.id).await?;
    let result = observed("discard failed job", sqlx::query!(
        "DELETE FROM background_jobs WHERE id = $1 AND kind = $2 AND claim_generation = $3 AND state = 'failed'",
        target.id.0, target.kind, target.version
    ).execute(&mut *tx)).await?;
    if result.rows_affected() != 1 {
        return Err(OperatorError::Stale);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn recovery_inputs_reject_values_before_database_access() {
        assert_eq!(
            RecoveryTarget::try_new("00000000-0000-0000-0000-000000000000", "a", "0"),
            Err(InputError::Id)
        );
        assert_eq!(
            RecoveryTarget::try_new(ID, "bad kind", "0"),
            Err(InputError::Kind)
        );
        for version in ["", "-1", "+1", " 1", "1 ", "9223372036854775808", "1e2"] {
            assert_eq!(
                RecoveryTarget::try_new(ID, "a", version),
                Err(InputError::Version)
            );
        }
        assert_eq!(
            RecoveryTarget::try_new(ID, "a", "9223372036854775807")
                .unwrap()
                .version(),
            i64::MAX
        );
        assert_eq!(RecoveryTarget::try_new(ID, "a", "0").unwrap().version(), 0);
        assert!(RecoveryTarget::try_new(&ID.to_ascii_uppercase(), "a", "0").is_ok());
    }

    #[test]
    fn inspection_admits_only_bounded_explicit_requests() {
        let unhandled_none = Inspection::unhandled("", None, 1).unwrap();
        assert!(
            unhandled_none
                .handled_kinds()
                .is_some_and(BTreeSet::is_empty)
        );
        assert!(Inspection::unhandled("a,a,b", None, 500).is_ok());
        for list in ["a,", "a, b", "A", ","] {
            std::assert_matches!(
                Inspection::unhandled(list, None, 100),
                Err(InputError::HandledKinds)
            );
        }
        assert!(Inspection::unhandled(&vec!["a"; 1024].join(","), None, 100).is_ok());
        std::assert_matches!(
            Inspection::unhandled(&vec!["a"; 1025].join(","), None, 100),
            Err(InputError::HandledKinds)
        );
        std::assert_matches!(Inspection::failed(None, 0), Err(InputError::Limit));
        std::assert_matches!(Inspection::failed(None, 501), Err(InputError::Limit));
        assert!(Inspection::failed(Some(&format!("v1:{ID}")), 100).is_ok());
        for cursor in [
            format!("v2:{ID}"),
            format!("v1:{}", ID.to_uppercase()),
            format!("v1:{}", ID.replace('-', "")),
            "v1:0123456-789ab-cdef-0123-456789abcdef".to_owned(),
            "v1:00000000-0000-0000-0000-000000000000".to_owned(),
        ] {
            std::assert_matches!(
                Inspection::failed(Some(&cursor), 100),
                Err(InputError::Cursor)
            );
        }
    }

    #[test]
    fn cursor_diagnostic_names_the_shared_prefix() {
        assert!(InputError::Cursor.to_string().contains(CURSOR_PREFIX));
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
