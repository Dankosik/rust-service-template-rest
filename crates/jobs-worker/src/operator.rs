use std::collections::BTreeSet;
use std::io::Write;
use std::num::NonZeroU32;
use std::time::Duration;

use infra_jobs::operator::{
    self as jobs, FailureReason, InputError, Inspection, InspectionMode, InspectionResult,
    JobSnapshot, OperatorError, RecoveryTarget,
};
use infra_jobs::{JobId, StartupError};
use infra_postgres::{CloseOutcome, ConnectError, Dsn, Isolation, PgPool, TxOptions};
use migrate::HistoryError;
use secrecy::ExposeSecret;
use serde::Serialize;
use serde_with::{DisplayFromStr, serde_as, skip_serializing_none};
use service_config::{JobsOperatorLoadError, LoadOptions, OperatorPostgresConfig};

use crate::report_failure;

use crate::cli::{OperatorCommand, RecoveryArgs};
use crate::shutdown::{DEPENDENCY_CLOSE_BUDGET, RUNTIME_SHUTDOWN_TIMEOUT};
use crate::signals::Signals;

pub(crate) enum Request {
    Inspect(Inspection),
    Redrive(RecoveryTarget),
    Discard(RecoveryTarget),
}

impl Request {
    pub(crate) fn try_new(command: OperatorCommand) -> Result<Self, InputError> {
        Ok(match command {
            OperatorCommand::Inspect { id } => Self::Inspect(Inspection::one(&id)?),
            OperatorCommand::Failed { page } => {
                Self::Inspect(Inspection::failed(page.after.as_deref(), page.limit)?)
            }
            OperatorCommand::Unhandled {
                handled_kinds,
                page,
            } => Self::Inspect(Inspection::unhandled(
                &handled_kinds,
                page.after.as_deref(),
                page.limit,
            )?),
            OperatorCommand::Redrive(target) => Self::Redrive(recovery_target(&target)?),
            OperatorCommand::Discard(target) => Self::Discard(recovery_target(&target)?),
        })
    }

    fn action(&self) -> &'static str {
        match self {
            Self::Inspect(query) => match query.mode() {
                InspectionMode::One => "inspect",
                InspectionMode::Failed => "failed",
                InspectionMode::Unhandled => "unhandled",
            },
            Self::Redrive(_) => "redrive",
            Self::Discard(_) => "discard",
        }
    }

    fn is_mutation(&self) -> bool {
        match self {
            Self::Inspect(_) => false,
            Self::Redrive(_) | Self::Discard(_) => true,
        }
    }

    fn fields(&self) -> RequestFields<'_> {
        match self {
            Self::Inspect(query) => RequestFields::Inspect {
                id: query.job_id(),
                handled_kinds: query.handled_kinds(),
            },
            Self::Redrive(target) | Self::Discard(target) => RequestFields::Recovery {
                id: target.id(),
                kind: target.kind(),
                expected_version: target.version(),
            },
        }
    }

    fn failed_before_start(&self, cause: Cause) -> Failure {
        Failure {
            outcome: if self.is_mutation() {
                Outcome::Failed
            } else {
                Outcome::Unavailable
            },
            cause: cause.as_str(),
            detail: None,
        }
    }

    fn refused(&self, cause: Cause, detail: &'static str) -> Failure {
        Failure {
            detail: Some(FailureDetail::Admission { detail }),
            ..self.failed_before_start(cause)
        }
    }

    fn failed_after_start(&self, cause: Cause) -> Failure {
        Failure {
            outcome: if self.is_mutation() {
                Outcome::Unknown
            } else {
                Outcome::Unavailable
            },
            cause: cause.as_str(),
            detail: None,
        }
    }

    fn database_error(&self, error: &OperatorError) -> Failure {
        let outcome = if self.is_mutation() {
            match error {
                OperatorError::Missing => Outcome::Missing,
                OperatorError::Stale => Outcome::Stale,
                OperatorError::Conflict => Outcome::Conflict,
                OperatorError::Transaction(_) if error.is_commit_unknown() => Outcome::Unknown,
                OperatorError::Transaction(_)
                | OperatorError::Database(_)
                | OperatorError::InvalidStoredState => Outcome::Failed,
            }
        } else {
            Outcome::Unavailable
        };
        Failure {
            outcome,
            cause: error.cause(),
            detail: Some(FailureDetail::Database {
                sqlstate: error.sqlstate().map(std::borrow::Cow::into_owned),
            }),
        }
    }

    async fn execute(&self, pool: &PgPool) -> Result<Executed, OperatorError> {
        match self {
            Self::Inspect(query) => {
                let result = infra_postgres::in_tx_with(
                    pool,
                    TxOptions {
                        isolation: Isolation::ReadCommitted,
                        read_only: true,
                    },
                    async |tx| jobs::inspect(tx, query).await,
                )
                .await?;
                Ok(Executed::Inspected(result))
            }
            Self::Redrive(target) => {
                let redriven =
                    infra_postgres::in_tx(pool, async |tx| jobs::redrive(tx, target).await).await?;
                Ok(Executed::Redriven {
                    new_version: redriven.new_version,
                })
            }
            Self::Discard(target) => {
                infra_postgres::in_tx(pool, async |tx| jobs::discard(tx, target).await).await?;
                Ok(Executed::Discarded)
            }
        }
    }
}

