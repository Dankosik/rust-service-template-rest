use infra_telemetry::{ExporterState, Metrics, ResolvedSampler, TracingOptions};
use service_config::{Config, TracesSampler};

use super::BootstrapError;

pub(super) fn install_metrics() -> Result<Metrics, BootstrapError> {
    Ok(Metrics::install(&histogram_specs())?)
}

fn histogram_specs() -> Vec<(&'static str, &'static [f64])> {
    [
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
        infra_outbound_grpc::HISTOGRAMS,
        // template:end grpc:bootstrap-grpc-histograms
        // template:begin cache:service-bootstrap-cache-histogram
        infra_cache::HISTOGRAMS,
        // template:end cache:service-bootstrap-cache-histogram
        // template:begin object-storage:service-bootstrap-object-storage-histogram
        infra_object_storage::HISTOGRAMS,
        // template:end object-storage:service-bootstrap-object-storage-histogram
        infra_temporal::HISTOGRAMS,
    ]
    .concat()
}

pub(super) fn tracing_options(config: &Config) -> TracingOptions<'_> {
    let otel = &config.observability().otel;
    let sampler = match otel.traces_sampler {
        TracesSampler::AlwaysOn => ResolvedSampler::AlwaysOn,
        TracesSampler::AlwaysOff => ResolvedSampler::AlwaysOff,
        TracesSampler::TraceIdRatio(ratio) => ResolvedSampler::TraceIdRatio(ratio),
        TracesSampler::ParentBasedTraceIdRatio(ratio) => {
            ResolvedSampler::ParentBasedTraceIdRatio(ratio)
        }
    };
    TracingOptions {
        service_name: &otel.service_name,
        service_version: &config.app().version,
        vcs_revision: &config.app().commit,
        instance_id: config.app().instance_id.as_deref(),
        deployment_environment: config.app().env(),
        sampler,
        otlp_endpoint: otel.exporter.otlp_endpoint.as_deref(),
        otlp_headers: otel.exporter.otlp_headers.as_ref(),
    }
}

pub(super) fn log_startup_summary(config: &Config, exporter: &ExporterState) {
    exporter.log();
    tracing::info!(
        app.env = %config.app().env(),
        app.version = %config.app().version,
        app.commit = %config.app().commit,
        runtime.worker_threads = tokio::runtime::Handle::current().metrics().num_workers(),
        http.addr = %config.http().listener().addr,
        http.request_timeout = ?config.http().listener().request_timeout,
        http.drain_timeout = ?config.http().drain_timeout,
        http.grace_period = ?config.http().grace_period,
        observability.metrics.addr = ?config.observability().metrics.addr,
        // template:begin postgres:bootstrap-startup-log-postgres
        postgres.enabled = config.postgres().is_some(),
        // template:end postgres:bootstrap-startup-log-postgres
        // template:begin service-auth:bootstrap-startup-log-service-auth
        service_auth.mode = config.service_auth().mode(),
        // template:end service-auth:bootstrap-startup-log-service-auth
        temporal.enabled = config.temporal().is_some(),
        tracing.exporter = exporter.as_str(),
        "service_starting"
    );
}

#[cfg(test)]
mod tests {
    use crate::bootstrap::test_config;

    use super::*;

    #[test]
    fn tracing_options_attaches_the_ratio_only_to_ratio_variants() {
        let for_sampler = |sampler| {
            test_config(&[
                ("APP__OBSERVABILITY__OTEL__TRACES_SAMPLER", sampler),
                ("APP__OBSERVABILITY__OTEL__TRACES_SAMPLER_ARG", "0.5"),
            ])
        };
        let config = for_sampler("always_on");
        let options = tracing_options(&config);
        std::assert_matches!(options.sampler, ResolvedSampler::AlwaysOn);

        let config = for_sampler("traceidratio");
        let options = tracing_options(&config);
        std::assert_matches!(options.sampler, ResolvedSampler::TraceIdRatio(ratio) if (ratio - 0.5).abs() < f64::EPSILON
        );
    }

    #[test]
    fn install_metrics_registers_every_temporal_histogram_bucket_set() {
        let specs = histogram_specs();
        for &(name, buckets) in infra_temporal::HISTOGRAMS {
            assert!(
                specs.contains(&(name, buckets)),
                "missing Temporal histogram spec {name}"
            );
        }
    }
}
