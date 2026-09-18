//! OpenTelemetry tracer provider and OTLP export.
//!
//! The provider is always installed so every request has a trace id for log
//! correlation. The OTLP exporter is added only when an endpoint resolves:
//! the typed endpoint first, then the standard `OTEL_EXPORTER_OTLP_*`
//! variables, which the SDK reads itself. When the typed endpoint selects
//! the collector, ambient credential and trust variables are refused so one
//! collector's credential is never sent to another.

use std::collections::HashMap;
use std::time::Duration;

use opentelemetry::trace::TracerProvider;
use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::resource::{
    EnvResourceDetector, SdkProvidedResourceDetector, TelemetryResourceDetector,
};
use opentelemetry_sdk::trace::{self as sdktrace, SdkTracer, SdkTracerProvider};
use opentelemetry_semantic_conventions::attribute;
use secrecy::{ExposeSecret, SecretString};

/// Trace sampler selected by typed configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sampler {
    AlwaysOn,
    AlwaysOff,
    TraceIdRatio(f64),
    ParentBasedTraceIdRatio(f64),
}

#[derive(Debug)]
pub struct TracingOptions {
    pub service_name: String,
    pub service_version: String,
    pub vcs_revision: String,
    pub instance_id: String,
    pub deployment_environment: String,
    pub sampler: Sampler,
    /// Typed OTLP/HTTP traces endpoint; empty falls back to the environment.
    pub otlp_endpoint: String,
    /// Typed collector headers as `key=value,key=value`.
    pub otlp_headers: SecretString,
}

/// How the exporter ended up after startup. A startup-configuration signal,
/// not continuous delivery health.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExporterState {
    /// An OTLP exporter is attached to the provider.
    Initialized { endpoint_source: &'static str },
    /// No endpoint resolved; spans get ids but are not exported.
    Disabled,
    /// An endpoint resolved but the exporter could not be built.
    Degraded { reason: String },
}

impl ExporterState {
    /// Bounded label for logs and the startup gauge.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            ExporterState::Initialized { .. } => "initialized",
            ExporterState::Disabled => "disabled",
            ExporterState::Degraded { .. } => "degraded",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TracingError {
    #[error(
        "ambient OpenTelemetry credential or trust variable {name} is present while \
         a typed OTLP endpoint selects the collector; unset it or configure the \
         credential through the typed OTLP headers field"
    )]
    AmbientCredential { name: String },
}

/// The installed provider and its startup state.
#[derive(Debug)]
pub struct TracerProviderHandle {
    provider: SdkTracerProvider,
    pub exporter_state: ExporterState,
}

/// Ambient variables that carry credentials or trust material.
const AMBIENT_CREDENTIAL_VARS: &[&str] = &[
    "OTEL_EXPORTER_OTLP_HEADERS",
    "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
    "OTEL_EXPORTER_OTLP_CERTIFICATE",
    "OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE",
    "OTEL_EXPORTER_OTLP_CLIENT_KEY",
    "OTEL_EXPORTER_OTLP_TRACES_CLIENT_KEY",
    "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
    "OTEL_EXPORTER_OTLP_TRACES_CLIENT_CERTIFICATE",
];

const AMBIENT_ENDPOINT_VARS: &[&str] = &[
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
];

/// Join slack around `spawn_blocking` after the SDK's own shutdown timeout.
/// Not extra flush time.
const SHUTDOWN_JOIN_SLACK: Duration = Duration::from_millis(500);

/// Install the global tracer provider and the W3C propagator.
///
/// Call inside the Tokio runtime and before the subscriber is installed, so
/// the returned handle can feed the OpenTelemetry layer.
///
/// # Errors
///
/// Returns [`TracingError::AmbientCredential`] when the typed endpoint is
/// set and an ambient credential variable is present. Exporter build
/// failures do not error; they degrade and are reported in the handle.
pub fn install_tracer_provider(
    options: &TracingOptions,
) -> Result<TracerProviderHandle, TracingError> {
    let endpoint_source =
        resolve_endpoint_source(options, |name| std::env::var_os(name).is_some())?;

    let mut builder = SdkTracerProvider::builder()
        .with_resource(resource(options))
        .with_sampler(sampler(options.sampler));

    let exporter = match endpoint_source {
        None => ExporterState::Disabled,
        Some(source) => match span_exporter(options) {
            Ok(exporter) => {
                builder = builder.with_batch_exporter(exporter);
                ExporterState::Initialized {
                    endpoint_source: source,
                }
            }
            Err(err) => ExporterState::Degraded {
                reason: truncate(&err.to_string()),
            },
        },
    };

    let provider = builder.build();
    global::set_text_map_propagator(TraceContextPropagator::new());
    global::set_tracer_provider(provider.clone());
    Ok(TracerProviderHandle {
        provider,
        exporter_state: exporter,
    })
}

