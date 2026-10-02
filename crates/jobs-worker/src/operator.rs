//! One PostgreSQL-only operation, followed by bounded dependency and runtime close.

use std::cell::Cell;
use std::io::Write;
use std::num::NonZeroU32;
use std::time::Duration;

use infra_jobs::operator::{
    self as jobs, InputError, Inspection, InspectionResult, JobSnapshot, OperatorError,
    RecoveryTarget,
};
use infra_postgres::{Closed, Dsn, Isolation, PgPool, PoolOptions, SessionBudgets, TxOptions};
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use service_config::{LoadOptions, PostgresConfig, PostgresSessionBudgets, process_failure};

use crate::cli::OperatorCommand;
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
    pub(crate) fn new(command: OperatorCommand) -> Result<Self, InputError> {
        Ok(match command {
            OperatorCommand::Inspect { id } => Self::Inspect {
                action: "inspect",
                query: Inspection::one(&id)?,
            },
            OperatorCommand::Failed { after, limit } => Self::Inspect {
                action: "failed",
                query: Inspection::failed(after.as_deref(), limit)?,
            },
            OperatorCommand::Unhandled {
                handled_kinds,
                after,
                limit,
            } => Self::Inspect {
                action: "unhandled",
                query: Inspection::unhandled(&handled_kinds, after.as_deref(), limit)?,
            },
            OperatorCommand::Redrive { id, kind, version } => {
                Self::Redrive(RecoveryTarget::new(&id, &kind, &version)?)
            }
            OperatorCommand::Discard { id, kind, version } => {
                Self::Discard(RecoveryTarget::new(&id, &kind, &version)?)
            }
        })
    }

    fn mutation(&self) -> bool {
        !matches!(self, Self::Inspect { .. })
    }

    fn receipt(&self, outcome: &str) -> Value {
        match self {
            Self::Inspect { action, .. } => json!({"action": action, "outcome": outcome}),
            Self::Redrive(target) | Self::Discard(target) => json!({
                "action": if matches!(self, Self::Redrive(_)) { "redrive" } else { "discard" },
                "id": target.id().to_string(),
                "kind": target.kind(),
                "expected_version": target.version().to_string(),
                "outcome": outcome,
            }),
        }
    }

    fn failed(&self, cause: &'static str, invoked: bool) -> Report {
        let outcome = if !self.mutation() {
            "unavailable"
        } else if invoked {
            "unknown"
        } else {
            "failed"
        };
        let mut body = self.receipt(outcome);
        body["cause"] = cause.into();
        if outcome == "unknown" {
            body["next_action"] = "inspect the same id before any deliberate retry".into();
        }
        Report {
            body,
            success: false,
        }
    }

    fn database_error(&self, error: &OperatorError) -> Report {
        if !self.mutation() {
            let mut report = self.failed(error.cause(), false);
            report.body["sqlstate"] = json!(error.sqlstate());
            return report;
        }
        let outcome = match error {
            OperatorError::Missing => "missing",
            OperatorError::Stale => "stale",
            OperatorError::Conflict => "conflict",
            _ if error.is_commit_unknown() => "unknown",
            _ => "failed",
        };
        let mut body = self.receipt(outcome);
        body["cause"] = error.cause().into();
        body["sqlstate"] = json!(error.sqlstate());
        if outcome == "unknown" {
            body["next_action"] = "inspect the same id before any deliberate retry".into();
        }
        Report {
            body,
            success: false,
        }
    }

    async fn execute(&self, pool: &PgPool) -> Result<Value, OperatorError> {
        match self {
            Self::Inspect { action, query } => {
                let result = infra_postgres::in_tx_with(
                    pool,
                    TxOptions {
                        isolation: Isolation::ReadCommitted,
                        read_only: true,
                    },
                    async |tx| jobs::inspect(tx, query).await,
                )
                .await?;
                Ok(inspection(action, result))
            }
            Self::Redrive(target) => {
                let redriven =
                    infra_postgres::in_tx(pool, async |tx| jobs::redrive(tx, target).await).await?;
                let mut body = self.receipt("redriven");
                body["new_version"] = redriven.new_version.into();
                Ok(body)
            }
            Self::Discard(target) => {
                infra_postgres::in_tx(pool, async |tx| jobs::discard(tx, target).await).await?;
                Ok(self.receipt("discarded"))
            }
        }
    }
}

struct Report {
    body: Value,
    success: bool,
}