fn recovery_target(args: &RecoveryArgs) -> Result<RecoveryTarget, InputError> {
    RecoveryTarget::try_new(&args.id, &args.kind, &args.version)
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Ok,
    Found,
    Missing,
    Unavailable,
    Redriven,
    Discarded,
    Stale,
    Conflict,
    Failed,
    Unknown,
}

#[derive(Clone, Copy)]
enum Cause {
    Configuration,
    PostgresDisabled,
    Runtime,
    Signals,
    Dsn,
    DatabaseAdmission,
    MigrationHistory,
    SessionAdmission,
    Interrupted,
    Timeout,
}

impl Cause {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::PostgresDisabled => "postgres_disabled",
            Self::Runtime => "runtime",
            Self::Signals => "signals",
            Self::Dsn => "dsn",
            Self::DatabaseAdmission => "database_admission",
            Self::MigrationHistory => "migration_history",
            Self::SessionAdmission => "session_admission",
            Self::Interrupted => "interrupted",
            Self::Timeout => "timeout",
        }
    }
}

enum Executed {
    Inspected(InspectionResult),
    Redriven { new_version: String },
    Discarded,
}

struct Failure {
    outcome: Outcome,
    cause: &'static str,
    detail: Option<FailureDetail>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum FailureDetail {
    Admission { detail: &'static str },
    Database { sqlstate: Option<String> },
}

struct Report {
    result: Result<Executed, Failure>,
    cleanup_incomplete: bool,
}

impl From<Failure> for Report {
    fn from(failure: Failure) -> Self {
        Self {
            result: Err(failure),
            cleanup_incomplete: false,
        }
    }
}

impl Report {
    fn receipt<'a>(&'a self, request: &'a Request) -> Receipt<'a> {
        let (outcome, body) = match &self.result {
            Ok(Executed::Inspected(InspectionResult::One { observed_at, item })) => (
                if item.is_some() {
                    Outcome::Found
                } else {
                    Outcome::Missing
                },
                Some(ReceiptBody::One {
                    observed_at,
                    item: item.as_ref().map(Snapshot::from),
                }),
            ),
            Ok(Executed::Inspected(InspectionResult::Page {
                observed_at,
                scanned,
                items,
                next_cursor,
            })) => (
                Outcome::Ok,
                Some(ReceiptBody::Page {
                    observed_at,
                    scanned: *scanned,
                    items: items.iter().map(Snapshot::from).collect(),
                    complete: next_cursor.is_none(),
                    next_cursor: next_cursor.as_deref(),
                }),
            ),
            Ok(Executed::Redriven { new_version }) => (
                Outcome::Redriven,
                Some(ReceiptBody::Redriven { new_version }),
            ),
            Ok(Executed::Discarded) => (Outcome::Discarded, None),
            Err(Failure {
                outcome,
                cause,
                detail,
            }) => (
                *outcome,
                Some(ReceiptBody::Failed(FailureFields {
                    cause,
                    next_action: (*outcome == Outcome::Unknown)
                        .then_some("inspect the same id before any deliberate retry"),
                    detail: detail.as_ref(),
                })),
            ),
        };
        Receipt {
            schema_version: 1,
            action: request.action(),
            outcome,
            request: request.fields(),
            body,
            cleanup: self.cleanup_incomplete.then_some("incomplete"),
        }
    }
}

#[skip_serializing_none]
#[derive(Serialize)]
struct Receipt<'a> {
    schema_version: u8,
    action: &'static str,
    outcome: Outcome,
    #[serde(flatten)]
    request: RequestFields<'a>,
    #[serde(flatten)]
    body: Option<ReceiptBody<'a>>,
    cleanup: Option<&'static str>,
}

#[serde_as]
#[skip_serializing_none]
#[derive(Serialize)]
#[serde(untagged)]
enum RequestFields<'a> {
    Inspect {
        #[serde_as(as = "Option<DisplayFromStr>")]
        id: Option<JobId>,
        handled_kinds: Option<&'a BTreeSet<String>>,
    },
    Recovery {
        #[serde_as(as = "DisplayFromStr")]
        id: JobId,
        kind: &'a str,
        #[serde_as(as = "DisplayFromStr")]
        expected_version: i64,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
enum ReceiptBody<'a> {
    One {
        observed_at: &'a str,
        item: Option<Snapshot<'a>>,
    },
    Page {
        observed_at: &'a str,
        scanned: u16,
        items: Vec<Snapshot<'a>>,
        complete: bool,
        next_cursor: Option<&'a str>,
    },
    Redriven {
        new_version: &'a str,
    },
    Failed(FailureFields<'a>),
}

#[skip_serializing_none]
#[derive(Serialize)]
struct FailureFields<'a> {
    cause: &'static str,
    next_action: Option<&'static str>,
    #[serde(flatten)]
    detail: Option<&'a FailureDetail>,
}

#[expect(
    clippy::disallowed_methods,
    reason = "finite operator output is emitted after its runtime stops; this function owns the stdout lock"
)]
pub(crate) fn run(options: &LoadOptions, request: &Request) -> bool {
    let report = match service_config::load_jobs_operator(options) {
        Ok(config) => run_configured(config.postgres(), request),
        Err(JobsOperatorLoadError::PostgresDisabled) => {
            request.failed_before_start(Cause::PostgresDisabled).into()
        }
        Err(JobsOperatorLoadError::Config(error)) => {
            report_failure(&error);
            request.failed_before_start(Cause::Configuration).into()
        }
    };
    let success = report.result.is_ok() && !report.cleanup_incomplete;
    let receipt = report.receipt(request);
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &receipt).is_err()
        || writeln!(stdout).is_err()
        || stdout.flush().is_err()
    {
        report_failure(
            "operator receipt could not be written; inspect the same identity before retrying",
        );
        return false;
    }
    success
}

