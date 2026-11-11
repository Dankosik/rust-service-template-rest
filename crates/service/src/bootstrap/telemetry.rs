//! Metrics registration, trace options, and the startup record.

use infra_telemetry::{ExporterState, Metrics, ResolvedSampler, TracingOptions};
use service_config::{Config, TracesSampler};

use super::BootstrapError;

pub(super) fn install_metrics() -> Result<Metrics, BootstrapError> {
    Ok(Metrics::install(
        &[
            infra_http::HISTOGRAMS,
            // template:begin authn:bootstrap-authn-histograms
            infra_bearerauthn::HISTOGRAMS,
            // template:end authn:bootstrap-authn-histograms
            // template:begin postgres:bootstrap-postgres-histograms
            infra_postgres::HISTOGRAMS,
            // template:end postgres:bootstrap-postgres-histograms
            // template:begin outbound-http:service-bootstrap-outbound-histogram
            infra_outbound_http::HISTOGRAMS,
            // template:end outbound-http:service-bootstrap-outbound-histogram
            // template:begin grpc:bootstrap-grpc-histograms
            infra_grpc::HISTOGRAMS,
            // template:end grpc:bootstrap-grpc-histograms
            // template:begin cache:service-bootstrap-cache-histogram
            infra_cache::HISTOGRAMS,
            // template:end cache:service-bootstrap-cache-histogram
            // template:begin object-storage:service-bootstrap-object-storage-histogram
            infra_object_storage::HISTOGRAMS,
            // template:end object-storage:service-bootstrap-object-storage-histogram
        ]
        .concat(),
    )?)
}

pub(super) fn tracing_options(config: &Config) -> TracingOptions {
    let otel = &config.observability.otel;
    let sampler = match otel.traces_sampler {
        TracesSampler::AlwaysOn => ResolvedSampler::AlwaysOn,
        TracesSampler::AlwaysOff => ResolvedSampler::AlwaysOff,
        TracesSampler::TraceIdRatio => ResolvedSampler::TraceIdRatio(otel.traces_sampler_arg),
        TracesSampler::ParentBasedTraceIdRatio => {
            ResolvedSampler::ParentBasedTraceIdRatio(otel.traces_sampler_arg)
        }
    };
    TracingOptions {
        service_name: otel.service_name.clone(),
        service_version: config.app.version.clone(),
        vcs_revision: config.app.commit.clone(),
        instance_id: config.app.instance_id.clone(),
        deployment_environment: config.app.env.clone(),
        sampler,
        otlp_endpoint: otel.exporter.otlp_endpoint.clone(),
        otlp_headers: otel.exporter.otlp_headers.clone(),
    }
}

/// One record with the non-secret facts an operator needs on every boot.
pub(super) fn log_startup_summary(config: &Config, exporter: &ExporterState) {
    exporter.log();
    tracing::info!(
        app.env = %config.app.env,
        app.version = %config.app.version,
        app.commit = %config.app.commit,
        runtime.worker_threads = tokio::runtime::Handle::current().metrics().num_workers(),
        http.addr = %config.http.addr,
        http.request_timeout = ?config.http.request_timeout,
        http.drain_timeout = ?config.http.drain_timeout,
        http.grace_period = ?config.http.grace_period,
        observability.metrics.addr = ?config.observability.metrics.addr,
        // template:begin postgres:bootstrap-startup-log-postgres
        postgres.enabled = config.postgres.enabled,
        // template:end postgres:bootstrap-startup-log-postgres
        // template:begin service-auth:bootstrap-startup-log-service-auth
        service_auth.mode = config.service_auth.mode(),
        // template:end service-auth:bootstrap-startup-log-service-auth
        log.level = %config.log.level,
        tracing.exporter = exporter.as_str(),
        "service_starting"
    );
}

#[cfg(test)]
mod tests {
    use service_config::OtelConfig;

    use super::*;

    #[test]
    fn tracing_options_attaches_the_ratio_only_to_ratio_variants() {
        let mut config = Config::default();
        config.observability.otel = OtelConfig {
            traces_sampler: TracesSampler::AlwaysOn,
            traces_sampler_arg: 0.5,
            ..OtelConfig::default()
        };
        let options = tracing_options(&config);
        assert!(matches!(options.sampler, ResolvedSampler::AlwaysOn));

        config.observability.otel.traces_sampler = TracesSampler::TraceIdRatio;
        let options = tracing_options(&config);
        assert!(
            matches!(options.sampler, ResolvedSampler::TraceIdRatio(ratio) if (ratio - 0.5).abs() < f64::EPSILON)
        );
    }
}
