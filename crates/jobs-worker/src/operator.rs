//! One PostgreSQL-only operation, followed by bounded dependency and runtime close.

use std::cell::Cell;
use std::io::Write;
use std::num::NonZeroU32;
use std::time::Duration;

use infra_jobs::StartupError;
use infra_jobs::operator::{
    self as jobs, InputError, Inspection, InspectionResult, JobSnapshot, OperatorError,
    RecoveryTarget,
};
use infra_postgres::{Closed, ConnectError, Dsn, Isolation, PgPool, TxOptions};
use migrate::HistoryError;
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use service_config::{LoadOptions, PostgresConfig, process_failure};

use crate::cli::{OperatorCommand, RecoveryArgs};
use crate::shutdown::{DEPENDENCY_CLOSE, Signals};

pub(crate) enum Request {
    Inspect {
        action: &'static str,
        query: Inspection,
    },
    Redrive(RecoveryTarget),
    Discard(RecoveryTarget),
}

impl Request {
    pub(crate) fn try_new(command: OperatorCommand) -> Result<Self, InputError> {
        Ok(match command {
            OperatorCommand::Inspect { id } => Self::Inspect {
                action: "inspect",
                query: Inspection::one(&id)?,
            },
            OperatorCommand::Failed { page } => Self::Inspect {
                action: "failed",
                query: Inspection::failed(page.after.as_deref(), page.limit)?,
            },
            OperatorCommand::Unhandled {
                handled_kinds,
                page,
            } => Self::Inspect {
                action: "unhandled",
                query: Inspection::unhandled(&handled_kinds, page.after.as_deref(), page.limit)?,
            },
            OperatorCommand::Redrive(target) => Self::Redrive(recovery_target(&target)?),
            OperatorCommand::Discard(target) => Self::Discard(recovery_target(&target)?),
        })
    }

