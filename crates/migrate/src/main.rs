//! `migrate`: apply the embedded migrations to the configured database.
//!
//! Loads the same configuration as the service (`--config`, overlays,
//! `APP__*`), so a migration run is attributable to the same service,
//! version, and environment as the process it prepares the schema for.
//! Requires the PostgreSQL profile to be enabled; writes one terminal
//! `migration_run` record; exits 0 on success or no change, 1 otherwise.
//! A stop signal drops the run, which ends the session and with it the
//! advisory lock and any in-flight transaction.

use std::process::ExitCode;
use std::time::Duration;

use infra_postgres::{Dsn, DsnError};
use infra_telemetry::{LoggingFormat, LoggingOptions, install_subscriber};
use migrate::{FailedRun, MIGRATOR, RunOptions, RunResult, Stage};
use secrecy::ExposeSecret;
use service_config::{BuildInfo, Config, FromArgs, LoadOptions, ValidationError, process_failure};

const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error("postgres.enabled must be true to run migrations")]
    PostgresDisabled,
    #[error(transparent)]
    Config(#[from] ValidationError),
    #[error(transparent)]
    Dsn(#[from] DsnError),
    #[error(transparent)]
    Run(#[from] FailedRun),
    #[error("interrupted by {0}")]
    Interrupted(&'static str),
}

/// Words on the `migration_run` `stage` field: migrator [`Stage`] plus the
/// process-level failures this binary adds.
#[derive(Clone, Copy)]
enum TerminalStage {
    Run(Stage),
    Config,
    Interrupted,
}

impl TerminalStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Run(stage) => stage.as_str(),
            Self::Config => "config",
            Self::Interrupted => "interrupted",
        }
    }
}

impl Failure {
    fn stage(&self) -> TerminalStage {
        match self {
            Self::PostgresDisabled | Self::Config(_) | Self::Dsn(_) => TerminalStage::Config,
            Self::Run(failure) => TerminalStage::Run(failure.stage()),
            Self::Interrupted(_) => TerminalStage::Interrupted,
        }
    }

    /// What the run had observed before failing; the target is known even
    /// when nothing else is.
    fn observed(&self) -> RunResult {
        match self {
            Self::Run(failure) => failure.observed.clone(),
            Self::PostgresDisabled | Self::Config(_) | Self::Dsn(_) | Self::Interrupted(_) => {
                RunResult {
                    target: MIGRATOR.iter().map(|m| m.version).max(),
                    ..RunResult::default()
                }
            }
        }
    }
}

fn main() -> ExitCode {
    let options = match LoadOptions::from_args(std::env::args_os()) {
        FromArgs::Run(options) => options,
        FromArgs::Exit(code) => return code,
    };
    let config = match service_config::load(&options, BUILD_INFO) {
        Ok(config) => config,
        Err(err) => return process_failure(&err.to_string()),
    };
    if let Err(err) = install_subscriber(&LoggingOptions {
        level: &config.log.level,
        format: match config.log.format {
            service_config::LogFormat::Json => LoggingFormat::Json,
            service_config::LogFormat::Text => LoggingFormat::Text,
        },
        tracer_provider: None,
        service_name: &config.observability.otel.service_name,
    }) {
        return process_failure(&err.to_string());
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return process_failure(&format!("build tokio runtime: {err}")),
    };

    let outcome = runtime.block_on(apply(&config));
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);

    match outcome {
        Ok(result) => {
            log_terminal(&result, None);
            ExitCode::SUCCESS
        }
        Err(failure) => {
            log_terminal(&failure.observed(), Some(&failure));
            ExitCode::FAILURE
        }
    }
}

async fn apply(config: &Config) -> Result<RunResult, Failure> {
    if !config.postgres.enabled {
        return Err(Failure::PostgresDisabled);
    }
    let dsn = Dsn::parse(config.postgres.required_dsn()?.expose_secret())?;
    tracing::info!(
        app.env = %config.app.env,
        app.version = %config.app.version,
        app.commit = %config.app.commit,
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        migration.target = version_or_zero(MIGRATOR.iter().map(|m| m.version).max()),
        "migration_starting"
    );
    let options = RunOptions::defaults(&dsn, &config.observability.otel.service_name);
    run_until_stop(&options).await
}

/// Apply migrations, interrupting on a stop signal when handlers install.
///
/// When no handler can be installed the run continues until success, SQL
/// failure, or the orchestration deadline. That is a job-style contract,
/// not the service binary's fail-startup path. A successful unix `Signal`
/// stream is held until the wait completes; dropping it would swallow a
/// later SIGTERM.
async fn run_until_stop(options: &RunOptions<'_>) -> Result<RunResult, Failure> {
    #[cfg(unix)]
    {
        use std::future::pending;
        use tokio::signal::unix::{Signal, SignalKind, signal};

        async fn recv_or_pending(stream: Option<&mut Signal>) {
            match stream {
                Some(stream) => {
                    stream.recv().await;
                }
                None => pending().await,
            }
        }

        let mut terminate = signal(SignalKind::terminate()).ok();
        let mut interrupt = signal(SignalKind::interrupt()).ok();
        tokio::select! {
            result = migrate::run(&MIGRATOR, options) => result.map_err(Failure::from),
            () = recv_or_pending(terminate.as_mut()) => Err(Failure::Interrupted("SIGTERM")),
            () = recv_or_pending(interrupt.as_mut()) => Err(Failure::Interrupted("SIGINT")),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            result = migrate::run(&MIGRATOR, options) => result.map_err(Failure::from),
            _ = tokio::signal::ctrl_c() => Err(Failure::Interrupted("ctrl-c")),
        }
    }
}

/// `0` is reserved for absent or unobserved versions: source rules require
/// a positive version, so this sentinel cannot collide with a real one.
fn version_or_zero(version: Option<i64>) -> i64 {
    version.unwrap_or(0)
}

/// One record per run with the fields an operator or a deploy hook reads.
fn log_terminal(result: &RunResult, failure: Option<&Failure>) {
    let duration_ms = u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX);
    match failure {
        None => tracing::info!(
            migration.before = version_or_zero(result.before),
            migration.target = version_or_zero(result.target),
            migration.after = version_or_zero(result.after),
            migration.applied_count = result.applied,
            migration.duration_ms = duration_ms,
            outcome = result.outcome(),
            "migration_run"
        ),
        Some(failure) => tracing::error!(
            migration.before = version_or_zero(result.before),
            migration.target = version_or_zero(result.target),
            migration.after = version_or_zero(result.after),
            migration.applied_count = result.applied,
            migration.duration_ms = duration_ms,
            outcome = "error",
            stage = failure.stage().as_str(),
            error = %failure,
            "migration_run"
        ),
    }
}