fn run_configured(config: &OperatorPostgresConfig, request: &Request) -> Report {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            report_failure(&error);
            return request.failed_before_start(Cause::Runtime).into();
        }
    };
    let report = runtime.block_on(run_request(config, request));
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    report
}

async fn run_request(config: &OperatorPostgresConfig, request: &Request) -> Report {
    let mut signals = match Signals::install() {
        Ok(signals) => signals,
        Err(error) => {
            report_failure(&error);
            return request.failed_before_start(Cause::Signals).into();
        }
    };
    let Ok(dsn) = Dsn::admit_for_environment(
        config.dsn.expose_secret(),
        config.password_file.as_deref(),
        config.local_development,
    ) else {
        return request.failed_before_start(Cause::Dsn).into();
    };
    let options = crate::bootstrap::postgres::pool_options(
        config.session_budgets,
        NonZeroU32::MIN,
        "jobs-worker-operator",
    );
    let pool = match bounded(
        &mut signals,
        jobs::STARTUP_TIMEOUT,
        Box::pin(infra_postgres::connect(&dsn, &options)),
    )
    .await
    {
        Ok(Ok(pool)) => pool,
        Ok(Err(error)) => {
            return request
                .refused(Cause::DatabaseAdmission, connect_detail(&error))
                .into();
        }
        Err(cause) => return request.failed_before_start(cause).into(),
    };
    let result = verify_and_execute(&pool, &mut signals, request).await;
    let cleanup_incomplete =
        infra_postgres::close(&pool, DEPENDENCY_CLOSE_BUDGET).await != CloseOutcome::Complete;
    Report {
        result,
        cleanup_incomplete,
    }
}

