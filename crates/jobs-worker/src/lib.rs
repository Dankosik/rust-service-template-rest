//! The jobs worker's entry point and registration contract.
//!
//! A composition root registers its retained job kinds in `src/main.rs` by
//! passing a registration function or closure to [`run`], which fills a
//! [`Registration`]. The shipped binary supplies its retained profile
//! registrations; a composition with no retained capability refuses after
//! configuration is loaded. The synchronous startup phases and the one
//! exit-code mapping live here, the asynchronous startup in `bootstrap`, and
//! the staged teardown in `shutdown`. The full order is in
//! docs/architecture/runtime-lifecycle.md (section "Jobs worker").

mod bootstrap;
mod shutdown;

use std::ffi::OsString;
use std::fmt;
use std::process::ExitCode;
use std::time::Duration;

use service_config::{BuildInfo, LoadOptions, process_failure};
use tokio_util::sync::CancellationToken;

/// The error a registration returns; the worker refuses with it.
pub type BuildError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// What a derived service's registration fills and may use to build
/// handlers: each retained job kind and typed messaging handler. The worker
/// decides which registered capabilities become active from the immutable
/// configuration before it opens either dependency.
pub struct Registration<'a> {
    // template:begin jobs:worker-registration-jobs
    /// The job kinds this worker claims.
    pub jobs: infra_jobs::Kinds,
    // template:end jobs:worker-registration-jobs
    // template:begin messaging:worker-registration-messaging
    /// The typed message handlers this worker consumes with.
    pub messages: infra_messaging::Registry,
    // template:end messaging:worker-registration-messaging
    config: &'a service_config::Config,
    background: &'a shutdown::Background,
}

impl<'a> Registration<'a> {
    /// The loaded configuration.
    #[must_use]
    pub fn config(&self) -> &'a service_config::Config {
        self.config
    }

    /// Spawn a background task the worker owns: `start` builds it from a
    /// token cancelled at the background-join stage, which then joins it.
    ///
    /// The task must run until that token is cancelled. One that returns or
    /// panics earlier stops the worker with exit code 1, and the failure
    /// names it by `name`.
    pub fn spawn<F>(&self, name: &'static str, start: impl FnOnce(CancellationToken) -> F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.background.spawn(name, start);
    }

    /// A new child of the worker's root token, cancelled at the background-join stage.
    #[must_use]
    pub fn shutdown(&self) -> CancellationToken {
        self.background.cancel.child_token()
    }
}

impl fmt::Debug for Registration<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registration").finish_non_exhaustive()
    }
}

/// The registration [`run`] was given, called once after configuration is loaded.
type Register<'r> = Box<dyn FnOnce(&mut Registration<'_>) -> Result<(), BuildError> + 'r>;

/// Version and revision stamped into this binary.
const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

/// The process stopped on a signal but a stage voted degraded, including a forced drain.
const EXIT_DEGRADED_SHUTDOWN: u8 = 3;

/// Bound for dropping whatever the runtime still owns after the ordered
/// teardown: connection tasks that outlived drain and `pool.close`, and any
/// blocking tracer-provider shutdown that outlived its budget.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// Parse flags, load configuration, run the worker, and map the result to an
/// exit code. A composition with no retained capability refuses after
/// configuration is loaded. `--help` exits 0 and a flag error exits 2 through
/// clap, as in the service. A failure is reported once. Later failures do not call
/// `process::exit`.
#[must_use]
pub fn run<I, R>(args: I, register: R) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
    R: FnOnce(&mut Registration<'_>) -> Result<(), BuildError>,
{
    let options = LoadOptions::parse_from(args);
    let result = start(&options, Box::new(register));
    if let Err(err) = &result {
        tracing::error!(error = %err, "jobs worker failed");
        let _ = process_failure(&err.to_string());
    }
    ExitCode::from(exit_code(&result))
}

/// The one owner of the exit-code table. No other code in the crate maps an
/// outcome or an error to an exit code.
fn exit_code(result: &Result<shutdown::Outcome, bootstrap::WorkerError>) -> u8 {
    match result {
        Ok(shutdown::Outcome::Graceful) => 0,
        Ok(shutdown::Outcome::Degraded) => EXIT_DEGRADED_SHUTDOWN,
        Err(_) => 1,
    }
}

/// Everything [`run`] does after flag parsing: configuration, the
/// preconditions, the runtime, and `bootstrap::serve`, with the runtime shut
/// down within [`RUNTIME_SHUTDOWN_TIMEOUT`].
fn start(
    options: &LoadOptions,
    register: Register<'_>,
) -> Result<shutdown::Outcome, bootstrap::WorkerError> {
    let config = service_config::load(options, BUILD_INFO)?;
    bootstrap::check_preconditions(&config)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(bootstrap::WorkerError::Runtime)?;
    let outcome = runtime.block_on(bootstrap::serve(config, register));
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    outcome
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use service_config::LoadOptions;

    use super::bootstrap::WorkerError;
    use super::shutdown::Outcome;
    use super::{exit_code, start};

    #[test]
    fn exit_code_maps_the_three_rows() {
        assert_eq!(exit_code(&Ok(Outcome::Graceful)), 0);
        assert_eq!(exit_code(&Ok(Outcome::Degraded)), 3);
        assert_eq!(exit_code(&Err(WorkerError::NoRegistrations)), 1);
        // template:begin jobs:worker-lib-test-postgres-refusal
        assert_eq!(exit_code(&Err(WorkerError::PostgresDisabled)), 1);
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
        assert!(
            matches!(started, Err(WorkerError::Load(_))),
            "configuration must fail before registration"
        );
    }
}
