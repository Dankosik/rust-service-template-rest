//! Traces, logs, metrics, and the startup record.

use infra_telemetry::{
    ExporterState, LoggingFormat, LoggingOptions, Metrics, TracingOptions, install_subscriber,
    install_tracer_provider, runtime_metrics,
};
use service_config::{Config, LogFormat, TracesSampler};

use super::{METRICS_MAINTENANCE_INTERVAL, WorkerError};
use crate::background::Background;
use crate::resources::Resources;

pub(super) fn install_observability(
    config: &Config,
    identity: &str,
    resources: &mut Resources,
) -> Result<(Metrics, ExporterState), WorkerError> {
    let tracer_provider = resources
        .tracer_provider
        .insert(install_tracer_provider(&tracing_options(config, identity))?);
    resources.logger = Some(install_subscriber(&LoggingOptions {
        level: &config.log.level,
        format: match config.log.format {
            LogFormat::Json => LoggingFormat::Json,
            LogFormat::Text => LoggingFormat::Text,
        },
        tracer_provider: Some(tracer_provider),
    })?);
    let exporter_state = tracer_provider.exporter_state.clone();
    let metrics = Metrics::install(
        &[
            infra_http::HISTOGRAMS,
            // template:begin jobs:worker-bootstrap-jobs-histograms
            infra_postgres::HISTOGRAMS,
            infra_jobs::HISTOGRAMS,
            // template:end jobs:worker-bootstrap-jobs-histograms
            // template:begin outbound-http:worker-bootstrap-outbound-histogram
            infra_outbound_http::HISTOGRAMS,
            // template:end outbound-http:worker-bootstrap-outbound-histogram
        ]
        .concat(),
    )?;
    metrics.record_trace_exporter(Some(&exporter_state));
    Ok((metrics, exporter_state))
}

pub(super) fn spawn_metrics_tasks(metrics: &Metrics, background: &Background) {
    background.spawn("metrics_upkeep", |cancel| metrics.clone().upkeep(cancel));
    background.spawn("runtime_progress", |cancel| {
        metrics.clone().runtime_progress(cancel)
    });
    background.spawn("runtime_metrics", |cancel| {
        runtime_metrics(METRICS_MAINTENANCE_INTERVAL, cancel)
    });
}

fn tracing_options(config: &Config, identity: &str) -> TracingOptions {
    let otel = &config.observability.otel;
    let sampler = match otel.traces_sampler {
        TracesSampler::AlwaysOn => infra_telemetry::ResolvedSampler::AlwaysOn,
        TracesSampler::AlwaysOff => infra_telemetry::ResolvedSampler::AlwaysOff,
        TracesSampler::TraceIdRatio => {
            infra_telemetry::ResolvedSampler::TraceIdRatio(otel.traces_sampler_arg)
        }
        TracesSampler::ParentBasedTraceIdRatio => {
            infra_telemetry::ResolvedSampler::ParentBasedTraceIdRatio(otel.traces_sampler_arg)
        }
    };
    TracingOptions {
        service_name: identity.to_owned(),
        service_version: config.app.version.clone(),
        vcs_revision: config.app.commit.clone(),
        instance_id: config.app.instance_id.clone(),
        deployment_environment: config.app.env.clone(),
        sampler,
        otlp_endpoint: otel.exporter.otlp_endpoint.clone(),
        otlp_headers: otel.exporter.otlp_headers.clone(),
    }
}

pub(super) fn log_startup_record(
    config: &Config,
    identity: &str,
    // template:begin jobs:worker-bootstrap-log-jobs-parameter
    registry: Option<&infra_jobs::Kinds>,
    // template:end jobs:worker-bootstrap-log-jobs-parameter
    exporter: &ExporterState,
) {
    exporter.log();
    tracing::info!(
        service.name = %identity,
        app.env = %config.app.env,
        app.version = %config.app.version,
        app.commit = %config.app.commit,
        runtime.worker_threads = tokio::runtime::Handle::current().metrics().num_workers(),
        http.addr = %config.http.addr,
        http.drain_timeout = ?config.http.drain_timeout,
        http.grace_period = ?config.http.grace_period,
        observability.metrics.addr = ?config.observability.metrics.addr,
        // template:begin jobs:worker-bootstrap-log-jobs-fields
        postgres.max_connections = config.postgres.max_connections.get(),
        jobs.max_workers = config.jobs.max_workers.get(),
        jobs.kinds = %super::jobs::kind_names(registry),
        // template:end jobs:worker-bootstrap-log-jobs-fields
        log.level = %config.log.level,
        tracing.exporter = exporter.as_str(),
        "jobs_worker_starting"
    );
}