/// Returns process success, never maps an exit code or retries an operation.
pub(crate) fn run(options: &LoadOptions, request: Request) -> bool {
    let report = match service_config::load_jobs_operator(options) {
        Ok(config) => run_configured(&config.postgres, &request),
        Err(error) => {
            let _ = process_failure(&error.to_string());
            request.failed("configuration", false)
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
        return request.failed("postgres_disabled", false);
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return request.failed("runtime", false),
    };
    let report = runtime.block_on(serve(config, request));
    runtime.shutdown_timeout(crate::RUNTIME_SHUTDOWN_TIMEOUT);
    report
}

async fn serve(config: &PostgresConfig, request: &Request) -> Report {
    let mut signals = match Signals::install() {
        Ok(signals) => signals,
        Err(_) => return request.failed("signals", false),
    };
    let raw = match config.required_dsn() {
        Ok(raw) => raw,
        Err(_) => return request.failed("dsn", false),
    };
    let dsn = match Dsn::admit_with(raw.expose_secret(), config.password_file.as_deref()) {
        Ok(dsn) => dsn,
        // Do not repeat an arbitrary URL parameter from DsnError::Parameter.
        Err(_) => return request.failed("dsn", false),
    };
    let options = PoolOptions {
        max_connections: NonZeroU32::MIN,
        application_name: "jobs-worker-operator",
        default_isolation: Isolation::ReadCommitted,
        session_budgets: match config.session_budgets {
            PostgresSessionBudgets::Startup => SessionBudgets::Startup,
            PostgresSessionBudgets::Server => SessionBudgets::Server,
        },
    };
    let pool = match bounded(
        &mut signals,
        jobs::STARTUP_TIMEOUT,
        infra_postgres::connect(&dsn, &options),
    )
    .await
    {
        Ok(Ok(pool)) => pool,
        Ok(Err(_)) => return request.failed("database_admission", false),
        Err(cause) => return request.failed(cause, false),
    };
    let mut report = admitted(&pool, &mut signals, request).await;
    if infra_postgres::close(&pool, DEPENDENCY_CLOSE).await != Closed::Complete {
        // A missing close acknowledgement never changes a known commit outcome.
        report.body["cleanup"] = "incomplete".into();
        report.success = false;
    }
    report
}

async fn admitted(pool: &PgPool, signals: &mut Signals, request: &Request) -> Report {
    match bounded(
        signals,
        migrate::HISTORY_VERIFY_BUDGET,
        migrate::verify_history(pool),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(_)) => return request.failed("migration_history", false),
        Err(cause) => return request.failed(cause, false),
    }
    match bounded(
        signals,
        jobs::STARTUP_TIMEOUT,
        jobs::check_startup(pool, request.mutation()),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(_)) => return request.failed("session_admission", false),
        Err(cause) => return request.failed(cause, false),
    }
    let invoked = Cell::new(false);
    let operation = async {
        invoked.set(true);
        request.execute(pool).await
    };
    match bounded(signals, jobs::OPERATION_TIMEOUT, operation).await {
        Ok(Ok(body)) => Report {
            body,
            success: true,
        },
        Ok(Err(error)) => request.database_error(&error),
        Err(cause) => request.failed(cause, invoked.get()),
    }
}

/// Prefer an already observed result to cancellation; pending stops prevent
/// invoking the next stage. Dropping the future precedes dependency close.
async fn bounded<T>(
    signals: &mut Signals,
    budget: Duration,
    operation: impl Future<Output = T>,
) -> Result<T, &'static str> {
    if signals.pending() {
        return Err("interrupted");
    }
    tokio::select! {
        biased;
        result = tokio::time::timeout(budget, operation) => result.map_err(|_| "timeout"),
        () = signals.wait() => Err("interrupted"),
    }
}

fn inspection(action: &str, result: InspectionResult) -> Value {
    match result {
        InspectionResult::One { observed_at, item } => json!({
            "action": action,
            "outcome": if item.is_some() { "found" } else { "missing" },
            "observed_at": observed_at,
            "item": item.as_ref().map(snapshot),
        }),
        InspectionResult::Page {
            observed_at,
            scanned,
            items,
            complete,
            next_cursor,
        } => json!({
            "action": action,
            "outcome": "ok",
            "observed_at": observed_at,
            "scanned": scanned,
            "items": items.iter().map(snapshot).collect::<Vec<_>>(),
            "complete": complete,
            "next_cursor": next_cursor,
        }),
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
