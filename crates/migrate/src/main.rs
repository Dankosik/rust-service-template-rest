//! `migrate`: apply the embedded migrations to the configured database.
//!
//! Reads the service's configuration sources (`--config`, overlays,
//! `APP__*`) and decodes only the sections it uses, so a migration run is
//! attributable to the same service, version, and environment as the
//! process it prepares the schema for and needs no secret but the DSN.
//! Requires the PostgreSQL profile to be enabled; writes one terminal
//! `migration_run` record; exits 0 on success or no change, 1 otherwise.
//! A stop signal drops the run, the server ends the session, lock, and
//! transaction.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use infra_postgres::{Dsn, DsnError};
use infra_telemetry::{
    LoggerGuard, LoggingFormat, LoggingOptions, install_panic_hook, install_subscriber,
};
use migrate::{MIGRATOR, Report, RunError, RunOptions};
use secrecy::ExposeSecret;
use service_config::{BuildInfo, LoadOptions, MigrationConfig, ValidationError, process_failure};

const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);

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
    #[error(transparent)]
    Run(#[from] RunError),
    #[error("interrupted by {0}")]
    Interrupted(&'static str),
}

impl Failure {
    fn stage(&self) -> &'static str {
        match self {
            Self::PostgresDisabled | Self::Config(_) | Self::Dsn(_) => "config",
            Self::Runtime(_) => "runtime",
            Self::Signals(_) => "signals",
            Self::Run(error) => error.stage(),
            Self::Interrupted(_) => "interrupted",
        }
    }
}

fn main() -> ExitCode {
    let options = LoadOptions::parse_from(std::env::args_os());
    let config = match service_config::load_migration(&options, BUILD_INFO) {
        Ok(config) => config,
        Err(err) => return process_failure(&err.to_string()),
    };
    let logger = match install_subscriber(&LoggingOptions {
        level: &config.log.level,
        format: match config.log.format {
            service_config::LogFormat::Json => LoggingFormat::Json,
            service_config::LogFormat::Text => LoggingFormat::Text,
        },
        tracer_provider: None,
    }) {
        Ok(logger) => logger,
        Err(err) => return process_failure(&err.to_string()),
    };
    install_panic_hook();
    let target = MIGRATOR.iter().map(|migration| migration.version).max();
    let started = Instant::now();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            let cleanup_deadline = Instant::now() + CLEANUP_TIMEOUT;
            logger.begin_shutdown();
            return finish(
                Err(Failure::Runtime(err)),
                target,
                started,
                logger,
                cleanup_deadline,
            );
        }
    };

    let outcome = runtime.block_on(apply(&config, target));
    finish_run(runtime, logger, outcome, target, started)
}

fn finish_run(
    runtime: tokio::runtime::Runtime,
    logger: LoggerGuard,
    outcome: Result<Report, Failure>,
    target: Option<i64>,
    started: Instant,
) -> ExitCode {
    let cleanup_deadline = Instant::now() + CLEANUP_TIMEOUT;
    logger.begin_shutdown();
    runtime.shutdown_timeout(cleanup_deadline.saturating_duration_since(Instant::now()));
    finish(outcome, target, started, logger, cleanup_deadline)
}

fn finish(
    outcome: Result<Report, Failure>,
    target: Option<i64>,
    started: Instant,
    logger: LoggerGuard,
    cleanup_deadline: Instant,
) -> ExitCode {
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    // One terminal record per run, read by operators and deploy hooks. An
    // absent version field means none exists or it was not observed.
    let primary = match outcome {
        Ok(report) => {
            tracing::info!(
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
            tracing::error!(
                migration.target = target,
                migration.duration_ms = duration_ms,
                outcome = "error",
                stage = failure.stage(),
                error = %failure,
                "migration_run"
            );
            ExitCode::FAILURE
        }
    };
    // The snapshot remains local: another record after closing would require
    // another drain. Telemetry loss cannot change or replay the database result.
    let _logger_shutdown = logger.shutdown(cleanup_deadline);
    primary
}

async fn apply(config: &MigrationConfig, target: Option<i64>) -> Result<Report, Failure> {
    if !config.postgres.enabled {
        return Err(Failure::PostgresDisabled);
    }
    let dsn = Dsn::admit_with(
        config.postgres.required_dsn()?.expose_secret(),
        config.postgres.password_file.as_deref(),
    )?;
    tracing::info!(
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
            result = migrate::run(&MIGRATOR, options) => Ok(result?),
            _ = terminate.recv() => Err(Failure::Interrupted("SIGTERM")),
            _ = interrupt.recv() => Err(Failure::Interrupted("SIGINT")),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            result = migrate::run(&MIGRATOR, options) => Ok(result?),
            _ = tokio::signal::ctrl_c() => Err(Failure::Interrupted("ctrl-c")),
        }
    }
}

#[cfg(all(test, unix))]
#[path = "terminal_tests.rs"]
mod tests;
