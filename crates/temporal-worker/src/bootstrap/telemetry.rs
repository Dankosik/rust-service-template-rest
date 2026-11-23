use infra_telemetry::{
    ExporterState, LoggingFormat, LoggingOptions, Metrics, TracingOptions, install_subscriber,
    install_tracer_provider, runtime_metrics,
};
use service_config::{LogFormat, TemporalClientConfig, TemporalWorkerConfig, TracesSampler};

use super::{METRICS_MAINTENANCE_INTERVAL, WorkerError};
use crate::background::Background;
use crate::resources::Resources;

pub(super) async fn install_observability(
    config: &TemporalWorkerConfig,
    identity: &str,
    resources: &mut Resources,
) -> Result<(Metrics, ExporterState), WorkerError> {
    let tracer_provider = resources
        .telemetry
        .tracer_provider
        .insert(install_tracer_provider(&tracing_options(config, identity)).await?);
    resources.telemetry.logger = Some(install_subscriber(&LoggingOptions {
        level: &config.log().level,
        format: match config.log().format {
            LogFormat::Json => LoggingFormat::Json,
            LogFormat::Text => LoggingFormat::Text,
        },
        tracer_provider: Some(tracer_provider),
    })?);
    let exporter_state = tracer_provider.exporter_state().clone();
    let metrics = Metrics::install(&[infra_http::HISTOGRAMS, infra_temporal::HISTOGRAMS].concat())?;
    metrics.record_trace_exporter(&exporter_state);
    Ok((metrics, exporter_state))
}

pub(super) fn spawn_metrics_tasks(metrics: &Metrics, background: &mut Background) {
    background.spawn("metrics_upkeep", |cancel| metrics.clone().upkeep(cancel));
    background.spawn("runtime_progress", |cancel| {
        metrics.clone().runtime_progress(cancel)
    });
    background.spawn("runtime_metrics", |cancel| {
        runtime_metrics(METRICS_MAINTENANCE_INTERVAL, cancel)
    });
}

fn tracing_options<'a>(config: &'a TemporalWorkerConfig, identity: &'a str) -> TracingOptions<'a> {
    let otel = &config.observability().otel;
    let sampler = match otel.traces_sampler {
        TracesSampler::AlwaysOn => infra_telemetry::ResolvedSampler::AlwaysOn,
        TracesSampler::AlwaysOff => infra_telemetry::ResolvedSampler::AlwaysOff,
        TracesSampler::TraceIdRatio(ratio) => infra_telemetry::ResolvedSampler::TraceIdRatio(ratio),
        TracesSampler::ParentBasedTraceIdRatio(ratio) => {
            infra_telemetry::ResolvedSampler::ParentBasedTraceIdRatio(ratio)
        }
    };
    TracingOptions {
        service_name: identity,
        service_version: &config.app().version,
        vcs_revision: &config.app().commit,
        instance_id: config.app().instance_id.as_deref(),
        deployment_environment: config.app().env(),
        sampler,
        otlp_endpoint: otel.exporter.otlp_endpoint.as_deref(),
        otlp_headers: otel.exporter.otlp_headers.as_ref(),
    }
}

pub(super) fn log_startup_record(
    config: &TemporalWorkerConfig,
    identity: &str,
    temporal: &TemporalClientConfig,
    exporter: &ExporterState,
) {
    exporter.log();
    tracing::info!(
        service.name = %identity,
        app.env = %config.app().env(),
        app.version = %config.app().version,
        app.commit = %config.app().commit,
        runtime.worker_threads = tokio::runtime::Handle::current().metrics().num_workers(),
        http.addr = %config.http().listener().addr,
        http.drain_timeout = ?config.http().drain_timeout,
        http.grace_period = ?config.http().grace_period,
        observability.metrics.addr = ?config.observability().metrics.addr,
        temporal.namespace = %temporal.namespace(),
        temporal.task_queue = ?config.temporal_worker().task_queue(),
        temporal.graceful_shutdown_period = ?config.temporal_worker().graceful_shutdown_period(),
        temporal.activity_cancel_cleanup = ?config.temporal_worker().activity_cancel_cleanup(),
        temporal.finalize_budget = ?config.temporal_worker().finalize_budget(),
        tracing.exporter = exporter.as_str(),
        "temporal_worker_starting"
    );
}
