mod background;
mod bootstrap;
mod cli;
// template:begin jobs:worker-operator-module
mod operator;
// template:end jobs:worker-operator-module
mod resources;
mod shutdown;
mod signals;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;
use service_config::{BuildInfo, LoadOptions, process_failure};
use tokio_util::sync::CancellationToken;

pub type BuildError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(derive_more::Debug)]
#[debug("Registration {{ .. }}")]
pub struct Registration<'a> {
    // template:begin jobs:worker-registration-jobs
    pub jobs: infra_jobs::KindsBuilder,
    // template:end jobs:worker-registration-jobs
    // template:begin messaging:worker-registration-messaging
    pub messages: infra_messaging::Registry,
    // template:end messaging:worker-registration-messaging
    // template:begin outbox:worker-registration-postgres-messages
    postgres_messages: Option<PostgresMessages>,
    // template:end outbox:worker-registration-postgres-messages
    config: &'a service_config::Config,
    background: &'a background::Background,
}

// template:begin outbox:worker-postgres-messages-factory
type PostgresMessages = Box<
    dyn FnOnce(infra_postgres::PgPool, &mut infra_messaging::Registry) -> Result<(), BuildError>,
>;
// template:end outbox:worker-postgres-messages-factory

impl<'a> Registration<'a> {
    // template:begin outbox:worker-with-postgres-messages
    pub fn with_postgres_messages(
        &mut self,
        factory: impl FnOnce(
            infra_postgres::PgPool,
            &mut infra_messaging::Registry,
        ) -> Result<(), BuildError>
        + 'static,
    ) -> Result<(), BuildError> {
        if self.postgres_messages.is_some() {
            return Err("postgres message factory is already registered".into());
        }
        self.postgres_messages = Some(Box::new(factory));
        Ok(())
    }
    // template:end outbox:worker-with-postgres-messages

    #[must_use]
    pub fn config(&self) -> &'a service_config::Config {
        self.config
    }

    pub fn spawn<F>(&self, name: &'static str, start: impl FnOnce(CancellationToken) -> F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.background.spawn(name, start);
    }

    #[must_use]
    pub fn shutdown(&self) -> CancellationToken {
        self.background.cancel.child_token()
    }
}

type Register<'r> = Box<dyn FnOnce(&mut Registration<'_>) -> Result<(), BuildError> + 'r>;

const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

const EXIT_DEGRADED_SHUTDOWN: u8 = 3;

pub fn run<I, R>(args: I, register: R) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
    R: FnOnce(&mut Registration<'_>) -> Result<(), BuildError>,
{
    let args = match cli::WorkerArgs::try_parse_from(args) {
        Ok(args) => args,
        Err(error) => {
            let result = if error.kind() == clap::error::ErrorKind::DisplayHelp {
                ProcessResult::Reported(error.print().is_ok())
            } else {
                let _ = process_failure("invalid command arguments; use --help for usage");
                ProcessResult::Usage
            };
            return ExitCode::from(exit_code(&result));
        }
    };
    // template:begin jobs:worker-operator-dispatch
    if let Some(command) = args.command {
        let request = match operator::Request::try_new(command) {
            Ok(request) => request,
            Err(error) => {
                let _ = process_failure(error);
                return ExitCode::from(exit_code(&ProcessResult::Usage));
            }
        };
        return ExitCode::from(exit_code(&ProcessResult::Reported(operator::run(
            &args.options,
            &request,
        ))));
    }
    // template:end jobs:worker-operator-dispatch
    let result = start(&args.options, Box::new(register));
    ExitCode::from(exit_code(&ProcessResult::Worker(result)))
}

enum ProcessResult {
    Worker(Result<shutdown::Outcome, bootstrap::WorkerError>),
    Reported(bool),
    Usage,
}

fn exit_code(result: &ProcessResult) -> u8 {
    match result {
        ProcessResult::Worker(Ok(shutdown::Outcome::Graceful)) | ProcessResult::Reported(true) => 0,
        ProcessResult::Worker(Ok(shutdown::Outcome::Degraded)) => EXIT_DEGRADED_SHUTDOWN,
        ProcessResult::Worker(Err(_)) | ProcessResult::Reported(false) => 1,
        ProcessResult::Usage => 2,
    }
}

fn start(
    options: &LoadOptions,
    register: Register<'_>,
) -> Result<shutdown::Outcome, bootstrap::WorkerError> {
    let (config, runtime) = load_config_and_runtime(options).inspect_err(|error| {
        let _ = process_failure(error);
    })?;
    let grace = config.http.grace_period;
    let signals = {
        let _entered = runtime.enter();
        signals::Signals::install()
    };
    let mut signals = match signals {
        Ok(signals) => signals,
        Err(error) => {
            runtime.shutdown_timeout(shutdown::RUNTIME_SHUTDOWN_TIMEOUT);
            let error = bootstrap::WorkerError::Signals(error);
            let _ = process_failure(&error);
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
        shutdown::RUNTIME_SHUTDOWN_TIMEOUT
            .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
    );
    drop(signals);
    outcome
}

fn load_config_and_runtime(
    options: &LoadOptions,
) -> Result<(service_config::Config, tokio::runtime::Runtime), bootstrap::WorkerError> {
    let config = service_config::load(options, BUILD_INFO)?;
    shutdown::validate_grace_budget(&config.http)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.runtime.effective_worker_threads())
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
    use super::shutdown::Outcome;
    use super::{ProcessResult, exit_code, start};

    #[test]
    fn exit_code_maps_the_three_rows() {
        assert_eq!(exit_code(&ProcessResult::Worker(Ok(Outcome::Graceful))), 0);
        assert_eq!(exit_code(&ProcessResult::Worker(Ok(Outcome::Degraded))), 3);
        assert_eq!(
            exit_code(&ProcessResult::Worker(Err(WorkerError::NoRegistrations))),
            1
        );
        // template:begin jobs:worker-lib-test-postgres-refusal
        assert_eq!(
            exit_code(&ProcessResult::Worker(Err(WorkerError::PostgresDisabled))),
            1
        );
        // template:end jobs:worker-lib-test-postgres-refusal
    }

    fn missing_file() -> LoadOptions {
        LoadOptions {
            config: Some(PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/missing-jobs-worker-config.toml"
            ))),
            ..LoadOptions::default()
        }
    }

    #[test]
    fn start_with_registration_reads_configuration_first() {
        let started = start(
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