impl TracerProviderHandle {
    /// A tracer for the subscriber layer.
    #[must_use]
    pub fn tracer(&self, name: &str) -> SdkTracer {
        self.provider.tracer(name.to_owned())
    }

    /// Flush and stop the provider inside `budget`, off the async workers.
    ///
    /// Returns `false` when the budget expired or the SDK reported an error.
    /// [`SHUTDOWN_JOIN_SLACK`] is join time around `spawn_blocking`, not extra
    /// flush budget: the SDK already times out at `budget`.
    pub async fn shutdown(self, budget: Duration) -> bool {
        let provider = self.provider;
        let job = tokio::task::spawn_blocking(move || provider.shutdown_with_timeout(budget));
        match tokio::time::timeout(budget + SHUTDOWN_JOIN_SLACK, job).await {
            Ok(Ok(Ok(()))) => true,
            Ok(Ok(Err(err))) => {
                ::tracing::warn!(error = %err, "tracer provider shutdown reported an error");
                false
            }
            Ok(Err(join)) => {
                ::tracing::warn!(error = %join, "tracer provider shutdown task failed");
                false
            }
            Err(_elapsed) => {
                ::tracing::warn!(budget = ?budget, "tracer provider shutdown exceeded its budget");
                false
            }
        }
    }
}

/// Which source selects the endpoint, or `None` for disabled.
fn resolve_endpoint_source(
    options: &TracingOptions,
    env_present: impl Fn(&str) -> bool,
) -> Result<Option<&'static str>, TracingError> {
    if !options.otlp_endpoint.trim().is_empty() {
        if let Some(name) = AMBIENT_CREDENTIAL_VARS
            .iter()
            .find(|name| env_present(name))
        {
            return Err(TracingError::AmbientCredential {
                name: (*name).to_owned(),
            });
        }
        return Ok(Some("typed"));
    }
    Ok(AMBIENT_ENDPOINT_VARS
        .iter()
        .find(|name| env_present(name))
        .map(|_| "environment"))
}

fn span_exporter(
    options: &TracingOptions,
) -> Result<opentelemetry_otlp::SpanExporter, opentelemetry_otlp::ExporterBuildError> {
    let mut builder = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary);
    let endpoint = options.otlp_endpoint.trim();
    if !endpoint.is_empty() {
        builder = builder.with_endpoint(endpoint);
    }
    let headers = parse_headers(options.otlp_headers.expose_secret());
    if !headers.is_empty() {
        builder = builder.with_headers(headers);
    }
    builder.build()
}

/// `key=value,key=value` into a map; malformed pairs are dropped.
fn parse_headers(raw: &str) -> HashMap<String, String> {
    raw.split(',')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            let key = key.trim();
            (!key.is_empty()).then(|| (key.to_owned(), value.trim().to_owned()))
        })
        .collect()
}

/// Typed identity over the SDK detectors: detector values (including
/// `OTEL_RESOURCE_ATTRIBUTES`) fill in first, typed attributes override.
fn resource(options: &TracingOptions) -> Resource {
    let detectors: Vec<Box<dyn opentelemetry_sdk::resource::ResourceDetector>> = vec![
        Box::new(SdkProvidedResourceDetector),
        Box::new(TelemetryResourceDetector),
        Box::new(EnvResourceDetector::new()),
    ];
    let mut attributes = vec![
        KeyValue::new(attribute::SERVICE_NAME, options.service_name.clone()),
        KeyValue::new(attribute::SERVICE_VERSION, options.service_version.clone()),
        KeyValue::new(
            attribute::VCS_REF_HEAD_REVISION,
            options.vcs_revision.clone(),
        ),
        KeyValue::new(
            attribute::DEPLOYMENT_ENVIRONMENT_NAME,
            options.deployment_environment.clone(),
        ),
    ];
    if !options.instance_id.trim().is_empty() {
        attributes.push(KeyValue::new(
            attribute::SERVICE_INSTANCE_ID,
            options.instance_id.clone(),
        ));
    }
    Resource::builder_empty()
        .with_detectors(&detectors)
        .with_attributes(attributes)
        .build()
}