async fn verify_and_execute(
    pool: &PgPool,
    signals: &mut Signals,
    request: &Request,
) -> Result<Executed, Failure> {
    bounded(
        signals,
        migrate::HISTORY_VERIFY_BUDGET,
        migrate::verify_history(pool),
    )
    .await
    .map_err(|cause| request.failed_before_start(cause))?
    .map_err(|error| request.refused(Cause::MigrationHistory, history_detail(error)))?;
    bounded(signals, jobs::STARTUP_TIMEOUT, jobs::check_startup(pool))
        .await
        .map_err(|cause| request.failed_before_start(cause))?
        .map_err(|error| request.refused(Cause::SessionAdmission, startup_detail(error)))?;
    let mut invoked = false;
    let outcome = bounded(signals, jobs::OPERATION_TIMEOUT, async {
        invoked = true;
        request.execute(pool).await
    })
    .await;
    match outcome {
        Ok(result) => result.map_err(|error| request.database_error(&error)),
        Err(cause) if invoked => Err(request.failed_after_start(cause)),
        Err(cause) => Err(request.failed_before_start(cause)),
    }
}

const fn connect_detail(error: &ConnectError) -> &'static str {
    match error {
        ConnectError::Timeout { .. } => "timeout",
        ConnectError::SessionVerificationTimeout { .. } => "session_verification_timeout",
        ConnectError::Connect(_) => "connect",
        ConnectError::SessionBudget { .. } => "session_budget",
        ConnectError::SessionIsolation { .. } => "session_isolation",
    }
}

const fn history_detail(error: HistoryError) -> &'static str {
    match error {
        HistoryError::Pending => "pending",
        HistoryError::Mismatch => "mismatch",
        HistoryError::NotVisible => "not_visible",
        HistoryError::Unavailable => "unavailable",
    }
}

const fn startup_detail(error: StartupError) -> &'static str {
    match error {
        StartupError::UnsupportedEncoding => "unsupported_encoding",
        StartupError::NotWritable => "not_writable",
        StartupError::UnsupportedIsolation => "unsupported_isolation",
        StartupError::Unavailable => "unavailable",
    }
}

async fn bounded<T>(
    signals: &mut Signals,
    budget: Duration,
    operation: impl Future<Output = T>,
) -> Result<T, Cause> {
    if signals.take_pending().map_err(|_| Cause::Signals)? {
        return Err(Cause::Interrupted);
    }
    tokio::select! {
        biased;
        result = tokio::time::timeout(budget, operation) => result.map_err(|_| Cause::Timeout),
        result = signals.wait() => Err(if result.is_ok() { Cause::Interrupted } else { Cause::Signals }),
    }
}