    fn action(&self) -> &'static str {
        match self {
            Self::Inspect { action, .. } => action,
            Self::Redrive(_) => "redrive",
            Self::Discard(_) => "discard",
        }
    }

    fn mutation(&self) -> bool {
        match self {
            Self::Inspect { .. } => false,
            Self::Redrive(_) | Self::Discard(_) => true,
        }
    }

    fn receipt(&self, outcome: Outcome) -> Value {
        let mut body = match self {
            Self::Inspect { query, .. } => {
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

    fn failure(&self, outcome: Outcome, cause: &'static str) -> Report {
        let mut body = self.receipt(outcome);
        body["cause"] = cause.into();
        if outcome == Outcome::Unknown {
            body["next_action"] = "inspect the same id before any deliberate retry".into();
        }
        Report {
            body,
            success: false,
        }
    }

    /// The operation was never invoked, so a mutation certainly did not commit.
    fn failed(&self, cause: &'static str) -> Report {
        let outcome = if self.mutation() {
            Outcome::Failed
        } else {
            Outcome::Unavailable
        };
        self.failure(outcome, cause)
    }

    /// An admission check refused before the operation: `detail` names which
    /// refusal, since each one asks the operator for a different remedy.
    fn refused(&self, cause: &'static str, detail: &'static str) -> Report {
        let mut report = self.failed(cause);
        report.body["detail"] = detail.into();
        report
    }

    /// The operation may have run, so a mutation's commit is unknown.
    fn failed_after_start(&self, cause: &'static str) -> Report {
        let outcome = if self.mutation() {
            Outcome::Unknown
        } else {
            Outcome::Unavailable
        };
        self.failure(outcome, cause)
    }

    fn database_error(&self, error: &OperatorError) -> Report {
        let outcome = if self.mutation() {
            match error {
                OperatorError::Missing => Outcome::Missing,
                OperatorError::Stale => Outcome::Stale,
                OperatorError::Conflict => Outcome::Conflict,
                _ if error.is_commit_unknown() => Outcome::Unknown,
                _ => Outcome::Failed,
            }
        } else {
            Outcome::Unavailable
        };
        let mut report = self.failure(outcome, error.cause());
        report.body["sqlstate"] = json!(error.sqlstate());
        report
    }

    fn succeeded(&self, executed: Executed) -> Report {
        let body = match executed {
            Executed::Inspected(InspectionResult::One { observed_at, item }) => {
                let outcome = if item.is_some() {
                    Outcome::Found
                } else {
                    Outcome::Missing
                };
                let mut body = self.receipt(outcome);
                body["observed_at"] = observed_at.into();
                body["item"] = json!(item.as_ref().map(snapshot));
                body
            }
            Executed::Inspected(InspectionResult::Page {
                observed_at,
                scanned,
                items,
                complete,
                next_cursor,
            }) => {
                let mut body = self.receipt(Outcome::Ok);
                body["observed_at"] = observed_at.into();
                body["scanned"] = scanned.into();
                body["items"] = items.iter().map(snapshot).collect();
                body["complete"] = complete.into();
                body["next_cursor"] = json!(next_cursor);
                body
            }
            Executed::Redriven { new_version } => {
                let mut body = self.receipt(Outcome::Redriven);
                body["new_version"] = new_version.into();
                body
            }
            Executed::Discarded => self.receipt(Outcome::Discarded),
        };
        Report {
            body,
            success: true,
        }
    }

    async fn execute(&self, pool: &PgPool) -> Result<Executed, OperatorError> {
        match self {
            Self::Inspect { query, .. } => {
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

/// What an acknowledged operation established.
enum Executed {
    Inspected(InspectionResult),
    Redriven { new_version: String },
    Discarded,
}

struct Report {
    body: Value,
    success: bool,
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
            request.failed("configuration")
        }
    };
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &report.body).is_err()
        || writeln!(stdout).is_err()
        || stdout.flush().is_err()
    {
        let _ = process_failure(
            "operator receipt could not be written; inspect the same identity before retrying",
        );
        return false;
    }
    report.success
}

fn run_configured(config: &PostgresConfig, request: &Request) -> Report {
    if !config.enabled {
        return request.failed("postgres_disabled");
    }
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return request.failed("runtime");
    };
    let report = runtime.block_on(serve(config, request));
    runtime.shutdown_timeout(crate::RUNTIME_SHUTDOWN_TIMEOUT);
    report
}

async fn serve(config: &PostgresConfig, request: &Request) -> Report {
    let Ok(mut signals) = Signals::install() else {
        return request.failed("signals");
    };
    let Ok(raw) = config.required_dsn() else {
        return request.failed("dsn");
    };
    let Ok(dsn) = Dsn::admit_with(raw.expose_secret(), config.password_file.as_deref()) else {
        // Do not repeat an arbitrary URL parameter from DsnError::Parameter.
        return request.failed("dsn");
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
        Ok(Err(error)) => return request.refused("database_admission", connect_detail(&error)),
        Err(cause) => return request.failed(cause),
    };
    let mut report = verify_and_execute(&pool, &mut signals, request).await;
    if infra_postgres::close(&pool, DEPENDENCY_CLOSE).await != Closed::Complete {
        // A missing close acknowledgement never changes a known commit outcome.
        report.body["cleanup"] = "incomplete".into();
        report.success = false;
    }
    report
}

async fn verify_and_execute(pool: &PgPool, signals: &mut Signals, request: &Request) -> Report {
    match bounded(
        signals,
        migrate::HISTORY_VERIFY_BUDGET,
        migrate::verify_history(pool),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return request.refused("migration_history", history_detail(error)),
        Err(cause) => return request.failed(cause),
    }
    match bounded(signals, jobs::STARTUP_TIMEOUT, jobs::check_startup(pool)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return request.refused("session_admission", startup_detail(error)),
        Err(cause) => return request.failed(cause),
    }
    let invoked = Cell::new(false);
    let operation = async {
        invoked.set(true);
        request.execute(pool).await
    };
    match bounded(signals, jobs::OPERATION_TIMEOUT, operation).await {
        Ok(Ok(executed)) => request.succeeded(executed),
        Ok(Err(error)) => request.database_error(&error),
        Err(cause) if invoked.get() => request.failed_after_start(cause),
        Err(cause) => request.failed(cause),
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
) -> Result<T, &'static str> {
    if signals.pending().map_err(|_| "signals")? {
        return Err("interrupted");
    }
    tokio::select! {
        biased;
        result = tokio::time::timeout(budget, operation) => result.map_err(|_| "timeout"),
        result = signals.wait() => Err(if result.is_ok() { "interrupted" } else { "signals" }),
    }
}

fn snapshot(item: &JobSnapshot) -> Value {
    json!({
        "id": item.id.to_string(),
        "kind": item.kind,
        "state": item.state.as_str(),
        "version": item.version,
        "attempts": item.attempts,
        "failure_reason": item.failure_reason.as_ref().map(|reason| reason.as_str()),
        "created_at": item.created_at,
        "not_before": item.not_before,
        "claim_expires_at": item.claim_expires_at,
        "finished_at": item.finished_at,
        "recovery_count": item.recovery_count,
    })
}
