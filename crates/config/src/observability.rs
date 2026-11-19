use std::net::SocketAddr;

use secrecy::SecretString;
use serde::Deserialize;

use crate::validate::{ValidationError, Violations, non_blank};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ObservabilityFields<Metrics, Otel> {
    metrics: Metrics,
    pub(crate) otel: Otel,
}

type ObservabilityInput = ObservabilityFields<MetricsConfig, OtelConfig>;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(from = "ObservabilityInput")]
pub struct ObservabilityConfig {
    pub metrics: MetricsConfig,
    pub otel: OtelConfig,
}

impl From<ObservabilityInput> for ObservabilityConfig {
    fn from(input: ObservabilityInput) -> Self {
        Self {
            metrics: input.metrics,
            otel: input.otel,
        }
    }
}

#[serde_with::serde_as]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, educe::Educe)]
#[educe(Default)]
#[serde(deny_unknown_fields, default)]
pub struct MetricsConfig {
    /// Private Prometheus diagnostics listener. `:port` binds IPv4
    /// all-interfaces (`0.0.0.0`) because the scraper runs in another pod;
    /// deployment network policy must keep it private. Hostnames are
    /// refused; load does not look them up. Empty disables HTTP exposition
    /// and leaves liveness on the application listener only.
    #[serde_as(as = "crate::de::BlankAsNone<crate::de::ListenAddr>")]
    #[educe(Default(expression = Some(SocketAddr::from(([0, 0, 0, 0], 9090)))))]
    pub addr: Option<SocketAddr>,
}

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

#[serde_with::serde_as]
#[derive(Debug, Deserialize, educe::Educe)]
#[educe(Default)]
#[serde(transparent)]
struct TracesSamplerArgInput(
    #[serde_as(as = "crate::de::StrictRatio")]
    #[educe(Default = 0.10)]
    f64,
);

#[serde_with::serde_as]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct OtelFields<Sampler, SamplerArg, Exporter> {
    #[serde_as(as = "crate::de::StrictString")]
    pub(crate) service_name: String,
    traces_sampler: Sampler,
    traces_sampler_arg: SamplerArg,
    exporter: Exporter,
}

impl<Sampler: Default, SamplerArg: Default, Exporter: Default> Default
    for OtelFields<Sampler, SamplerArg, Exporter>
{
    fn default() -> Self {
        Self {
            service_name: "service".to_owned(),
            traces_sampler: Sampler::default(),
            traces_sampler_arg: SamplerArg::default(),
            exporter: Exporter::default(),
        }
    }
}

type OtelInput =
    OtelFields<crate::de::StrictEnum<TracesSampler>, TracesSamplerArgInput, OtelExporterConfig>;

#[derive(Clone, Debug, Deserialize)]
#[serde(from = "OtelInput")]
pub struct OtelConfig {
    // template:begin postgres:otel-postgres-application-name-doc
    /// It also supplies PostgreSQL `application_name` for pooled and migrator
    /// sessions; a distinct database session label is not a configuration axis.
    // template:end postgres:otel-postgres-application-name-doc
    // template:begin jobs:otel-jobs-worker-identity-doc
    /// The jobs worker derives its own identity from it: `{name}-jobs-worker`.
    // template:end jobs:otel-jobs-worker-identity-doc
    /// The initializer rewrites this default for a derived service.
    pub service_name: String,
    pub traces_sampler: TracesSampler,
    /// Ratio for the ratio-based samplers, in `[0, 1]`. Always validated
    /// (finite and in range) even when the selected sampler ignores it.
    pub traces_sampler_arg: f64,
    pub exporter: OtelExporterConfig,
}

impl From<OtelInput> for OtelConfig {
    fn from(input: OtelInput) -> Self {
        Self {
            service_name: input.service_name,
            traces_sampler: input.traces_sampler.0,
            traces_sampler_arg: input.traces_sampler_arg.0,
            exporter: input.exporter,
        }
    }
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self::from(OtelInput::default())
    }
}

#[serde_with::serde_as]
#[derive(Clone, derive_more::Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OtelExporterConfig {
    /// OTLP/HTTP traces endpoint. A collector root without a path resolves
    /// to `/v1/traces`. Missing, empty, or whitespace-only (after trim) is
    /// vacant and falls back to `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` then
    /// `OTEL_EXPORTER_OTLP_ENDPOINT`; when neither holds a non-blank value
    /// the exporter stays disabled when no credential is configured. Userinfo,
    /// query strings and fragments are rejected.
    #[serde(default)]
    #[serde_as(as = "crate::de::BlankAsNone")]
    #[debug(skip)]
    pub otlp_endpoint: Option<String>,
    /// Collector credential as `key=value,key=value`.
    /// Set `APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_HEADERS` in the
    /// environment or via `--secrets-dir`.
    /// Requires an HTTPS endpoint in `otlp_endpoint`. Missing, empty, or
    /// whitespace-only is absent (`None`).
    #[serde(default)]
    #[serde_as(as = "crate::de::BlankSecretAsNone")]
    pub otlp_headers: Option<SecretString>,
}

