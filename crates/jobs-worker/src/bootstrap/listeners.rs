//! The health and diagnostics listeners and the readiness they serve.

use health::{Probe, Readiness, RefreshPolicy};
use infra_http::{HardenOptions, Server, ServerOptions};
use infra_telemetry::{Metrics, diagnostics_router};
use service_config::Config;

use super::{READINESS_PROGRESS, WorkerError};
use crate::background::Background;
use crate::shutdown::{Listeners, Resources};

/// One probe per dependency that admission opened.
pub(super) fn readiness_probes(resources: &Resources) -> Vec<Box<dyn Probe>> {
    #[allow(unused_mut, reason = "retained profiles add their probes")]
    let mut probes: Vec<Box<dyn Probe>> = Vec::new();
    // template:begin jobs:worker-bootstrap-listener-jobs-probe
    probes.extend(super::postgres::probe(resources));
    // template:end jobs:worker-bootstrap-listener-jobs-probe
    // template:begin messaging:worker-bootstrap-listener-messaging-probe
    probes.extend(super::messaging::probe(resources));
    // template:end messaging:worker-bootstrap-listener-messaging-probe
    probes
}

pub(super) async fn bind_listeners(
    config: &Config,
    metrics: &Metrics,
    readiness: &Readiness,
    background: &Background,
    resources: &mut Resources,
) -> Result<(), WorkerError> {
    let options = server_options(config);
    let routes = infra_http::finalize_public(infra_http::router())?.with_state(readiness.reader());
    let app = infra_http::harden(routes, &harden_options(config));
    let health = resources
        .listeners
        .health
        .insert(Server::bind(config.http.addr, app, options).await?);
    watch_listener(Listeners::HEALTH, health, background);
    tracing::info!(addr = %health.local_addr(), "http listener bound");
    if let Some(addr) = config.observability.metrics.addr {
        // Liveness is served here as well, as in the service, so one probe
        // target fits both processes.
        let routes = diagnostics_router(metrics.clone()).merge(infra_http::liveness_router());
        let diagnostics = resources
            .listeners
            .diagnostics
            .insert(Server::bind(addr, routes, options).await?);
        watch_listener(Listeners::DIAGNOSTICS, diagnostics, background);
        tracing::info!(addr = %diagnostics.local_addr(), "diagnostics listener bound");
    }
    Ok(())
}

pub(super) fn start_progress(
    readiness: &Readiness,
    background: &Background,
) -> Result<(), WorkerError> {
    readiness.arm_progress().map_err(|error| {
        if readiness.progress_loss().is_some() {
            WorkerError::BackgroundStopped(READINESS_PROGRESS)
        } else {
            WorkerError::Admission(error)
        }
    })?;
    let observed = readiness.clone();
    let reporter = background.clone();
    background.spawn(READINESS_PROGRESS, |cancel| async move {
        if observed
            .wait_for_progress_loss(cancel.clone())
            .await
            .is_some()
        {
            reporter.record_failure(READINESS_PROGRESS);
            cancel.cancelled().await;
        }
    });
    let readiness = readiness.clone();
    background.spawn("readiness_refresher", |cancel| async move {
        readiness.refresh_until(cancel).await;
    });
    Ok(())
}

fn watch_listener(name: &'static str, server: &Server, background: &Background) {
    let failure = server.accept_failure();
    background.spawn(name, |cancel| async move {
        tokio::select! {
            biased;
            failure = failure => tracing::error!(listener = name, ?failure, "listener_failed"),
            () = cancel.cancelled() => {}
        }
    });
}

fn server_options(config: &Config) -> ServerOptions {
    ServerOptions {
        header_read_timeout: config.http.header_read_timeout,
        max_header_bytes: usize::try_from(config.http.max_header_bytes.as_u64())
            .unwrap_or(usize::MAX),
        max_connections: config.http.connection_cap(),
        max_connection_age: config.http.connection_age(),
    }
}

fn harden_options(config: &Config) -> HardenOptions {
    HardenOptions {
        max_body_bytes: usize::try_from(config.http.max_body_bytes.as_u64()).unwrap_or(usize::MAX),
        request_timeout: config.http.request_timeout,
        max_in_flight: config.http.in_flight_cap(),
        log_health_probes: config.http.access_log_health_probes,
    }
}

pub(super) fn refresh_policy(config: &Config) -> RefreshPolicy {
    RefreshPolicy {
        interval: config.health.refresh_interval,
        probe_budget: config.health.probe_budget,
        failure_threshold: config.health.failure_threshold,
    }
}
