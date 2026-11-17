use health::{Probe, ReadinessReader, RefreshPolicy};
use infra_http::{Authentication, HardenOptions, HttpContract};
use infra_http_transport::{BoundListener, Server, ServerOptions};
use infra_telemetry::{Metrics, diagnostics_router};
use service_config::WorkerConfig;

use super::WorkerError;
use crate::background::Background;
use crate::resources::{Listeners, Resources};

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

pub(super) fn bind_listeners(
    config: &WorkerConfig,
    metrics: &Metrics,
    readiness: ReadinessReader,
    background: &Background,
    resources: &mut Resources,
) -> Result<(), WorkerError> {
    let options = server_options(config);
    let app = HttpContract::default().with_probes("/").build(
        readiness.clone(),
        Authentication::Public,
        &harden_options(config, readiness.clone()),
    )?;
    let health = resources
        .listeners
        .health
        .insert(app.serve(BoundListener::bind(config.http.addr)?, options));
    watch_listener(Listeners::HEALTH, health, background);
    tracing::info!(addr = %health.local_addr(), "http listener bound");
    if let Some(addr) = config.observability.metrics.addr {
        let routes = diagnostics_router(metrics.clone(), move || readiness.is_ready())
            .merge(infra_http::liveness_router());
        let diagnostics = resources
            .listeners
            .diagnostics
            .insert(Server::bind(addr, routes, options)?);
        watch_listener(Listeners::DIAGNOSTICS, diagnostics, background);
        tracing::info!(addr = %diagnostics.local_addr(), "diagnostics listener bound");
    }
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

fn server_options(config: &WorkerConfig) -> ServerOptions {
    ServerOptions {
        header_read_timeout: config.http.header_read_timeout,
        max_header_bytes: usize::try_from(config.http.max_header_bytes.as_u64())
            .unwrap_or(usize::MAX),
        max_connections: config.http.connection_cap(),
        max_connection_age: config.http.connection_age_cap(),
    }
}

fn harden_options(config: &WorkerConfig, readiness: ReadinessReader) -> HardenOptions {
    HardenOptions {
        max_body_bytes: usize::try_from(config.http.max_body_bytes.as_u64()).unwrap_or(usize::MAX),
        request_timeout: config.http.request_timeout,
        max_in_flight: config.http.in_flight_cap(),
        admission_wait: config.http.admission_wait_cap(),
        log_health_probes: config.http.access_log_health_probes,
        draining: Some(readiness),
    }
}

pub(super) fn refresh_policy(config: &WorkerConfig) -> Result<RefreshPolicy, WorkerError> {
    RefreshPolicy::try_new(
        config.health.refresh_interval,
        config.health.probe_budget,
        config.health.failure_threshold,
    )
    .map_err(|err| service_config::ValidationError::new("health", err.to_string()).into())
}
