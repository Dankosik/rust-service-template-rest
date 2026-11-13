//! `migrate`: apply the embedded migrations to the configured database.
//!
//! Reads the service's configuration sources (`--config`, overlays,
//! `APP__*`) and decodes only the sections it uses, so a migration run is
//! attributable to the same service, version, and environment as the
//! process it prepares the schema for and needs no secret but the DSN.
//! Requires the PostgreSQL profile to be enabled. Once the logger is installed,
//! the CLI submits one terminal `migration_run` record. Configuration or
//! logger setup failures use a default JSON logger. If that logger cannot be
//! installed, the original error goes to stderr. The CLI exits 0 on success
//! or no change and 1 otherwise (clap flag errors keep clap's 0/2 convention).
//! A stop signal drops the run, and the server ends the session, lock, and
//! transaction.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use infra_postgres::{Dsn, DsnError};
use infra_telemetry::{
    LoggerGuard, LoggingFormat, LoggingOptions, install_panic_hook, install_subscriber,
};
use migrate::{MIGRATOR, Report, RunFailure, RunOptions};
use secrecy::ExposeSecret;
use service_config::{BuildInfo, LoadOptions, MigrationConfig, ValidationError, process_failure};

const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);

const FALLBACK_LOGGING: LoggingOptions<'static> = LoggingOptions {
    level: "info,migrate_record=info",
    format: LoggingFormat::Json,
    tracer_provider: None,
};

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error("postgres.enabled must be true to run migrations")]
    PostgresDisabled,
    #[error(transparent)]
    Config(#[from] ValidationError),
    #[error(transparent)]
    Dsn(#[from] DsnError),
    #[error("build tokio runtime: {0}")]
    Runtime(std::io::Error),
    #[error("install stop signal handlers: {0}")]
    Signals(std::io::Error),
    /// Configuration or logging setup failed before a migration session.
    #[error("{0}")]
    Setup(String),
    #[error(transparent)]
    Run(#[from] RunFailure),
    #[error("interrupted by {0}")]
    Interrupted(&'static str),
}

impl Failure {
    fn stage(&self) -> &'static str {
        match self {
            Self::PostgresDisabled | Self::Config(_) | Self::Dsn(_) | Self::Setup(_) => "config",
            Self::Runtime(_) => "runtime",
            Self::Signals(_) => "signals",
            Self::Run(error) => error.stage(),
            Self::Interrupted(_) => "interrupted",
        }
    }

    fn report(&self) -> Option<&Report> {
        match self {
            Self::Run(failure) => Some(&failure.report),
            _ => None,
        }
    }
}

fn main() -> ExitCode {
    let started = Instant::now();
    let target = MIGRATOR.iter().map(|migration| migration.version).max();
    let options = LoadOptions::parse_from(std::env::args_os());
    let config = match service_config::load_migration(&options, BUILD_INFO) {
        Ok(config) => config,
        Err(err) => return finish_config_failure(err.to_string(), target, started),
    };
    // The migration binary's own records survive a quieter service directive:
    // operators and deploy hooks read them, including the terminal record.
    let level = format!("{},migrate_record=info", config.log.level);
    let logger = match install_subscriber(&LoggingOptions {
        level: &level,
        format: match config.log.format {
            service_config::LogFormat::Json => LoggingFormat::Json,
            service_config::LogFormat::Text => LoggingFormat::Text,
        },
        tracer_provider: None,
    }) {
        Ok(logger) => logger,
        Err(err) => {
            let Ok(fallback) = install_subscriber(&FALLBACK_LOGGING) else {
                return process_failure(&err);
            };
            return finish_run(
                None,
                fallback,
                Err(Failure::Setup(format!("install logger: {err}"))),
                target,
                started,
            );
        }
    };
    install_panic_hook();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            return finish_run(None, logger, Err(Failure::Runtime(err)), target, started);
        }
    };

    let outcome = runtime.block_on(apply(&config, target));
    finish_run(Some(runtime), logger, outcome, target, started)
}

fn finish_config_failure(message: String, target: Option<i64>, started: Instant) -> ExitCode {
    let Ok(logger) = install_subscriber(&FALLBACK_LOGGING) else {
        return process_failure(&message);
    };
    finish_run(None, logger, Err(Failure::Setup(message)), target, started)
}

