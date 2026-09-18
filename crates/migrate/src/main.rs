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

use infra_postgres::Dsn;
use infra_telemetry::{LogFormat, LoggingOptions, install_subscriber};
use migrate::{FailedRun, MIGRATOR, RunOptions, RunResult};
use secrecy::ExposeSecret;
use service_config::{BuildInfo, Config, LoadOptions};

const BUILD_INFO: BuildInfo = BuildInfo {
    version: env!("CARGO_PKG_VERSION"),
    commit: env!("VERGEN_GIT_SHA"),
};

const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error("{0}")]
    Config(String),
    #[error(transparent)]
    Run(#[from] FailedRun),
    #[error("interrupted by {0}")]
    Interrupted(&'static str),
}

impl Failure {
    fn stage(&self) -> &'static str {
        match self {
            Self::Config(_) => "config",
            Self::Run(failure) => failure.stage().as_str(),
            Self::Interrupted(_) => "interrupted",
        }
    }

    /// What the run had observed before failing; the target is known even
    /// when nothing else is.
    fn observed(&self) -> RunResult {
        match self {
            Self::Run(failure) => failure.observed.clone(),
            Self::Config(_) | Self::Interrupted(_) => RunResult {
                target: MIGRATOR.iter().map(|m| m.version).max(),
                ..RunResult::default()
            },
        }
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args_os();
    let _argv0 = args.next();
    let options = match LoadOptions::parse_args(args) {
        Ok(options) => options,
        Err(err) => {
            let success = err.exit_code() == 0;
            let _ = err.print();
            return if success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
    };
    let config = match service_config::load(&options, BUILD_INFO) {
        Ok(config) => config,
        Err(err) => return startup_failure(&err.to_string()),
    };
    if let Err(err) = install_subscriber(&LoggingOptions {
        level: config.log.level.clone(),
        format: match config.log.format {
            service_config::LogFormat::Json => LogFormat::Json,
            service_config::LogFormat::Text => LogFormat::Text,
        },
        tracer_provider: None,
        service_name: &config.observability.otel.service_name,
    }) {
        return startup_failure(&err.to_string());
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return startup_failure(&format!("build tokio runtime: {err}")),
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

fn startup_failure(message: &str) -> ExitCode {
    #[allow(clippy::print_stderr)]
    {
        eprintln!("{message}");
    }
    ExitCode::FAILURE
}

async fn apply(config: &Config) -> Result<RunResult, Failure> {
    if !config.postgres.enabled {
        return Err(Failure::Config(
            "postgres.enabled must be true to run migrations".to_owned(),
        ));
    }
    let dsn = Dsn::parse(config.postgres.dsn.expose_secret())
        .map_err(|err| Failure::Config(err.to_string()))?;
    tracing::info!(
        app.env = %config.app.env,
        app.version = %config.app.version,
        app.commit = %config.app.commit,
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        migration.target = recorded_version(MIGRATOR.iter().map(|m| m.version).max()),
        "migration_starting"
    );
    let options = RunOptions::defaults(&dsn, &config.observability.otel.service_name);
    tokio::select! {
        result = migrate::run(&MIGRATOR, &options) => result.map_err(Failure::from),
        signal = stop_signal() => Err(Failure::Interrupted(signal)),
    }
}

async fn stop_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut terminate), Ok(mut interrupt)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = terminate.recv() => "SIGTERM",
            _ = interrupt.recv() => "SIGINT",
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "ctrl-c"
    }
}

/// `0` is reserved for absent or unobserved versions: source rules require
/// a positive version, so this sentinel cannot collide with a real one.
fn recorded_version(version: Option<i64>) -> i64 {
    version.unwrap_or(0)
}

/// One record per run with the fields an operator or a deploy hook reads.
fn log_terminal(result: &RunResult, failure: Option<&Failure>) {
    let duration_ms = u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX);
    match failure {
        None => tracing::info!(
            migration.before = recorded_version(result.before),
            migration.target = recorded_version(result.target),
            migration.after = recorded_version(result.after),
            migration.applied_count = result.applied,
            migration.duration_ms = duration_ms,
            outcome = result.outcome(),
            "migration_run"
        ),
        Some(failure) => tracing::error!(
            migration.before = recorded_version(result.before),
            migration.target = recorded_version(result.target),
            migration.after = recorded_version(result.after),
            migration.applied_count = result.applied,
            migration.duration_ms = duration_ms,
            outcome = "error",
            stage = failure.stage(),
            error = %failure,
            "migration_run"
        ),
    }
}
