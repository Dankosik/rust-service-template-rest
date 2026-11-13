//! The jobs worker's entry point and registration contract.
//!
//! A composition root registers its retained job kinds in `src/main.rs` by
//! passing a registration function or closure to [`run`], which fills a
//! [`Registration`]. The shipped binary supplies its retained profile
//! registrations; a composition with no retained capability refuses after
//! configuration is loaded. The synchronous startup phases and the one
//! exit-code mapping live here, the asynchronous startup in `bootstrap`, the
//! stop signals in `signals`, worker-owned tasks in `background`, and the
//! staged teardown in `shutdown`.

mod background;
mod bootstrap;
mod cli;
// template:begin jobs:worker-operator-module
mod operator;
// template:end jobs:worker-operator-module
mod shutdown;
mod signals;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;
use service_config::{BuildInfo, LoadOptions, process_failure};
use tokio_util::sync::CancellationToken;

pub type BuildError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// What a derived service's registration fills and may use to build
/// handlers: each retained job kind and typed messaging handler. The worker
/// decides which registered capabilities become active from the immutable
/// configuration before it opens either dependency.
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
    /// Add database-backed message handlers after the worker admits its shared pool.
    ///
    /// Declare routes through `messages` during ordinary registration. This hook
    /// declares consumer intent before dependency I/O and receives the same registry
    /// after pool admission and migration-history verification, before broker admission.
    /// It must perform only bounded synchronous composition: no I/O, pool creation,
    /// task spawning or retries. The worker retains pool and shutdown ownership.
    ///
    /// # Errors
    /// Refuses a second factory. A factory error, panic, duplicate handler or empty
    /// consumer registry later refuses startup through the existing cleanup path.
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

    /// Spawn a background task the worker owns: `start` builds it from a
    /// token cancelled at the background-join stage, which then joins it.
    ///
    /// The task must run until that token is cancelled. One that returns or
    /// panics earlier stops the worker with exit code 1, and the failure
    /// names it by `name`. A panic after cancellation still makes shutdown
    /// degraded. The worker bounds cooperative joining, requests abort when
    /// it expires, and distinguishes acknowledged termination from unfinished
    /// work before closing dependencies. Registration itself must return
    /// promptly and must not perform blocking provider I/O.
    ///
    /// `start` runs synchronously inside this call, so a panic while it
    /// builds the future unwinds registration: the worker reports the
    /// anonymous lifecycle panic, not a failure named `name`.
    pub fn spawn<F>(&self, name: &'static str, start: impl FnOnce(CancellationToken) -> F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.background.spawn(name, start);
    }

    /// Each call returns a new child of the background token, cancelled at the
    /// background-join stage. Cancelling the returned token does not stop the
    /// worker.
    #[must_use]
    pub fn shutdown(&self) -> CancellationToken {
        self.background.cancel.child_token()
    }
}

type Register<'r> = Box<dyn FnOnce(&mut Registration<'_>) -> Result<(), BuildError> + 'r>;

const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

/// The process stopped on a signal but a stage voted degraded, including a forced drain.
const EXIT_DEGRADED_SHUTDOWN: u8 = 3;

/// Parse flags, load configuration, run the worker, and map the result to an
/// exit code. The first item of `args` is the program name, as in
/// [`std::env::args_os`]. A composition with no retained capability refuses after
/// configuration is loaded. `--help` prints clap's help and exits 0; a flag
/// error prints one generic usage line and exits 2.
/// Later failures do not call [`std::process::exit`].
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
                // clap's detailed usage errors can repeat arbitrary rejected values.
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
    /// One-shot output, help or an operator receipt, and whether it succeeded.
    Reported(bool),
    Usage,
}

/// The one owner of the exit-code table, including one-shot commands.
fn exit_code(result: &ProcessResult) -> u8 {
    match result {
        ProcessResult::Worker(Ok(shutdown::Outcome::Graceful)) | ProcessResult::Reported(true) => 0,
        ProcessResult::Worker(Ok(shutdown::Outcome::Degraded)) => EXIT_DEGRADED_SHUTDOWN,
        ProcessResult::Worker(Err(_)) | ProcessResult::Reported(false) => 1,
        ProcessResult::Usage => 2,
    }
}

/// Run the long-lived worker. The final runtime wait spends at most
/// [`shutdown::RUNTIME_SHUTDOWN_TIMEOUT`] within the process deadline.
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
    // Native signal streams remain alive until the explicit runtime shutdown returns.
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