fn finish_run(
    runtime: Option<tokio::runtime::Runtime>,
    logger: LoggerGuard,
    outcome: Result<Report, Failure>,
    target: Option<i64>,
    started: Instant,
) -> ExitCode {
    let cleanup_deadline = Instant::now() + CLEANUP_TIMEOUT;
    logger.begin_shutdown();
    // Submit the terminal record before runtime shutdown can spend the shared
    // cleanup allowance on detached blocking work such as a DNS lookup.
    let primary = finish(outcome, target, started);
    if let Some(runtime) = runtime {
        runtime.shutdown_timeout(cleanup_deadline.saturating_duration_since(Instant::now()));
    }
    // The snapshot remains local: another record after closing would require
    // another drain. Telemetry loss cannot change or replay the database result.
    let _ = logger.shutdown(cleanup_deadline);
    primary
}

fn finish(outcome: Result<Report, Failure>, target: Option<i64>, started: Instant) -> ExitCode {
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    // One terminal record per run, read by operators and deploy hooks. An
    // absent version field means none exists or it was not observed.
    match outcome {
        Ok(report) => {
            tracing::event!(
                target: "migrate_record",
                tracing::Level::INFO,
                migration.before = report.before,
                migration.target = target,
                migration.after = report.after(),
                migration.applied_count = report.applied.len(),
                migration.duration_ms = duration_ms,
                outcome = if report.applied.is_empty() {
                    "no_change"
                } else {
                    "success"
                },
                "migration_run"
            );
            ExitCode::SUCCESS
        }
        Err(failure) => {
            let report = failure.report();
            tracing::event!(
                target: "migrate_record",
                tracing::Level::ERROR,
                migration.before = report.and_then(|report| report.before),
                migration.target = target,
                migration.after = report.and_then(Report::after),
                migration.applied_count = report.map(|report| report.applied.len()),
                migration.duration_ms = duration_ms,
                outcome = "error",
                stage = failure.stage(),
                error = %failure,
                "migration_run"
            );
            ExitCode::FAILURE
        }
    }
}

async fn apply(config: &MigrationConfig, target: Option<i64>) -> Result<Report, Failure> {
    if !config.postgres.enabled {
        return Err(Failure::PostgresDisabled);
    }
    let dsn = Dsn::admit_with(
        config.postgres.required_dsn()?.expose_secret(),
        config.postgres.password_file.as_deref(),
    )?;
    tracing::event!(
        target: "migrate_record",
        tracing::Level::INFO,
        app.env = %config.app.env,
        app.version = %config.app.version,
        app.commit = %config.app.commit,
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        migration.target = target,
        migration.deadline_ms =
            u64::try_from(config.postgres.migration_deadline.as_millis()).unwrap_or(u64::MAX),
        "migration_starting"
    );
    // Same service identity as traces; not a separate Postgres label.
    let options = RunOptions::defaults(
        &dsn,
        &config.observability.otel.service_name,
        config.postgres.migration_deadline,
    );
    run_until_stop(&options).await
}

/// Apply migrations unless a stop signal arrives first. Dropping the run
/// drops the connection; the server then ends the session, the advisory
/// lock, and any open transaction.
async fn run_until_stop(options: &RunOptions<'_>) -> Result<Report, Failure> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        // SIGINT first, as in the service: a failed SIGTERM install then
        // cannot drop a live SIGTERM stream, which would swallow the signal.
        let mut interrupt = signal(SignalKind::interrupt()).map_err(Failure::Signals)?;
        let mut terminate = signal(SignalKind::terminate()).map_err(Failure::Signals)?;
        tokio::select! {
            biased;
            result = migrate::run(&MIGRATOR, options) => Ok(result?),
            _ = terminate.recv() => Err(Failure::Interrupted("SIGTERM")),
            _ = interrupt.recv() => Err(Failure::Interrupted("SIGINT")),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            biased;
            result = migrate::run(&MIGRATOR, options) => Ok(result?),
            _ = tokio::signal::ctrl_c() => Err(Failure::Interrupted("ctrl-c")),
        }
    }
}

#[cfg(all(test, unix))]
#[path = "terminal_tests.rs"]
mod tests;
