use std::io::Write as _;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use infra_postgres::{Dsn, DsnError};
use infra_telemetry::{
    LoggerGuard, LoggingFormat, LoggingOptions, install_panic_hook, install_subscriber,
};
use migrate::{MIGRATOR, RECORD_TARGET, Report, RunFailure, RunOptions};
use secrecy::ExposeSecret;
use service_config::{BuildInfo, LoadOptions, MigrationConfig, MigrationLoadError};

const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);

fn record_level(base: &str) -> String {
    format!("{base},{RECORD_TARGET}=info")
}

#[derive(Clone, Copy, Debug)]
struct RunContext {
    started: Instant,
    target: Option<i64>,
}

impl RunContext {
    fn start() -> Self {
        Self {
            started: Instant::now(),
            target: MIGRATOR.iter().map(|migration| migration.version).max(),
        }
    }

    fn finish_config_failure(self, message: String) -> ExitCode {
        let level = record_level("info");
        let Ok(logger) = install_subscriber(&LoggingOptions {
            level: &level,
            format: LoggingFormat::Json,
            tracer_provider: None,
        }) else {
            return write_stderr_failure(&message);
        };
        self.finish_run(None, logger, Err(Failure::Setup(message)))
    }

    fn finish_run(
        self,
        runtime: Option<tokio::runtime::Runtime>,
        logger: LoggerGuard,
        outcome: Result<Report, Failure>,
    ) -> ExitCode {
        let cleanup_deadline = Instant::now() + CLEANUP_TIMEOUT;
        logger.begin_shutdown();
        let exit_code = self.record_outcome(outcome);
        if let Some(runtime) = runtime {
            runtime.shutdown_timeout(cleanup_deadline.saturating_duration_since(Instant::now()));
        }
        let _ = logger.shutdown(cleanup_deadline);
        exit_code
    }

    fn record_outcome(self, outcome: Result<Report, Failure>) -> ExitCode {
        let duration_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match outcome {
            Ok(report) => {
                tracing::event!(
                    target: RECORD_TARGET,
                    tracing::Level::INFO,
                    migration.before = report.before,
                    migration.target = self.target,
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
                    target: RECORD_TARGET,
                    tracing::Level::ERROR,
                    migration.before = report.and_then(|report| report.before),
                    migration.target = self.target,
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
}

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error(transparent)]
    Dsn(#[from] DsnError),
    #[error("build tokio runtime: {0}")]
    Runtime(std::io::Error),
    #[error("install stop signal handlers: {0}")]
    Signals(std::io::Error),
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
            Self::Dsn(_) | Self::Setup(_) => "config",
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
    let run = RunContext::start();
    let options = LoadOptions::try_parse_from(std::env::args_os()).unwrap_or_else(|err| err.exit());
    let config = service_config::load_migration(&options, BUILD_INFO);
    let log = match &config {
        Ok(config) => config.log(),
        Err(MigrationLoadError::PostgresDisabled { log }) => log,
        Err(err) => return run.finish_config_failure(err.to_string()),
    };
    let level = record_level(&log.level);
    let logger = match install_subscriber(&LoggingOptions {
        level: &level,
        format: match log.format {
            service_config::LogFormat::Json => LoggingFormat::Json,
            service_config::LogFormat::Text => LoggingFormat::Text,
        },
        tracer_provider: None,
    }) {
        Ok(logger) => logger,
        Err(err) => return run.finish_config_failure(format!("install logger: {err}")),
    };
    let config = match config {
        Ok(config) => config,
        Err(err) => return run.finish_run(None, logger, Err(Failure::Setup(err.to_string()))),
    };
    install_panic_hook();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return run.finish_run(None, logger, Err(Failure::Runtime(err))),
    };

    let outcome = runtime.block_on(apply(&config, run.target));
    run.finish_run(Some(runtime), logger, outcome)
}

#[expect(
    clippy::disallowed_methods,
    reason = "terminal diagnostic when fallback logging is unavailable"
)]
fn write_stderr_failure(message: &dyn std::fmt::Display) -> ExitCode {
    let _ = writeln!(std::io::stderr().lock(), "{message}");
    ExitCode::FAILURE
}

async fn apply(config: &MigrationConfig, target: Option<i64>) -> Result<Report, Failure> {
    let postgres = config.postgres();
    let dsn = Dsn::admit_for_environment(
        postgres.dsn.expose_secret(),
        postgres.password_file.as_deref(),
        config.app().is_local_development(),
    )?;
    tracing::event!(
        target: RECORD_TARGET,
        tracing::Level::INFO,
        app.env = %config.app().env,
        app.version = %config.app().version,
        app.commit = %config.app().commit,
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        migration.target = target,
        migration.deadline_ms =
            u64::try_from(postgres.migration_deadline.as_millis()).unwrap_or(u64::MAX),
        "migration_starting"
    );
    let options = RunOptions::defaults(&dsn, config.service_name(), postgres.migration_deadline);
    run_until_stop(&options).await
}

async fn run_until_stop(options: &RunOptions<'_>) -> Result<Report, Failure> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
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