#[serde_as]
#[derive(Serialize)]
struct Snapshot<'a> {
    #[serde_as(as = "DisplayFromStr")]
    id: JobId,
    kind: &'a str,
    state: &'static str,
    version: &'a str,
    attempts: i32,
    failure_reason: Option<&'static str>,
    created_at: &'a str,
    not_before: &'a str,
    claim_expires_at: Option<&'a str>,
    finished_at: Option<&'a str>,
    recovery_count: i32,
}

impl<'a> From<&'a JobSnapshot> for Snapshot<'a> {
    fn from(item: &'a JobSnapshot) -> Self {
        let JobSnapshot {
            id,
            kind,
            state,
            version,
            attempts,
            failure_reason,
            created_at,
            not_before,
            claim_expires_at,
            finished_at,
            recovery_count,
        } = item;
        Self {
            id: *id,
            kind,
            state: state.as_str(),
            version,
            attempts: *attempts,
            failure_reason: failure_reason.map(FailureReason::as_str),
            created_at,
            not_before,
            claim_expires_at: claim_expires_at.as_deref(),
            finished_at: finished_at.as_deref(),
            recovery_count: *recovery_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use infra_jobs::operator::JobState;
    use serde_json::{Value, json};

    use super::*;

    const ID: &str = "00000000-0000-4000-8000-000000000001";
    const OBSERVED_AT: &str = "2026-11-16T00:00:00.000000Z";

    fn render(request: &Request, report: &Report) -> Value {
        let mut out = Vec::new();
        serde_json::to_writer(&mut out, &report.receipt(request)).unwrap();
        serde_json::from_slice(&out).unwrap()
    }

    fn sample_snapshot() -> JobSnapshot {
        let id = Inspection::one(ID).unwrap().job_id().unwrap();
        JobSnapshot {
            id,
            kind: "jobs.sample".into(),
            state: JobState::Failed,
            version: "9007199254740993".into(),
            attempts: 3,
            failure_reason: Some(FailureReason::Exhausted),
            created_at: "2026-11-15T00:00:00.000000Z".into(),
            not_before: "2026-11-15T00:01:00.000000Z".into(),
            claim_expires_at: None,
            finished_at: Some("2026-11-15T00:02:00.000000Z".into()),
            recovery_count: 1,
        }
    }

    #[test]
    fn inspection_receipts_preserve_explicit_nulls_and_omit_absent_request_fields() {
        let one = Request::Inspect(Inspection::one(ID).unwrap());
        let found = Report {
            result: Ok(Executed::Inspected(InspectionResult::One {
                observed_at: OBSERVED_AT.into(),
                item: Some(sample_snapshot()),
            })),
            cleanup_incomplete: false,
        };
        assert_eq!(
            render(&one, &found),
            json!({
                "schema_version": 1,
                "action": "inspect",
                "outcome": "found",
                "id": ID,
                "observed_at": OBSERVED_AT,
                "item": {
                    "id": ID,
                    "kind": "jobs.sample",
                    "state": "failed",
                    "version": "9007199254740993",
                    "attempts": 3,
                    "failure_reason": "exhausted",
                    "created_at": "2026-11-15T00:00:00.000000Z",
                    "not_before": "2026-11-15T00:01:00.000000Z",
                    "claim_expires_at": null,
                    "finished_at": "2026-11-15T00:02:00.000000Z",
                    "recovery_count": 1,
                },
            })
        );

        let missing = Report {
            result: Ok(Executed::Inspected(InspectionResult::One {
                observed_at: OBSERVED_AT.into(),
                item: None,
            })),
            cleanup_incomplete: true,
        };
        assert_eq!(
            render(&one, &missing),
            json!({
                "schema_version": 1,
                "action": "inspect",
                "outcome": "missing",
                "id": ID,
                "observed_at": OBSERVED_AT,
                "item": null,
                "cleanup": "incomplete",
            })
        );

        let unhandled = Request::Inspect(Inspection::unhandled("zebra,alpha", None, 10).unwrap());
        let page = Report {
            result: Ok(Executed::Inspected(InspectionResult::Page {
                observed_at: OBSERVED_AT.into(),
                scanned: 2,
                items: Vec::new(),
                next_cursor: None,
            })),
            cleanup_incomplete: false,
        };
        assert_eq!(
            render(&unhandled, &page),
            json!({
                "schema_version": 1,
                "action": "unhandled",
                "outcome": "ok",
                "handled_kinds": ["alpha", "zebra"],
                "observed_at": OBSERVED_AT,
                "scanned": 2,
                "items": [],
                "complete": true,
                "next_cursor": null,
            })
        );
    }

    #[test]
    fn recovery_receipts_serialize_versions_as_strings() {
        let target = RecoveryTarget::try_new(ID, "jobs.sample", "9007199254740993").unwrap();
        let redrive = Request::Redrive(target.clone());
        let redriven = Report {
            result: Ok(Executed::Redriven {
                new_version: "9007199254740994".into(),
            }),
            cleanup_incomplete: false,
        };
        assert_eq!(
            render(&redrive, &redriven),
            json!({
                "schema_version": 1,
                "action": "redrive",
                "outcome": "redriven",
                "id": ID,
                "kind": "jobs.sample",
                "expected_version": "9007199254740993",
                "new_version": "9007199254740994",
            })
        );

        let discard = Request::Discard(target);
        let discarded = Report {
            result: Ok(Executed::Discarded),
            cleanup_incomplete: false,
        };
        assert_eq!(
            render(&discard, &discarded),
            json!({
                "schema_version": 1,
                "action": "discard",
                "outcome": "discarded",
                "id": ID,
                "kind": "jobs.sample",
                "expected_version": "9007199254740993",
            })
        );
    }

    #[test]
    fn failure_receipts_match_contract_variants() {
        let target = RecoveryTarget::try_new(ID, "jobs.sample", "9007199254740993").unwrap();
        let redrive = Request::Redrive(target);

        let refused: Report = redrive
            .refused(Cause::SessionAdmission, "not_writable")
            .into();
        assert_eq!(
            render(&redrive, &refused),
            json!({
                "schema_version": 1,
                "action": "redrive",
                "outcome": "failed",
                "id": ID,
                "kind": "jobs.sample",
                "expected_version": "9007199254740993",
                "cause": "session_admission",
                "detail": "not_writable",
            })
        );

        let unknown: Report = redrive.failed_after_start(Cause::Timeout).into();
        assert_eq!(
            render(&redrive, &unknown),
            json!({
                "schema_version": 1,
                "action": "redrive",
                "outcome": "unknown",
                "id": ID,
                "kind": "jobs.sample",
                "expected_version": "9007199254740993",
                "cause": "timeout",
                "next_action": "inspect the same id before any deliberate retry",
            })
        );

        let stale: Report = redrive.database_error(&OperatorError::Stale).into();
        assert_eq!(
            render(&redrive, &stale),
            json!({
                "schema_version": 1,
                "action": "redrive",
                "outcome": "stale",
                "id": ID,
                "kind": "jobs.sample",
                "expected_version": "9007199254740993",
                "cause": "stale",
                "sqlstate": null,
            })
        );

        let sqlstate_failure: Report = Failure {
            outcome: Outcome::Failed,
            cause: "database",
            detail: Some(FailureDetail::Database {
                sqlstate: Some("57014".into()),
            }),
        }
        .into();
        assert_eq!(
            render(&redrive, &sqlstate_failure),
            json!({
                "schema_version": 1,
                "action": "redrive",
                "outcome": "failed",
                "id": ID,
                "kind": "jobs.sample",
                "expected_version": "9007199254740993",
                "cause": "database",
                "sqlstate": "57014",
            })
        );
    }
}