fn sampler(sampler: Sampler) -> sdktrace::Sampler {
    match sampler {
        Sampler::AlwaysOn => sdktrace::Sampler::AlwaysOn,
        Sampler::AlwaysOff => sdktrace::Sampler::AlwaysOff,
        Sampler::TraceIdRatio(ratio) => sdktrace::Sampler::TraceIdRatioBased(ratio),
        Sampler::ParentBasedTraceIdRatio(ratio) => {
            sdktrace::Sampler::ParentBased(Box::new(sdktrace::Sampler::TraceIdRatioBased(ratio)))
        }
    }
}

/// Exporter errors can echo the endpoint, never a header; keep them short.
fn truncate(message: &str) -> String {
    message.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(endpoint: &str) -> TracingOptions {
        TracingOptions {
            service_name: "svc".into(),
            service_version: "1.0.0".into(),
            vcs_revision: "abc".into(),
            instance_id: String::new(),
            deployment_environment: "test".into(),
            sampler: Sampler::AlwaysOn,
            otlp_endpoint: endpoint.into(),
            otlp_headers: SecretString::from(String::new()),
        }
    }

    #[test]
    fn typed_endpoint_wins_and_refuses_ambient_credentials() {
        let typed = options("http://collector:4318");
        assert_eq!(
            resolve_endpoint_source(&typed, |_| false).unwrap(),
            Some("typed")
        );
        let err = resolve_endpoint_source(&typed, |name| name == "OTEL_EXPORTER_OTLP_HEADERS")
            .unwrap_err();
        assert!(
            matches!(err, TracingError::AmbientCredential { ref name } if name == "OTEL_EXPORTER_OTLP_HEADERS")
        );
        // Ambient endpoint variables are fine beside a typed endpoint; the
        // typed one wins inside the SDK.
        assert_eq!(
            resolve_endpoint_source(&typed, |name| name == "OTEL_EXPORTER_OTLP_ENDPOINT").unwrap(),
            Some("typed")
        );
    }

    #[test]
    fn environment_endpoint_enables_export_and_nothing_disables_it() {
        let untyped = options("");
        assert_eq!(resolve_endpoint_source(&untyped, |_| false).unwrap(), None);
        assert_eq!(
            resolve_endpoint_source(&untyped, |name| name == "OTEL_EXPORTER_OTLP_ENDPOINT")
                .unwrap(),
            Some("environment")
        );
        // Ambient credentials are the platform's business when the platform
        // also supplies the endpoint.
        assert_eq!(
            resolve_endpoint_source(&untyped, |name| name.starts_with("OTEL_EXPORTER_OTLP_"))
                .unwrap(),
            Some("environment")
        );
    }

    #[test]
    fn header_pairs_parse_and_skip_garbage() {
        let headers = parse_headers("authorization=Bearer x, x-tenant = t1 ,broken,=novalue");
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Bearer x")
        );
        assert_eq!(headers.get("x-tenant").map(String::as_str), Some("t1"));
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn resource_carries_typed_identity() {
        let resource = resource(&options(""));
        let get = |key: &str| {
            resource
                .get(&opentelemetry::Key::new(key.to_owned()))
                .map(|v| v.to_string())
        };
        assert_eq!(get(attribute::SERVICE_NAME).as_deref(), Some("svc"));
        assert_eq!(get(attribute::SERVICE_VERSION).as_deref(), Some("1.0.0"));
        assert_eq!(
            get(attribute::DEPLOYMENT_ENVIRONMENT_NAME).as_deref(),
            Some("test")
        );
        assert_eq!(
            get(attribute::VCS_REF_HEAD_REVISION).as_deref(),
            Some("abc")
        );
        assert!(get("telemetry.sdk.language").is_some());
    }
}
