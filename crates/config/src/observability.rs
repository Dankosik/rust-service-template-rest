//! Diagnostics listener and OpenTelemetry trace export.

use secrecy::SecretString;
use serde::Deserialize;

use crate::validate::{ValidationError, non_empty, socket_addr};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ObservabilityConfig {
    pub metrics: MetricsConfig,
    pub otel: OtelConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct MetricsConfig {
    /// Private Prometheus diagnostics listener. `:port` binds IPv4
    /// all-interfaces (`0.0.0.0`) because the scraper runs in another pod;
    /// deployment network policy must keep it private. Hostnames are
    /// refused; load does not look them up. Empty disables HTTP exposition.
    pub addr: String,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            addr: ":9090".to_owned(),
        }
    }
}

impl MetricsConfig {
    /// The diagnostics listen address, or `None` when exposition is disabled.
    ///
    /// # Errors
    ///
    /// Returns the validation error for a malformed address.
    pub fn listen_addr(&self) -> Result<Option<std::net::SocketAddr>, ValidationError> {
        if self.addr.trim().is_empty() {
            return Ok(None);
        }
        socket_addr("observability.metrics.addr", &self.addr).map(Some)
    }
}

/// Trace samplers this service accepts, by their OpenTelemetry names.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TracesSampler {
    AlwaysOn,
    AlwaysOff,
    #[serde(rename = "traceidratio")]
    TraceIdRatio,
    #[default]
    #[serde(rename = "parentbased_traceidratio")]
    ParentBasedTraceIdRatio,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OtelConfig {
    /// `service.name` resource attribute and the PostgreSQL
    /// `application_name` for pooled and migrator sessions. The initializer
    /// rewrites this default for a derived service. A distinct database
    /// session label is not a configuration axis.
    pub service_name: String,
    pub traces_sampler: TracesSampler,
    /// Ratio for the ratio-based samplers, in `[0, 1]`. Always validated
    /// (finite and in range) even when the selected sampler ignores it.
    pub traces_sampler_arg: f64,
    pub exporter: OtelExporterConfig,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            service_name: "service".to_owned(),
            traces_sampler: TracesSampler::default(),
            traces_sampler_arg: 0.10,
            exporter: OtelExporterConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OtelExporterConfig {
    /// OTLP/HTTP traces endpoint. A collector root without a path resolves
    /// to `/v1/traces`. Missing, empty, or whitespace-only (after trim) is
    /// vacant and falls back to `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` then
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` presence, including empty; when those
    /// variables are unset the exporter stays disabled.
    #[serde(default, deserialize_with = "crate::app::occupied_string")]
    pub otlp_endpoint: Option<String>,
    /// Collector credential as `key=value,key=value`. Environment only.
    /// Missing, empty, or whitespace-only is absent (`None`).
    #[serde(default, deserialize_with = "crate::secret_policy::occupied_secret")]
    pub otlp_headers: Option<SecretString>,
}

impl OtelExporterConfig {
    /// Whether a typed credential is configured.
    #[must_use]
    pub fn has_headers(&self) -> bool {
        self.otlp_headers.is_some()
    }
}

impl ObservabilityConfig {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        self.metrics.listen_addr()?;
        non_empty("observability.otel.service_name", &self.otel.service_name)?;
        let arg = self.otel.traces_sampler_arg;
        if !arg.is_finite() {
            return Err(ValidationError::new(
                "observability.otel.traces_sampler_arg",
                "must be finite",
            ));
        }
        if !(0.0..=1.0).contains(&arg) {
            return Err(ValidationError::new(
                "observability.otel.traces_sampler_arg",
                format!("must be in range [0, 1], got {arg}"),
            ));
        }
        if let Some(endpoint) = self.otel.exporter.otlp_endpoint.as_deref() {
            let is_http_url = endpoint.starts_with("http://") || endpoint.starts_with("https://");
            if !is_http_url {
                return Err(ValidationError::new(
                    "observability.otel.exporter.otlp_endpoint",
                    "must be an http:// or https:// URL",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampler_names_follow_opentelemetry_spelling() {
        #[derive(Deserialize)]
        struct Probe {
            sampler: TracesSampler,
        }
        for (text, expected) in [
            ("always_on", TracesSampler::AlwaysOn),
            ("always_off", TracesSampler::AlwaysOff),
            ("traceidratio", TracesSampler::TraceIdRatio),
            (
                "parentbased_traceidratio",
                TracesSampler::ParentBasedTraceIdRatio,
            ),
        ] {
            let probe: Probe = toml::from_str(&format!("sampler = \"{text}\"")).unwrap();
            assert_eq!(probe.sampler, expected, "{text}");
        }
        assert!(toml::from_str::<Probe>("sampler = \"jaeger_remote\"").is_err());
    }

    #[test]
    fn sampler_arg_bounds() {
        let mut cfg = ObservabilityConfig::default();
        cfg.otel.traces_sampler_arg = 1.5;
        assert_eq!(
            cfg.validate().unwrap_err().key,
            "observability.otel.traces_sampler_arg"
        );
        cfg.otel.traces_sampler_arg = f64::NAN;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn headers_never_appear_in_debug_output() {
        let cfg = OtelExporterConfig {
            otlp_headers: Some(SecretString::from(
                "authorization=Bearer topsecret".to_owned(),
            )),
            ..OtelExporterConfig::default()
        };
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("topsecret"), "{rendered}");
        assert!(cfg.has_headers());
    }

    #[test]
    fn empty_metrics_addr_disables_the_listener() {
        let mut cfg = ObservabilityConfig::default();
        cfg.metrics.addr = String::new();
        cfg.validate().unwrap();
    }

    #[test]
    fn occupied_otlp_endpoint_must_be_http() {
        let mut cfg = ObservabilityConfig::default();
        cfg.otel.exporter.otlp_endpoint = Some("collector:4318".into());
        assert_eq!(
            cfg.validate().unwrap_err().key,
            "observability.otel.exporter.otlp_endpoint"
        );
        cfg.otel.exporter.otlp_endpoint = Some("http://collector:4318".into());
        cfg.validate().unwrap();
    }
}
