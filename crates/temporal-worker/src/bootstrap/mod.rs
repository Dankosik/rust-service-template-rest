mod listeners;
mod telemetry;
mod temporal;

use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt;
use health::{BoxedProbe, PreparedReadiness, Readiness};
use infra_telemetry::install_panic_hook;
use infra_temporal::worker::{
    PreparedTemporalWorker, TemporalWorkerHandle, WorkerFatalError, WorkerPrepareError,
    WorkerStartError,
};
use service_config::TemporalWorkerConfig;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::background::{Background, wait_for_failure};
use crate::resources::{Listeners, Resources};
use crate::shutdown;
use crate::signals::{SignalError, Signals};
use crate::{BuildError, Register, Registration};

const METRICS_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkerError {
    #[error(transparent)]
    Load(#[from] service_config::Error),
    #[error("configuration is invalid: {0}")]
    Config(#[from] service_config::ValidationError),
    #[error("build tokio runtime: {0}")]
    Runtime(#[source] std::io::Error),
    #[error("install stop signal handlers: {0}")]
    Signals(#[source] std::io::Error),
    #[error(transparent)]
    SignalClosed(#[from] SignalError),
    #[error("worker lifecycle panicked; payload withheld")]
    Panicked,
    #[error("listener {0} stopped without a stop signal")]
    ListenerStopped(&'static str),
    #[error(transparent)]
    Tracing(#[from] infra_telemetry::TracingError),
    #[error(transparent)]
    Logging(#[from] infra_telemetry::LoggingError),
    #[error(transparent)]
    Metrics(#[from] infra_telemetry::MetricsError),
    #[error("temporal.enabled must be true to run the temporal worker")]
    TemporalDisabled,
    #[error(transparent)]
    TemporalConnect(#[from] infra_temporal::ConnectError),
    #[error("worker registration failed: {0}")]
    Registration(#[from] BuildError),
    #[error("temporal worker prepare failed: {0}")]
    TemporalPrepare(#[from] WorkerPrepareError),
    #[error("temporal worker start failed: {0}")]
    TemporalStart(#[from] WorkerStartError),
    #[error("temporal worker stopped with fatal error: {0}")]
    TemporalWorkerFailed(#[from] WorkerFatalError),
    #[error("background task {0} stopped without a stop signal")]
    BackgroundStopped(&'static str),
    #[error(transparent)]
    Server(#[from] infra_http_transport::BindError),
    #[error(transparent)]
    HttpContract(#[from] infra_http::FinalizeError),
    #[error("startup admission: {0}")]
    Admission(#[from] health::NotReady),
}

#[derive(Debug)]
enum Ended {
    Signal,
    Failure(WorkerError),
}

struct Prepared {
    readiness: PreparedReadiness,
    worker: PreparedTemporalWorker,
}

pub(crate) async fn serve(
    config: TemporalWorkerConfig,
    register: Register<'_>,
    signals: &mut Signals,
    deadline: &mut Option<Instant>,
) -> Result<shutdown::Outcome, WorkerError> {
    let mut background = Background::new();
    let mut background_failures = background.failure_observer();
    let mut resources = Resources::default();
    let ended = Box::pin(
        AssertUnwindSafe(run_worker(
            &config,
            register,
            signals,
            &mut background,
            &mut background_failures,
            &mut resources,
        ))
        .catch_unwind(),
    )
    .await
    .unwrap_or(Ended::Failure(WorkerError::Panicked));
    let ended = match ended {
        Ended::Failure(_) => ended,
        Ended::Signal => {
            pending_failure(&resources, &background_failures).map_or(ended, Ended::Failure)
        }
    };
    let deadline = *deadline.insert(shutdown::stop_deadline(
        signals.first_stop(),
        config.http().grace_period,
    ));
    if let Ended::Failure(error) = &ended {
        report_worker_failure(error, resources.telemetry.logger.is_some());
    }
    let report = shutdown::run(shutdown::Plan {
        http: config.http(),
        finalize_budget: config.temporal_worker().finalize_budget(),
        resources,
        background,
        signals,
        deadline,
    })
    .await;
    match ended {
        Ended::Failure(error) => Err(error),
        Ended::Signal => match report.worker_fatal {
            Some(fatal) => Err(WorkerError::TemporalWorkerFailed(fatal)),
            None => Ok(report.outcome),
        },
    }
}

fn report_worker_failure(error: &WorkerError, has_logger: bool) {
    if has_logger {
        tracing::error!(error = %error, "temporal worker failed");
    } else {
        crate::report_failure(error);
    }
}

async fn run_worker(
    config: &TemporalWorkerConfig,
    register: Register<'_>,
    signals: &mut Signals,
    background: &mut Background,
    background_failures: &mut watch::Receiver<Option<&'static str>>,
    resources: &mut Resources,
) -> Ended {
    match start_worker(
        config,
        register,
        signals,
        background,
        background_failures,
        resources,
    )
    .await
    {
        Ok(()) => wait_for_stop(resources, background_failures, signals).await,
        Err(ended) => ended,
    }
}

async fn start_worker(
    config: &TemporalWorkerConfig,
    register: Register<'_>,
    signals: &mut Signals,
    background: &mut Background,
    background_failures: &mut watch::Receiver<Option<&'static str>>,
    resources: &mut Resources,
) -> Result<(), Ended> {
    let prepared = tokio::select! {
        biased;
        result = signals.wait() => Err(signal_ended(result)),
        task = wait_for_failure(background_failures) => Err(Ended::Failure(WorkerError::BackgroundStopped(task))),
        result = prepare(config, register, background, resources) => result.map_err(Ended::Failure),
    };
    let prepared = prepared.map_err(|ended| {
        pending_failure(resources, background_failures).map_or(ended, Ended::Failure)
    })?;
    check_pending_end(resources, background_failures, signals)?;
    let handle = prepared
        .worker
        .start(tokio::runtime::Handle::current())
        .map_err(|error| Ended::Failure(error.into()))?;
    resources.work.temporal = Some(handle);
    activate_readiness(
        prepared.readiness,
        resources,
        background,
        background_failures,
        signals,
    )
    .await?;
    tracing::info!("temporal_worker_ready");
    Ok(())
}

async fn activate_readiness(
    mut readiness: PreparedReadiness,
    resources: &Resources,
    background: &mut Background,
    background_failures: &mut watch::Receiver<Option<&'static str>>,
    signals: &mut Signals,
) -> Result<(), Ended> {
    let freshness = tokio::select! {
        biased;
        ended = wait_for_stop(resources, background_failures, signals) => return Err(ended),
        result = readiness.ensure_fresh() => result,
    };
    check_pending_end(resources, background_failures, signals)?;
    let refresher = freshness
        .and_then(|()| readiness.activate())
        .map_err(|error| Ended::Failure(WorkerError::Admission(error)))?;
    background.spawn("readiness_refresher", |cancel| {
        refresher.refresh_until(cancel)
    });
    Ok(())
}

fn signal_ended(result: Result<(), SignalError>) -> Ended {
    match result {
        Ok(()) => Ended::Signal,
        Err(error) => Ended::Failure(error.into()),
    }
}

fn check_pending_end(
    resources: &Resources,
    background_failures: &watch::Receiver<Option<&'static str>>,
    signals: &mut Signals,
) -> Result<(), Ended> {
    if let Some(error) = pending_failure(resources, background_failures) {
        return Err(Ended::Failure(error));
    }
    match signals.take_pending() {
        Ok(false) => Ok(()),
        Ok(true) => Err(Ended::Signal),
        Err(error) => Err(Ended::Failure(error.into())),
    }
}

fn pending_failure(
    resources: &Resources,
    background_failures: &watch::Receiver<Option<&'static str>>,
) -> Option<WorkerError> {
    if let Some(task) = *background_failures.borrow() {
        return Some(WorkerError::BackgroundStopped(task));
    }
    let Listeners {
        health,
        diagnostics,
    } = &resources.listeners;
    for (name, server) in [
        (Listeners::HEALTH, health.as_ref()),
        (Listeners::DIAGNOSTICS, diagnostics.as_ref()),
    ] {
        if let Some(server) = server
            && server.accept_failure().now_or_never().is_some()
        {
            return Some(WorkerError::ListenerStopped(name));
        }
    }
    if resources
        .work
        .temporal
        .as_ref()
        .is_some_and(TemporalWorkerHandle::has_failed)
    {
        return Some(WorkerError::TemporalWorkerFailed(WorkerFatalError::Fatal));
    }
    None
}

async fn prepare(
    config: &TemporalWorkerConfig,
    register: Register<'_>,
    background: &mut Background,
    resources: &mut Resources,
) -> Result<Prepared, WorkerError> {
    let identity = worker_identity(&config.observability().otel.service_name);
    let (metrics, exporter_state) =
        telemetry::install_observability(config, &identity, resources).await?;
    raise_open_file_limit(config.finite_listener_connection_caps());
    install_panic_hook();
    let Some(temporal_cfg) = config.temporal() else {
        return Err(WorkerError::TemporalDisabled);
    };
    telemetry::log_startup_record(config, &identity, temporal_cfg, &exporter_state);
    telemetry::spawn_metrics_tasks(&metrics, background);

    let connection = temporal::open(
        temporal_cfg,
        config,
        background,
        &mut resources.dependencies.temporal,
    )
    .await?;
    let client = connection.client();

    let mut registration = Registration::new(
        config,
        &client,
        &mut resources.dependencies.close_hooks,
        background,
    );
    register(&mut registration)?;
    let (registry, custom_probes) = registration.into_parts();

    let worker_options = temporal::worker_options(config.temporal_worker());
    let deployment = temporal::deployment_metadata(config.temporal_worker());
    let worker =
        PreparedTemporalWorker::prepare(&connection, &worker_options, deployment, registry)?;

    let mut probes = Vec::with_capacity(1 + custom_probes.len());
    probes.push(BoxedProbe::new(worker.probe()));
    probes.extend(custom_probes);

    Ok(Prepared {
        readiness: prepare_readiness(config, &metrics, background, resources, probes).await?,
        worker,
    })
}

fn raise_open_file_limit(connection_caps: u64) {
    match rlimit::increase_nofile_limit(u64::MAX) {
        Err(error) => tracing::warn!(error = %error, "open_file_limit_unchanged"),
        Ok(open_file_limit) if open_file_limit < connection_caps => {
            tracing::warn!(
                open_file_limit,
                connection_caps,
                "open_file_limit_below_connection_caps"
            );
        }
        Ok(_) => {}
    }
}

async fn prepare_readiness(
    config: &TemporalWorkerConfig,
    metrics: &infra_telemetry::Metrics,
    background: &mut Background,
    resources: &mut Resources,
    probes: Vec<BoxedProbe>,
) -> Result<PreparedReadiness, WorkerError> {
    let readiness = Readiness::new(probes, listeners::refresh_policy(config)?);
    let reader = resources.readiness.insert(readiness.control()).reader();
    listeners::bind_listeners(config, metrics, reader, background, resources)?;
    Ok(readiness.prepare().await?)
}

async fn temporal_worker_failure(resources: &Resources) -> WorkerFatalError {
    match &resources.work.temporal {
        Some(worker) => worker.wait_failed().await,
        None => std::future::pending().await,
    }
}

async fn wait_for_stop(
    resources: &Resources,
    background_failures: &mut watch::Receiver<Option<&'static str>>,
    signals: &mut Signals,
) -> Ended {
    tokio::select! {
        biased;
        result = signals.wait() => {
            pending_failure(resources, background_failures)
                .map_or_else(|| signal_ended(result), Ended::Failure)
        }
        task = wait_for_failure(background_failures) => {
            Ended::Failure(WorkerError::BackgroundStopped(task))
        }
        fatal = temporal_worker_failure(resources) => {
            Ended::Failure(WorkerError::TemporalWorkerFailed(fatal))
        }
    }
}

const IDENTITY_SUFFIX: &str = "-temporal-worker";

fn worker_identity(service_name: &str) -> String {
    format!("{service_name}{IDENTITY_SUFFIX}")
}