impl ObservabilityConfig {
    pub(crate) fn validate(&self, v: &mut Violations) {
        v.check(validate_service_name(&self.otel.service_name));
        v.check(sampler_arg(self.otel.traces_sampler_arg));
        if let Some(endpoint) = self.otel.exporter.otlp_endpoint.as_deref() {
            v.check(otlp_endpoint(endpoint));
        }
    }
}

pub(crate) fn validate_service_name(name: &str) -> Result<(), ValidationError> {
    non_blank("observability.otel.service_name", name)
}

fn sampler_arg(arg: f64) -> Result<(), ValidationError> {
    let key = "observability.otel.traces_sampler_arg";
    if !arg.is_finite() {
        return Err(ValidationError::new(key, "must be finite"));
    }
    if !(0.0..=1.0).contains(&arg) {
        return Err(ValidationError::new(
            key,
            format!("must be in range [0, 1], got {arg}"),
        ));
    }
    Ok(())
}

fn otlp_endpoint(endpoint: &str) -> Result<(), ValidationError> {
    let key = "observability.otel.exporter.otlp_endpoint";
    let is_http_url = endpoint.starts_with("http://") || endpoint.starts_with("https://");
    if !is_http_url {
        return Err(ValidationError::new(
            key,
            "must be an http:// or https:// URL",
        ));
    }
    if shared_rules::uri::has_whitespace_or_control(endpoint) {
        return Err(ValidationError::new(
            key,
            "must not contain whitespace or controls",
        ));
    }
    // RFC 9110 §4.2.4
    if shared_rules::uri::authority_has_userinfo(endpoint) {
        return Err(ValidationError::new(key, "must not include userinfo"));
    }
    if endpoint.contains(['?', '#', '\\']) {
        return Err(ValidationError::new(
            key,
            "must not include a query, fragment or backslash",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::validate_with;

    #[test]
    fn sampler_names_follow_opentelemetry_spelling() {
        #[serde_with::serde_as]
        #[derive(Deserialize)]
        struct Probe {
            #[serde_as(as = "crate::de::StrictEnum")]
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
        assert!(
            validate_with(|v| cfg.validate(v))
                .unwrap_err()
                .names("observability.otel.traces_sampler_arg")
        );
        cfg.otel.traces_sampler_arg = f64::NAN;
        assert!(validate_with(|v| cfg.validate(v)).is_err());
    }

    #[test]
    fn collector_credentials_and_destinations_stay_out_of_debug_output() {
        let cfg = OtelExporterConfig {
            otlp_endpoint: Some("https://collector.example/private-destination".to_owned()),
            otlp_headers: Some(SecretString::from(
                "authorization=Bearer topsecret".to_owned(),
            )),
        };
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("topsecret"), "{rendered}");
        assert!(!rendered.contains("private-destination"));
        assert!(cfg.otlp_headers.is_some());
    }

    #[test]
    fn empty_metrics_addr_disables_the_listener() {
        assert_eq!(
            ObservabilityConfig::default().metrics.addr,
            Some(SocketAddr::from(([0, 0, 0, 0], 9090)))
        );
        let blank: ObservabilityConfig = toml::from_str("[metrics]\naddr = \"\"\n").unwrap();
        assert_eq!(blank.metrics.addr, None);
        validate_with(|v| blank.validate(v)).unwrap();
        let omitted: ObservabilityConfig = toml::from_str("[metrics]\n").unwrap();
        assert_eq!(
            omitted.metrics.addr,
            Some(SocketAddr::from(([0, 0, 0, 0], 9090)))
        );
    }

    #[test]
    fn occupied_otlp_endpoint_must_be_http() {
        let mut cfg = ObservabilityConfig::default();
        cfg.otel.exporter.otlp_endpoint = Some("collector:4318".into());
        assert!(
            validate_with(|v| cfg.validate(v))
                .unwrap_err()
                .names("observability.otel.exporter.otlp_endpoint")
        );
        cfg.otel.exporter.otlp_endpoint = Some("http://collector:4318".into());
        validate_with(|v| cfg.validate(v)).unwrap();
    }

    #[test]
    fn otlp_endpoint_is_admitted_only_as_written() {
        for endpoint in [
            "http://collector:4318/v1\ttraces",
            "http://collector\u{a0}:4318",
            "http://user:secret@collector:4318",
            "https://@collector:4318",
            "https://collector/v1?private-value",
            "https://collector/v1#private-value",
        ] {
            let mut cfg = ObservabilityConfig::default();
            cfg.otel.exporter.otlp_endpoint = Some(endpoint.into());
            assert!(
                validate_with(|v| cfg.validate(v))
                    .unwrap_err()
                    .names("observability.otel.exporter.otlp_endpoint"),
                "{endpoint:?}"
            );
        }
    }
}
