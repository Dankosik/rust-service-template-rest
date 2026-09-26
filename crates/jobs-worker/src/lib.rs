//! The jobs worker's entry point and registration contract.
//!
//! A composition root registers its retained job kinds in `src/main.rs` by
//! passing a [`Register`] function to [`run`]. The shipped binary supplies its
//! retained profile registrations; a composition with no registrations still
//! refuses. The synchronous startup phases and the one exit-code mapping live
//! here, the asynchronous startup in `bootstrap`, and the staged teardown in
//! `shutdown`. The full order is in
//! docs/architecture/runtime-lifecycle.md (section "Jobs worker").

mod bootstrap;
mod shutdown;

use std::ffi::OsString;
use std::fmt;
use std::process::ExitCode;
use std::time::Duration;

use service_config::{BuildInfo, FromArgs, LoadOptions, process_failure};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// The error a registration returns; the worker refuses with it.
pub type BuildError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// A derived service's registration: add each retained job kind and typed
/// messaging handler. The worker decides which registered capabilities become
/// active from the immutable configuration before it opens either dependency.
pub type Register = fn(
    // template:begin jobs:worker-register-jobs-parameter
    &mut infra_jobs::Kinds,
    // template:end jobs:worker-register-jobs-parameter
    // template:begin messaging:worker-register-messaging-parameter
    &mut infra_messaging::Registry,
    // template:end messaging:worker-register-messaging-parameter
    &Support<'_>,
) -> Result<(), BuildError>;

/// What a registration may use to build handlers.
pub struct Support<'a> {
    config: &'a service_config::Config,
    tracker: &'a TaskTracker,
    cancel: &'a CancellationToken,
}

impl<'a> Support<'a> {
    /// The loaded configuration.
    #[must_use]
    pub fn config(&self) -> &'a service_config::Config {
        self.config
    }

    /// The worker's background task tracker.
    #[must_use]
    pub fn tracker(&self) -> &'a TaskTracker {
        self.tracker
    }

    /// A new child of the worker's root token, cancelled at the background-join stage.
    #[must_use]
    pub fn shutdown(&self) -> CancellationToken {
        self.cancel.child_token()
    }
}

impl fmt::Debug for Support<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Support").finish_non_exhaustive()
    }
}

/// Version and revision stamped into this binary.
const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

/// The process stopped on a signal but a stage voted degraded, including a forced drain.
const EXIT_DEGRADED_SHUTDOWN: u8 = 3;

/// Bound for dropping whatever the runtime still owns after the ordered
/// teardown: connection tasks that outlived drain and `pool.close`, and any
/// blocking tracer-provider shutdown that outlived its budget.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// Parse flags, load configuration, run the worker, and map the result to an
/// exit code. `None` refuses with the no-kind message before configuration is
/// loaded. `--help` exits 0 and a flag error exits 1 through [`FromArgs`], as
/// in the service. A failure is reported once. Never calls `process::exit`.
#[must_use]
pub fn run<I>(args: I, register: Option<Register>) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    let options = match FromArgs::from_argv(args) {
        FromArgs::Exit(code) => return code,
        FromArgs::Run(options) => options,
    };
    let result = start(options, register);
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

/// Everything [`run`] does after flag parsing: the no-kind refusal before
/// configuration is read, configuration, the preconditions, the runtime, and
/// `bootstrap::serve`, with the runtime shut down within [`RUNTIME_SHUTDOWN_TIMEOUT`].
#[allow(
    clippy::needless_pass_by_value,
    reason = "run hands over the options it parsed; the signature is the fixed synchronous startup entry"
)]
fn start(
    options: LoadOptions,
    register: Option<Register>,
) -> Result<shutdown::Outcome, bootstrap::WorkerError> {
    let Some(register) = register else {
        return Err(bootstrap::WorkerError::NoRegistrations);
    };
    let config = service_config::load(&options, BUILD_INFO)?;
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
    use super::{BUILD_INFO, Support, exit_code, start};

    #[test]
    fn exit_code_maps_the_three_rows() {
        assert_eq!(exit_code(&Ok(Outcome::Graceful)), 0);
        assert_eq!(exit_code(&Ok(Outcome::Degraded)), 3);
        assert_eq!(exit_code(&Err(WorkerError::EngineStopped)), 1);
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
    fn start_without_registration_refuses_before_configuration() {
        let options = missing_file();
        let loaded = service_config::load(&options, BUILD_INFO);
        assert!(
            matches!(loaded, Err(service_config::Error::ReadFile { .. })),
            "loading the missing file must refuse, got {loaded:?}"
        );
        let started = start(options, None);
        assert!(
            matches!(started, Err(WorkerError::NoRegistrations)),
            "{started:?}"
        );
    }

    fn refuse(
        // template:begin jobs:worker-register-test-jobs-parameter
        _: &mut infra_jobs::Kinds,
        // template:end jobs:worker-register-test-jobs-parameter
        // template:begin messaging:worker-register-test-messaging-parameter
        _: &mut infra_messaging::Registry,
        // template:end messaging:worker-register-test-messaging-parameter
        _: &Support<'_>,
    ) -> Result<(), super::BuildError> {
        Err("registration must not run before configuration".into())
    }

    #[test]
    fn start_with_registration_reads_configuration_first() {
        let started = start(missing_file(), Some(refuse));
        assert!(matches!(started, Err(WorkerError::Load(_))), "{started:?}");
    }
}
