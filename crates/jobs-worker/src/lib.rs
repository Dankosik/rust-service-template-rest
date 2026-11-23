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
use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser;
use service_config::{BuildInfo, LoadOptions};
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
    postgres_messages: Option<PostgresMessageFactory>,
    // template:end outbox:worker-registration-postgres-messages
    // template:begin webhooks:worker-registration-outbound-webhooks
    outbound_webhooks: &'a service_config::WebhooksConfig,
    // template:end webhooks:worker-registration-outbound-webhooks
    // template:begin inbound-webhooks:worker-registration-inbound-webhook-endpoints
    inbound_webhook_endpoints: &'a [String],
    // template:end inbound-webhooks:worker-registration-inbound-webhook-endpoints
    background: &'a mut background::Background,
}

// template:begin outbox:worker-postgres-messages-factory
type PostgresMessageFactory =
    Box<dyn FnOnce(infra_postgres::Pool, &mut infra_messaging::Registry) -> Result<(), BuildError>>;
// template:end outbox:worker-postgres-messages-factory

impl<'a> Registration<'a> {
    #[allow(
        unused_variables,
        reason = "only webhook registrations use configuration"
    )]
    fn new(
        config: &'a service_config::WorkerConfig,
        background: &'a mut background::Background,
    ) -> Self {
        Self {
            // template:begin jobs:worker-bootstrap-register-jobs
            jobs: infra_jobs::KindsBuilder::new(),
            // template:end jobs:worker-bootstrap-register-jobs
            // template:begin messaging:worker-bootstrap-register-messaging
            messages: infra_messaging::Registry::default(),
            // template:end messaging:worker-bootstrap-register-messaging
            // template:begin outbox:worker-bootstrap-postgres-messages-initial
            postgres_messages: None,
            // template:end outbox:worker-bootstrap-postgres-messages-initial
            // template:begin webhooks:worker-registration-outbound-webhooks-value
            outbound_webhooks: config.webhooks(),
            // template:end webhooks:worker-registration-outbound-webhooks-value
            // template:begin inbound-webhooks:worker-registration-inbound-webhook-endpoints-value
            inbound_webhook_endpoints: config.inbound_webhook_endpoints(),
            // template:end inbound-webhooks:worker-registration-inbound-webhook-endpoints-value
            background,
        }
    }

    // template:begin outbox:worker-with-postgres-messages
    pub fn with_postgres_messages(
        &mut self,
        factory: impl FnOnce(
            infra_postgres::Pool,
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

    // template:begin webhooks:worker-outbound-webhooks-accessor
    #[must_use]
    pub fn outbound_webhooks(&self) -> &'a service_config::WebhooksConfig {
        self.outbound_webhooks
    }
    // template:end webhooks:worker-outbound-webhooks-accessor

    // template:begin inbound-webhooks:worker-inbound-webhook-endpoints-accessor
    pub fn inbound_webhook_endpoints(&self) -> impl Iterator<Item = &'a str> + use<'a> {
        self.inbound_webhook_endpoints.iter().map(String::as_str)
    }
    // template:end inbound-webhooks:worker-inbound-webhook-endpoints-accessor

    pub fn spawn<F>(&mut self, name: &'static str, start: impl FnOnce(CancellationToken) -> F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.background.spawn(name, start);
    }

    #[must_use]
    pub fn shutdown(&self) -> CancellationToken {
        self.background.child_token()
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
fn report_failure(message: impl std::fmt::Display) {
    let _ = writeln!(std::io::stderr().lock(), "{message}");
}

pub fn run<I, R>(args: I, register: R) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
    R: FnOnce(&mut Registration<'_>) -> Result<(), BuildError>,
{
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
    // template:begin jobs:worker-operator-dispatch
    if let Some(command) = args.command {
        let request = match operator::Request::try_new(command) {
            Ok(request) => request,
            Err(error) => {
                report_failure(error);
                return ExitCode::from(EXIT_USAGE);
            }
        };
        return reported_exit(operator::run(&args.options, &request));
    }
    // template:end jobs:worker-operator-dispatch
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
        shutdown::RUNTIME_SHUTDOWN_TIMEOUT
            .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
    );
    drop(signals);
    outcome
}

fn load_config_and_runtime(
    options: &LoadOptions,
) -> Result<(service_config::WorkerConfig, tokio::runtime::Runtime), bootstrap::WorkerError> {
    let config = service_config::load_worker(options, BUILD_INFO, shutdown::TAIL)?;
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
                "/missing-jobs-worker-config.toml"
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
