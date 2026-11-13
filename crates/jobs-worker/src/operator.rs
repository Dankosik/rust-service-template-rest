//! One PostgreSQL-only operation, followed by bounded dependency and runtime close.

use std::cell::Cell;
use std::io::Write;
use std::num::NonZeroU32;
use std::time::Duration;

use infra_jobs::StartupError;
use infra_jobs::operator::{
    self as jobs, InputError, Inspection, InspectionMode, InspectionResult, JobSnapshot,
    OperatorError, RecoveryTarget,
};
use infra_postgres::{CloseOutcome, ConnectError, Dsn, Isolation, PgPool, TxOptions};
use migrate::HistoryError;
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use service_config::{LoadOptions, PostgresConfig, process_failure};

use crate::cli::{OperatorCommand, RecoveryArgs};
use crate::shutdown::{DEPENDENCY_CLOSE, RUNTIME_SHUTDOWN_TIMEOUT};
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

    fn receipt(&self, outcome: Outcome) -> Value {
        let mut body = match self {
            Self::Inspect(query) => {
                let mut body = json!({});
                if let Some(kinds) = query.handled_kinds() {
                    body["handled_kinds"] = json!(kinds);
                }
                body
            }
            Self::Redrive(target) | Self::Discard(target) => json!({
                "id": target.id().to_string(),
                "kind": target.kind(),
                "expected_version": target.version().to_string(),
            }),
        };
        body["action"] = self.action().into();
        body["outcome"] = outcome.as_str().into();
        body["schema_version"] = 1.into();
        body
    }

    /// The operation was never invoked, so a mutation certainly did not commit.
    fn failed_before_start(&self, cause: Cause) -> FailureReceipt {
        FailureReceipt {
            outcome: if self.is_mutation() {
                Outcome::Failed
            } else {
                Outcome::Unavailable
            },
            cause: cause.as_str(),
            detail: FailureDetail::None,
        }
    }

    /// An admission check refused before the operation: `detail` names which
    /// refusal, since each one asks the operator for a different remedy.
    fn refused(&self, cause: Cause, detail: &'static str) -> FailureReceipt {
        FailureReceipt {
            detail: FailureDetail::Admission(detail),
            ..self.failed_before_start(cause)
        }
    }

    /// The operation may have run, so a mutation's commit is unknown.
    fn failed_after_start(&self, cause: Cause) -> FailureReceipt {
        FailureReceipt {
            outcome: if self.is_mutation() {
                Outcome::Unknown
            } else {
                Outcome::Unavailable
            },
            cause: cause.as_str(),
            detail: FailureDetail::None,
        }
    }

    fn database_error(&self, error: &OperatorError) -> FailureReceipt {
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
        FailureReceipt {
            outcome,
            cause: error.cause(),
            detail: FailureDetail::Database(error.sqlstate().map(std::borrow::Cow::into_owned)),
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

/// The receipt's closed `outcome` vocabulary.
#[derive(Clone, Copy, PartialEq, Eq)]
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

impl Outcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Found => "found",
            Self::Missing => "missing",
            Self::Unavailable => "unavailable",
            Self::Redriven => "redriven",
            Self::Discarded => "discarded",
            Self::Stale => "stale",
            Self::Conflict => "conflict",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

/// The receipt's closed `cause` vocabulary for failures the operator detects
/// itself; database failures carry [`OperatorError::cause`] instead.
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

/// What an acknowledged operation established.
enum Executed {
    Inspected(InspectionResult),
    Redriven { new_version: String },
    Discarded,
}

struct FailureReceipt {
    outcome: Outcome,
    /// The bounded code from [`Cause`] or [`OperatorError::cause`].
    cause: &'static str,
    detail: FailureDetail,
}

enum FailureDetail {
    None,
    Admission(&'static str),
    Database(Option<String>),
}

struct Report {
    result: Result<Executed, FailureReceipt>,
    cleanup_incomplete: bool,
}

impl From<FailureReceipt> for Report {
    fn from(failure: FailureReceipt) -> Self {
        Self {
            result: Err(failure),
            cleanup_incomplete: false,
        }
    }
}

impl Report {
    fn into_receipt(self, request: &Request) -> Value {
        let mut body = match self.result {
            Ok(Executed::Inspected(InspectionResult::One { observed_at, item })) => {
                let outcome = if item.is_some() {
                    Outcome::Found
                } else {
                    Outcome::Missing
                };
                let mut body = request.receipt(outcome);
                body["observed_at"] = observed_at.into();
                body["item"] = json!(item.as_ref().map(snapshot));
                body
            }
            Ok(Executed::Inspected(InspectionResult::Page {
                observed_at,
                scanned,
                items,
                next_cursor,
            })) => {
                let mut body = request.receipt(Outcome::Ok);
                body["observed_at"] = observed_at.into();
                body["scanned"] = scanned.into();
                body["items"] = items.iter().map(snapshot).collect();
                body["complete"] = next_cursor.is_none().into();
                body["next_cursor"] = json!(next_cursor);
                body
            }
            Ok(Executed::Redriven { new_version }) => {
                let mut body = request.receipt(Outcome::Redriven);
                body["new_version"] = new_version.into();
                body
            }
            Ok(Executed::Discarded) => request.receipt(Outcome::Discarded),
            Err(FailureReceipt {
                outcome,
                cause,
                detail,
            }) => {
                let mut body = request.receipt(outcome);
                body["cause"] = cause.into();
                if outcome == Outcome::Unknown {
                    body["next_action"] = "inspect the same id before any deliberate retry".into();
                }
                match detail {
                    FailureDetail::None => {}
                    FailureDetail::Admission(detail) => body["detail"] = detail.into(),
                    FailureDetail::Database(sqlstate) => body["sqlstate"] = json!(sqlstate),
                }
                body
            }
        };
        if self.cleanup_incomplete {
            body["cleanup"] = "incomplete".into();
        }
        body
    }
}

/// Returns process success, never maps an exit code or retries an operation.
#[expect(
    clippy::disallowed_methods,
    reason = "finite operator output is emitted after its runtime stops; this function owns the stdout lock"
)]
pub(crate) fn run(options: &LoadOptions, request: &Request) -> bool {
    let report = match service_config::load_jobs_operator(options) {
        Ok(config) => run_configured(&config.postgres, request),
        Err(error) => {
            let _ = process_failure(&error);
            request.failed_before_start(Cause::Configuration).into()
        }
    };
    let success = report.result.is_ok() && !report.cleanup_incomplete;
    let receipt = report.into_receipt(request);
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &receipt).is_err()
        || writeln!(stdout).is_err()
        || stdout.flush().is_err()
    {
        let _ = process_failure(
            "operator receipt could not be written; inspect the same identity before retrying",
        );
        return false;
    }
    success
}

