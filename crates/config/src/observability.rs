use std::net::SocketAddr;

use secrecy::SecretString;
use serde::Deserialize;

use crate::de::{
    BlankAsNone, BlankAsNoneVerbatim, Keep, ListenAddr, Mode, ModeMarker, StrictRatio, When,
};
use crate::validate::{ValidationError, Violations, non_blank};

const DEFAULT_SERVICE_NAME: &str = "service";
const DEFAULT_TRACES_SAMPLER_ARG: f64 = 0.10;

#[serde_with::serde_as]
#[derive(Debug, Deserialize, educe::Educe)]
#[educe(Default(bound = false))]
#[serde(bound(deserialize = ""), deny_unknown_fields, default)]
pub(crate) struct ObservabilityFields<Full: Mode = Keep> {
    #[serde_as(as = "When<Full>")]
    metrics: MetricsConfig,
    pub(crate) otel: OtelFields<Full>,
    #[serde(skip)]
    _mode: ModeMarker<Full>,
}

pub(crate) type ObservabilityInput = ObservabilityFields;

#[derive(Clone, Debug)]
pub struct ObservabilityConfig {
    pub metrics: MetricsConfig,
    pub otel: OtelConfig,
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
    #[serde_as(as = "BlankAsNone<ListenAddr>")]
    #[educe(Default(expression = Some(SocketAddr::from(([0, 0, 0, 0], 9090)))))]
    pub addr: Option<SocketAddr>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TracesSamplerKind {
    AlwaysOn,
    AlwaysOff,
    #[serde(rename = "traceidratio")]
    TraceIdRatio,
    #[default]
    #[serde(rename = "parentbased_traceidratio")]
    ParentBasedTraceIdRatio,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TracesSampler {
    AlwaysOn,
    AlwaysOff,
    TraceIdRatio(f64),
    ParentBasedTraceIdRatio(f64),
}

#[serde_with::serde_as]
#[derive(Debug, Deserialize, educe::Educe)]
#[educe(Default(bound = false))]
#[serde(bound(deserialize = ""), deny_unknown_fields, default)]
pub(crate) struct OtelFields<Full: Mode = Keep> {
    #[educe(Default(expression = DEFAULT_SERVICE_NAME.to_owned()))]
    pub(crate) service_name: String,
    #[serde_as(as = "When<Full>")]
    traces_sampler: TracesSamplerKind,
    /// Ratio for the ratio-based samplers, in `[0, 1]`. Always validated
    /// (finite and in range) even when the selected sampler ignores it.
    #[serde_as(as = "When<Full, StrictRatio>")]
    #[educe(Default(expression = DEFAULT_TRACES_SAMPLER_ARG))]
    traces_sampler_arg: f64,
    #[serde_as(as = "When<Full>")]
    exporter: OtelExporterConfig,
    #[serde(skip)]
    _mode: ModeMarker<Full>,
}

#[derive(Clone, Debug)]
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
    pub exporter: OtelExporterConfig,
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
    #[serde_as(as = "BlankAsNone")]
    #[debug(skip)]
    pub otlp_endpoint: Option<String>,
    /// Collector credential as `key=value,key=value`.
    /// Set `APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_HEADERS` in the
    /// environment or via `--secrets-dir`.
    /// Requires an HTTPS endpoint in `otlp_endpoint`. Missing, empty, or
    /// whitespace-only is absent (`None`).
    #[serde_as(as = "BlankAsNoneVerbatim")]
    pub otlp_headers: Option<SecretString>,
}

impl ObservabilityInput {
    pub(crate) fn admit(self, v: &mut Violations) -> ObservabilityConfig {
        v.check(validate_service_name(&self.otel.service_name));
        v.check(sampler_arg(self.otel.traces_sampler_arg));
        if let Some(endpoint) = self.otel.exporter.otlp_endpoint.as_deref() {
            v.check(otlp_endpoint(endpoint));
        }
        let ratio = self.otel.traces_sampler_arg;
        let traces_sampler = match self.otel.traces_sampler {
            TracesSamplerKind::AlwaysOn => TracesSampler::AlwaysOn,
            TracesSamplerKind::AlwaysOff => TracesSampler::AlwaysOff,
            TracesSamplerKind::TraceIdRatio => TracesSampler::TraceIdRatio(ratio),
            TracesSamplerKind::ParentBasedTraceIdRatio => {
                TracesSampler::ParentBasedTraceIdRatio(ratio)
            }
        };
        ObservabilityConfig {
            metrics: self.metrics,
            otel: OtelConfig {
                service_name: self.otel.service_name,
                traces_sampler,
                exporter: self.otel.exporter,
            },
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
    use crate::ValidationErrors;
    use crate::validate::admit_with;

    fn admit(input: ObservabilityInput) -> Result<ObservabilityConfig, ValidationErrors> {
        admit_with(|v| Ok(input.admit(v)))
    }

    #[test]
    fn sampler_names_follow_opentelemetry_spelling() {
        #[derive(Deserialize)]
        struct Probe {
            sampler: TracesSamplerKind,
        }
        for (text, expected) in [
            ("always_on", TracesSamplerKind::AlwaysOn),
            ("always_off", TracesSamplerKind::AlwaysOff),
            ("traceidratio", TracesSamplerKind::TraceIdRatio),
            (
                "parentbased_traceidratio",
                TracesSamplerKind::ParentBasedTraceIdRatio,
            ),
        ] {
            let probe: Probe = toml::from_str(&format!("sampler = \"{text}\"")).unwrap();
            assert_eq!(probe.sampler, expected, "{text}");
        }
        assert!(toml::from_str::<Probe>("sampler = \"jaeger_remote\"").is_err());
    }

    #[test]
    fn sampler_arg_bounds_and_variant_attachment() {
        let mut input = ObservabilityInput::default();
        input.otel.traces_sampler = TracesSamplerKind::AlwaysOn;
        input.otel.traces_sampler_arg = 1.5;
        assert!(
            admit(input)
                .unwrap_err()
                .names("observability.otel.traces_sampler_arg")
        );

        let mut nan = ObservabilityInput::default();
        nan.otel.traces_sampler_arg = f64::NAN;
        assert!(admit(nan).is_err());

        let always_on: ObservabilityInput =
            toml::from_str("[otel]\ntraces_sampler = \"always_on\"\ntraces_sampler_arg = 0.5\n")
                .unwrap();
        assert_eq!(
            admit(always_on).unwrap().otel.traces_sampler,
            TracesSampler::AlwaysOn
        );

        let ratio: ObservabilityInput =
            toml::from_str("[otel]\ntraces_sampler = \"traceidratio\"\ntraces_sampler_arg = 0.5\n")
                .unwrap();
        assert_eq!(
            admit(ratio).unwrap().otel.traces_sampler,
            TracesSampler::TraceIdRatio(0.5)
        );
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
            admit(ObservabilityInput::default()).unwrap().metrics.addr,
            Some(SocketAddr::from(([0, 0, 0, 0], 9090)))
        );
        let blank: ObservabilityInput = toml::from_str("[metrics]\naddr = \"\"\n").unwrap();
        assert_eq!(admit(blank).unwrap().metrics.addr, None);
        let omitted: ObservabilityInput = toml::from_str("[metrics]\n").unwrap();
        assert_eq!(
            admit(omitted).unwrap().metrics.addr,
            Some(SocketAddr::from(([0, 0, 0, 0], 9090)))
        );
    }

    #[test]
    fn occupied_otlp_endpoint_must_be_http() {
        let mut input = ObservabilityInput::default();
        input.otel.exporter.otlp_endpoint = Some("collector:4318".into());
        assert!(
            admit(input)
                .unwrap_err()
                .names("observability.otel.exporter.otlp_endpoint")
        );
        let mut valid = ObservabilityInput::default();
        valid.otel.exporter.otlp_endpoint = Some("http://collector:4318".into());
        admit(valid).unwrap();
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
            let mut input = ObservabilityInput::default();
            input.otel.exporter.otlp_endpoint = Some(endpoint.into());
            assert!(
                admit(input)
                    .unwrap_err()
                    .names("observability.otel.exporter.otlp_endpoint"),
                "{endpoint:?}"
            );
        }
    }
}
