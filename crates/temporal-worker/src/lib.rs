mod background;
mod bootstrap;
mod cli;
mod resources;
mod shutdown;
mod signals;

use std::ffi::OsString;
use std::fmt;
use std::io::Write as _;
use std::pin::Pin;
use std::process::ExitCode;

use clap::Parser;
use health::BoxedProbe;
use infra_temporal::worker::{RegistryError, WorkerRegistry, WorkflowRegistrationError};
use infra_temporal::{CloseOutcome, TemporalClient};
use service_config::{BuildInfo, LoadOptions, TemporalWorkerConfig};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub use shutdown::TEMPORAL_SHUTDOWN_TAIL;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct BuildError(#[source] pub Box<dyn std::error::Error + Send + Sync>);

impl BuildError {
    #[must_use]
    pub fn new(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self(error.into())
    }
}

impl From<Box<dyn std::error::Error + Send + Sync>> for BuildError {
    fn from(error: Box<dyn std::error::Error + Send + Sync>) -> Self {
        Self(error)
    }
}

impl From<RegistryError> for BuildError {
    fn from(error: RegistryError) -> Self {
        Self(Box::new(error))
    }
}

impl From<WorkflowRegistrationError> for BuildError {
    fn from(error: WorkflowRegistrationError) -> Self {
        Self(Box::new(error))
    }
}

impl From<&'static str> for BuildError {
    fn from(message: &'static str) -> Self {
        Self(message.into())
    }
}

impl From<String> for BuildError {
    fn from(message: String) -> Self {
        Self(message.into())
    }
}

pub struct Registration<'a> {
    pub temporal: WorkerRegistry,
    config: &'a TemporalWorkerConfig,
    client: &'a TemporalClient,
    readiness_probes: Vec<BoxedProbe>,
    close_hooks: &'a mut Vec<resources::CloseHook>,
    background: &'a mut background::Background,
}

impl fmt::Debug for Registration<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Registration")
            .field("temporal", &self.temporal)
            .field("readiness_probes", &self.readiness_probes.len())
            .field("close_hooks", &self.close_hooks.len())
            .finish_non_exhaustive()
    }
}

impl<'a> Registration<'a> {
    fn new(
        config: &'a TemporalWorkerConfig,
        client: &'a TemporalClient,
        close_hooks: &'a mut Vec<resources::CloseHook>,
        background: &'a mut background::Background,
    ) -> Self {
        Self {
            temporal: WorkerRegistry::new(),
            config,
            client,
            readiness_probes: Vec::new(),
            close_hooks,
            background,
        }
    }

    #[must_use]
    pub const fn config(&self) -> &TemporalWorkerConfig {
        self.config
    }

    #[must_use]
    pub const fn client(&self) -> &TemporalClient {
        self.client
    }

    pub fn add_readiness_probe(&mut self, probe: BoxedProbe) {
        self.readiness_probes.push(probe);
    }

    #[expect(
        clippy::type_complexity,
        reason = "public close-hook signature matches Registration contract"
    )]
    pub fn register_close_hook(
        &mut self,
        hook: Box<dyn FnOnce(Instant) -> Pin<Box<dyn Future<Output = CloseOutcome> + Send>> + Send>,
    ) {
        self.close_hooks.push(hook);
    }

    pub fn spawn<F: Future<Output = ()> + Send + 'static>(
        &mut self,
        name: &'static str,
        task: impl FnOnce(CancellationToken) -> F,
    ) {
        self.background.spawn(name, task);
    }

    #[must_use]
    pub fn shutdown(&self) -> CancellationToken {
        self.background.child_token()
    }

    fn into_parts(self) -> (WorkerRegistry, Vec<BoxedProbe>) {
        (self.temporal, self.readiness_probes)
    }
}

type Register<'r> = Box<dyn FnOnce(&mut Registration<'_>) -> Result<(), BuildError> + 'r>;

const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

const EXIT_USAGE: u8 = 2;
const EXIT_DEGRADED_SHUTDOWN: u8 = 3;

#[expect(
    clippy::disallowed_methods,
    reason = "the worker owns its terminal failure diagnostics"
)]
fn report_failure(message: impl fmt::Display) {
    let _ = writeln!(std::io::stderr().lock(), "{message}");
}

pub fn run(
    args: impl IntoIterator<Item = OsString>,
    register: impl FnOnce(&mut Registration<'_>) -> Result<(), BuildError>,
) -> ExitCode {
    let args = match cli::WorkerArgs::try_parse_from(args) {
        Ok(args) => args,
        Err(error) => {
            if error.kind() == clap::error::ErrorKind::DisplayHelp {
                return reported_exit(error.print().is_ok());
            }
            report_failure("invalid command arguments; use --help for usage");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    worker_exit(run_worker(&args.options, Box::new(register)))
}

fn reported_exit(success: bool) -> ExitCode {
    if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn worker_exit(result: Result<shutdown::Outcome, bootstrap::WorkerError>) -> ExitCode {
    result.map_or(ExitCode::FAILURE, |outcome| match outcome {
        shutdown::Outcome::Graceful => ExitCode::SUCCESS,
        shutdown::Outcome::Degraded => ExitCode::from(EXIT_DEGRADED_SHUTDOWN),
    })
}

fn run_worker(
    options: &LoadOptions,
    register: Register<'_>,
) -> Result<shutdown::Outcome, bootstrap::WorkerError> {
    let (config, runtime) = load_config_and_runtime(options).inspect_err(|error| {
        report_failure(error);
    })?;
    let grace = config.http().grace_period;
    let signals = {
        let _entered = runtime.enter();
        signals::Signals::install()
    };
    let mut signals = match signals {
        Ok(signals) => signals,
        Err(error) => {
            runtime.shutdown_timeout(shutdown::RUNTIME_SHUTDOWN_TIMEOUT);
            let error = bootstrap::WorkerError::Signals(error);
            report_failure(&error);
            return Err(error);
        }
    };
    let mut deadline = None;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(bootstrap::serve(
            config,
            register,
            &mut signals,
            &mut deadline,
        ))
    }))
    .unwrap_or(Err(bootstrap::WorkerError::Panicked));
    let deadline = deadline.unwrap_or_else(|| shutdown::stop_deadline(signals.first_stop(), grace));
    runtime.shutdown_timeout(
        shutdown::RUNTIME_SHUTDOWN_TIMEOUT.min(deadline.saturating_duration_since(Instant::now())),
    );
    drop(signals);
    outcome
}

fn load_config_and_runtime(
    options: &LoadOptions,
) -> Result<(TemporalWorkerConfig, tokio::runtime::Runtime), bootstrap::WorkerError> {
    let config = service_config::load_temporal_worker(options, BUILD_INFO, shutdown::TAIL)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.runtime().effective_worker_threads())
        .enable_all()
        .build()
        .map_err(bootstrap::WorkerError::Runtime)?;
    Ok((config, runtime))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use service_config::LoadOptions;

    use super::bootstrap::WorkerError;
    use super::run_worker;

    fn missing_file() -> LoadOptions {
        LoadOptions {
            config: Some(PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/missing-temporal-worker-config.toml"
            ))),
            ..LoadOptions::default()
        }
    }

    #[test]
    fn start_with_registration_reads_configuration_first() {
        let started = run_worker(
            &missing_file(),
            Box::new(|_| Err("registration must not run before configuration".into())),
        );
        std::assert_matches!(
            started,
            Err(WorkerError::Load(_)),
            "configuration must fail before registration"
        );
    }
}