fn run_configured(config: &PostgresConfig, request: &Request) -> Report {
    if !config.enabled {
        return request.failed_before_start(Cause::PostgresDisabled).into();
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = process_failure(&error);
            return request.failed_before_start(Cause::Runtime).into();
        }
    };
    let report = runtime.block_on(serve(config, request));
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    report
}

async fn serve(config: &PostgresConfig, request: &Request) -> Report {
    let mut signals = match Signals::install() {
        Ok(signals) => signals,
        Err(error) => {
            let _ = process_failure(&error);
            return request.failed_before_start(Cause::Signals).into();
        }
    };
    let Ok(raw) = config.required_dsn() else {
        return request.failed_before_start(Cause::Dsn).into();
    };
    let Ok(dsn) = Dsn::admit_with(raw.expose_secret(), config.password_file.as_deref()) else {
        // Do not repeat an arbitrary URL parameter from DsnError::Parameter.
        return request.failed_before_start(Cause::Dsn).into();
    };
    let options =
        crate::bootstrap::postgres::pool_options(config, NonZeroU32::MIN, "jobs-worker-operator");
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
    // A missing close acknowledgement never changes a known commit outcome.
    let cleanup_incomplete =
        infra_postgres::close(&pool, DEPENDENCY_CLOSE).await != CloseOutcome::Complete;
    Report {
        result,
        cleanup_incomplete,
    }
}

async fn verify_and_execute(
    pool: &PgPool,
    signals: &mut Signals,
    request: &Request,
) -> Result<Executed, FailureReceipt> {
    match bounded(
        signals,
        migrate::HISTORY_VERIFY_BUDGET,
        migrate::verify_history(pool),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return Err(request.refused(Cause::MigrationHistory, history_detail(error)));
        }
        Err(cause) => return Err(request.failed_before_start(cause)),
    }
    match bounded(signals, jobs::STARTUP_TIMEOUT, jobs::check_startup(pool)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return Err(request.refused(Cause::SessionAdmission, startup_detail(error)));
        }
        Err(cause) => return Err(request.failed_before_start(cause)),
    }
    let invoked = Cell::new(false);
    let operation = async {
        invoked.set(true);
        request.execute(pool).await
    };
    match bounded(signals, jobs::OPERATION_TIMEOUT, operation).await {
        Ok(result) => result.map_err(|error| request.database_error(&error)),
        Err(cause) if invoked.get() => Err(request.failed_after_start(cause)),
        Err(cause) => Err(request.failed_before_start(cause)),
    }
}

// Fixed names rather than the errors' text: a driver message can carry
// server-side detail, and the receipt promises bounded diagnostics.
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

/// Prefer an already observed result to cancellation; pending stops prevent
/// invoking the next stage. Dropping the future precedes dependency close.
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

fn snapshot(item: &JobSnapshot) -> Value {
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
    json!({
        "id": id.to_string(),
        "kind": kind,
        "state": state.as_str(),
        "version": version,
        "attempts": attempts,
        "failure_reason": failure_reason.as_ref().map(|reason| reason.as_str()),
        "created_at": created_at,
        "not_before": not_before,
        "claim_expires_at": claim_expires_at,
        "finished_at": finished_at,
        "recovery_count": recovery_count,
    })
}
