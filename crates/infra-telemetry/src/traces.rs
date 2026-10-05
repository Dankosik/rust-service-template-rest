//! OpenTelemetry tracer provider and OTLP export.
//!
//! The provider is always installed so every request has a trace id for log
//! correlation. The OTLP exporter is added only when an endpoint resolves:
//! the typed endpoint first, then the standard `OTEL_EXPORTER_OTLP_*`
//! endpoint variables, which the SDK reads itself. This template always
//! builds OTLP/HTTP protobuf and does not honor `OTEL_EXPORTER_OTLP_PROTOCOL`.
//! When the typed endpoint selects the collector, ambient header variables
//! are refused because the SDK would merge them into the typed request, so
//! one collector's credential is never sent to another.
//! `OTEL_EXPORTER_OTLP_COMPRESSION` and its traces variant select `gzip`;
//! the default is uncompressed, as the specification has it.
//! The collector is verified with the platform trust store unless
//! `OTEL_EXPORTER_OTLP_CERTIFICATE` names a PEM file of trusted
//! certificates, which then are the only ones trusted;
//! `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE` with
//! `OTEL_EXPORTER_OTLP_CLIENT_KEY` presents a client certificate. Each has
//! a traces variant that wins. The SDK's OTLP/HTTP exporter reads none of
//! them, so this module builds the exporter's HTTP client when one is set.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use axum::http::{HeaderName, HeaderValue, Uri};
use opentelemetry::trace::TracerProvider;
use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{
    OTEL_EXPORTER_OTLP_TIMEOUT, OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT,
    OTEL_EXPORTER_OTLP_TRACES_TIMEOUT, WithExportConfig, WithHttpConfig,
};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{
    self as sdktrace, SdkTracer, SdkTracerProvider, SpanData, SpanExporter,
};
use opentelemetry_semantic_conventions::attribute;
use secrecy::{ExposeSecret, SecretString};

use crate::metrics::TRACE_SPANS_EXPORTED_METRIC;

/// Trace sampler selected by typed configuration, with the ratio already
/// attached to the variants that use it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResolvedSampler {
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
    pub sampler: ResolvedSampler,
    /// Typed OTLP/HTTP endpoint; a URL without a path gets `/v1/traces`.
    /// `None` falls back to the environment.
    pub otlp_endpoint: Option<String>,
    /// Typed collector headers as `key=value,key=value`. `None` means no
    /// extra headers.
    pub otlp_headers: Option<SecretString>,
}

/// Which configuration selected the OTLP endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointSource {
    /// `observability.otel.exporter.otlp_endpoint` was occupied.
    Typed,
    /// An `OTEL_EXPORTER_OTLP_*ENDPOINT` variable was occupied.
    Environment,
}

impl EndpointSource {
    /// Bounded label for the startup log field.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Typed => "typed",
            Self::Environment => "environment",
        }
    }
}

/// How the exporter ended up after startup. A startup-configuration signal,
/// not continuous delivery health.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExporterState {
    /// An OTLP exporter is attached to the provider.
    Initialized {
        endpoint_source: EndpointSource,
        /// The collector is verified against the certificate file of a
        /// standard variable, not the platform trust store.
        certificate_file: bool,
        /// A client certificate from the standard variables is presented.
        client_certificate: bool,
    },
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

    /// Log the startup state once the subscriber is installed.
    pub fn log(&self) {
        match self {
            ExporterState::Initialized {
                endpoint_source,
                certificate_file,
                client_certificate,
            } => tracing::info!(
                endpoint_source = endpoint_source.as_str(),
                certificate_file,
                client_certificate,
                "trace exporter initialized"
            ),
            ExporterState::Disabled => {}
            ExporterState::Degraded { reason } => tracing::warn!(
                reason = %reason,
                "trace exporter degraded; spans are recorded but not exported"
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TracingError {
    #[error(
        "ambient OpenTelemetry header variable {name} is present while a typed \
         OTLP endpoint selects the collector; unset it or configure the \
         credential through the typed OTLP headers field"
    )]
    AmbientCredential { name: &'static str },
    #[error(
        "observability.otel.exporter.otlp_headers entry {position} is not a \
         valid `name=value` HTTP header"
    )]
    InvalidHeader { position: usize },
}

/// The installed provider and its startup state.
#[derive(Debug)]
pub struct TracerProviderHandle {
    provider: SdkTracerProvider,
    pub exporter_state: ExporterState,
}

/// Header variables the SDK merges over typed headers.
const AMBIENT_HEADER_VARS: &[&str] = &[
    "OTEL_EXPORTER_OTLP_HEADERS",
    "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
];

/// Standard trust variables, each a path to a PEM file; the traces variant
/// wins.
const CERTIFICATE_VARS: [&str; 2] = [
    "OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE",
    "OTEL_EXPORTER_OTLP_CERTIFICATE",
];
const CLIENT_CERTIFICATE_VARS: [&str; 2] = [
    "OTEL_EXPORTER_OTLP_TRACES_CLIENT_CERTIFICATE",
    "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
];
const CLIENT_KEY_VARS: [&str; 2] = [
    "OTEL_EXPORTER_OTLP_TRACES_CLIENT_KEY",
    "OTEL_EXPORTER_OTLP_CLIENT_KEY",
];

const AMBIENT_ENDPOINT_VARS: &[&str] = &[
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
];

/// Instrumentation scope of the `tracing` bridge; the service itself is the
/// `service.name` resource attribute.
const INSTRUMENTATION_SCOPE: &str = env!("CARGO_PKG_NAME");

/// Join slack around `spawn_blocking` after the SDK's own shutdown timeout.
/// Not extra flush time.
pub const SHUTDOWN_JOIN_SLACK: Duration = Duration::from_millis(500);

/// Install the global tracer provider and the W3C propagator.
///
/// Call before the subscriber is installed so the returned handle can feed
/// the OpenTelemetry layer. The global tracer provider keeps a clone of the
/// SDK provider.
///
/// # Errors
///
/// Returns [`TracingError::AmbientCredential`] when the typed endpoint is set
/// and an ambient header variable is occupied, and
/// [`TracingError::InvalidHeader`] for a malformed typed header. Exporter
/// build failures do not error; they degrade and are reported in the handle.
pub fn install_tracer_provider(
    options: &TracingOptions,
) -> Result<TracerProviderHandle, TracingError> {
    let trust = CollectorTrust::from_env(|name| std::env::var_os(name));
    let handle = tracer_provider(options, occupied_env, &trust)?;
    global::set_text_map_propagator(TraceContextPropagator::new());
    global::set_tracer_provider(handle.provider.clone());
    Ok(handle)
}

/// The provider and its startup state, before anything global is set.
fn tracer_provider(
    options: &TracingOptions,
    occupied: impl Fn(&str) -> bool,
    trust: &CollectorTrust,
) -> Result<TracerProviderHandle, TracingError> {
    let endpoint_source = resolve_endpoint_source(options, occupied)?;
    let headers = match options.otlp_headers.as_ref() {
        Some(raw) => parse_headers(raw.expose_secret())?,
        None => HashMap::new(),
    };

    let mut builder = SdkTracerProvider::builder()
        .with_resource(resource(options))
        .with_sampler(options.sampler.to_sdk());

    let exporter_state = match endpoint_source {
        None => ExporterState::Disabled,
        Some(source) => match span_exporter(options.otlp_endpoint.as_deref(), headers, trust) {
            Ok(span_exporter) => {
                builder = builder.with_batch_exporter(Counted(span_exporter));
                ExporterState::Initialized {
                    endpoint_source: source,
                    certificate_file: trust.certificate.is_some(),
                    client_certificate: trust.client_certificate.is_some(),
                }
            }
            Err(err) => ExporterState::Degraded {
                reason: truncate(&err.to_string()),
            },
        },
    };

    Ok(TracerProviderHandle {
        provider: builder.build(),
        exporter_state,
    })
}

impl TracerProviderHandle {
    /// A tracer for the subscriber layer.
    #[must_use]
    pub(crate) fn tracer(&self) -> SdkTracer {
        self.provider.tracer(INSTRUMENTATION_SCOPE)
    }

    /// Flush and stop the provider inside `budget`, off the async workers.
    ///
    /// The global provider shares this provider's state, so shutting down
    /// here also stops the global clone. If the join exceeds `budget` plus
    /// [`SHUTDOWN_JOIN_SLACK`], the blocking job is detached and reported as
    /// [`ProviderShutdown::Incomplete`].
    pub async fn shutdown(self, budget: Duration) -> ProviderShutdown {
        self.shutdown_until(
            budget,
            tokio::time::Instant::now() + budget + SHUTDOWN_JOIN_SLACK,
        )
        .await
    }

    /// Attempt SDK shutdown off the async workers, and observe completion
    /// only until `deadline`. The SDK receives at most `sdk_budget`, with
    /// [`SHUTDOWN_JOIN_SLACK`] reserved inside the remaining time.
    ///
    /// Even an expired deadline submits an explicit zero-budget shutdown.
    /// A timed-out blocking job may continue; its flush is unconfirmed.
    pub async fn shutdown_until(
        self,
        sdk_budget: Duration,
        deadline: tokio::time::Instant,
    ) -> ProviderShutdown {
        let budget = sdk_budget.min(
            deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .saturating_sub(SHUTDOWN_JOIN_SLACK),
        );
        let provider = self.provider;
        let job = tokio::task::spawn_blocking(move || provider.shutdown_with_timeout(budget));
        match tokio::time::timeout_at(deadline, job).await {
            Ok(Ok(Ok(()))) => ProviderShutdown::Flushed,
            Ok(Ok(Err(err))) => {
                tracing::warn!(error = %err, "tracer provider shutdown reported an error");
                ProviderShutdown::Incomplete
            }
            Ok(Err(join)) => {
                tracing::warn!(error = %join, "tracer provider shutdown task failed");
                ProviderShutdown::Incomplete
            }
            Err(_elapsed) => {
                tracing::warn!(budget = ?budget, "tracer provider shutdown exceeded its budget");
                ProviderShutdown::Incomplete
            }
        }
    }
}

/// Outcome of [`TracerProviderHandle::shutdown`].
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderShutdown {
    /// The SDK reported a clean flush.
    Flushed,
    /// Timeout, SDK error, or join failure: not confirmed flushed.
    Incomplete,
}

/// Whether an environment variable holds a non-blank value. The SDK ignores
/// a blank endpoint and falls back to `localhost:4318`, so blank is vacant.
fn occupied_env(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.to_string_lossy().trim().is_empty())
}

/// Which source selects the endpoint, or `None` for disabled.
fn resolve_endpoint_source(
    options: &TracingOptions,
    occupied: impl Fn(&str) -> bool,
) -> Result<Option<EndpointSource>, TracingError> {
    if options
        .otlp_endpoint
        .as_deref()
        .is_some_and(|endpoint| !endpoint.trim().is_empty())
    {
        if let Some(&name) = AMBIENT_HEADER_VARS.iter().find(|name| occupied(name)) {
            return Err(TracingError::AmbientCredential { name });
        }
        return Ok(Some(EndpointSource::Typed));
    }
    Ok(AMBIENT_ENDPOINT_VARS
        .iter()
        .any(|name| occupied(name))
        .then_some(EndpointSource::Environment))
}

/// A PEM file named by a standard trust variable.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TrustFile {
    variable: &'static str,
    path: PathBuf,
}

impl TrustFile {
    fn read(&self) -> Result<Vec<u8>, ExporterError> {
        std::fs::read(&self.path).map_err(|source| ExporterError::Read {
            variable: self.variable,
            source,
        })
    }

    fn unusable(&self, source: reqwest::Error) -> ExporterError {
        ExporterError::Pem {
            variable: self.variable,
            source,
        }
    }
}

/// The collector's trust material from the standard variables. All vacant
/// is the platform trust store and no client certificate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CollectorTrust {
    certificate: Option<TrustFile>,
    client_certificate: Option<TrustFile>,
    client_key: Option<TrustFile>,
}

impl CollectorTrust {
    /// A blank value is vacant, like a blank endpoint.
    fn from_env(var: impl Fn(&str) -> Option<OsString>) -> Self {
        let file = |variables: [&'static str; 2]| {
            variables.into_iter().find_map(|variable| {
                let path = var(variable)?;
                (!path.to_string_lossy().trim().is_empty()).then(|| TrustFile {
                    variable,
                    path: path.into(),
                })
            })
        };
        Self {
            certificate: file(CERTIFICATE_VARS),
            client_certificate: file(CLIENT_CERTIFICATE_VARS),
            client_key: file(CLIENT_KEY_VARS),
        }
    }
}

/// Why an exporter could not be built; the bounded text becomes
/// [`ExporterState::Degraded`]. Names the variable, never file content.
#[derive(Debug, thiserror::Error)]
enum ExporterError {
    #[error(transparent)]
    Build(#[from] opentelemetry_otlp::ExporterBuildError),
    #[error("read the file {variable} names: {source}")]
    Read {
        variable: &'static str,
        source: std::io::Error,
    },
    #[error("the file {variable} names is not usable PEM: {source}")]
    Pem {
        variable: &'static str,
        source: reqwest::Error,
    },
    #[error("the file {variable} names holds no certificate")]
    NoCertificate { variable: &'static str },
    #[error("the client certificate and key files are not a usable PEM identity: {0}")]
    Identity(#[source] reqwest::Error),
    #[error("{present} is set without {missing}; a client certificate needs both")]
    IncompleteIdentity {
        present: &'static str,
        missing: &'static str,
    },
    #[error("build the collector HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("start the thread that builds the collector HTTP client: {0}")]
    ClientThread(#[source] std::io::Error),
    #[error("the thread that builds the collector HTTP client panicked")]
    ClientThreadPanicked,
}

fn span_exporter(
    endpoint: Option<&str>,
    headers: HashMap<String, String>,
    trust: &CollectorTrust,
) -> Result<opentelemetry_otlp::SpanExporter, ExporterError> {
    // Always OTLP/HTTP protobuf. An environment endpoint still lets the SDK
    // pick the URL; it does not select gRPC or another protocol.
    let mut builder = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary);
    if let Some(endpoint) = endpoint {
        builder = builder.with_endpoint(traces_url(endpoint));
    }
    if !headers.is_empty() {
        builder = builder.with_headers(headers);
    }
    if let Some(client) = collector_client(trust)? {
        builder = builder.with_http_client(client);
    }
    Ok(builder.build()?)
}

/// The exporter's HTTP client when a trust variable is set; `None` leaves
/// the SDK's own client, which verifies with the platform trust store.
fn collector_client(
    trust: &CollectorTrust,
) -> Result<Option<reqwest::blocking::Client>, ExporterError> {
    if *trust == CollectorTrust::default() {
        return Ok(None);
    }
    // The blocking client starts its own runtime thread and waits for it,
    // which an async worker must not do; the SDK builds its client the same
    // way.
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .spawn_scoped(scope, || trusting_client(trust))
            .map_err(ExporterError::ClientThread)?
            .join()
            .map_err(|_| ExporterError::ClientThreadPanicked)?
    })
    .map(Some)
}

fn trusting_client(trust: &CollectorTrust) -> Result<reqwest::blocking::Client, ExporterError> {
    // The SDK sets its timeout on the client it builds, so a supplied
    // client carries the same one.
    let mut builder = reqwest::blocking::Client::builder().timeout(export_timeout());
    if let Some(file) = &trust.certificate {
        let certificates = reqwest::Certificate::from_pem_bundle(&file.read()?)
            .map_err(|source| file.unusable(source))?;
        if certificates.is_empty() {
            return Err(ExporterError::NoCertificate {
                variable: file.variable,
            });
        }
        builder = builder.tls_certs_only(certificates);
    }
    match (&trust.client_certificate, &trust.client_key) {
        (Some(certificate), Some(key)) => {
            let mut pem = key.read()?;
            pem.push(b'\n');
            pem.extend(certificate.read()?);
            let identity = reqwest::Identity::from_pem(&pem).map_err(ExporterError::Identity)?;
            builder = builder.identity(identity);
        }
        (Some(present), None) => {
            return Err(ExporterError::IncompleteIdentity {
                present: present.variable,
                missing: CLIENT_KEY_VARS[1],
            });
        }
        (None, Some(present)) => {
            return Err(ExporterError::IncompleteIdentity {
                present: present.variable,
                missing: CLIENT_CERTIFICATE_VARS[1],
            });
        }
        (None, None) => {}
    }
    builder.build().map_err(ExporterError::Client)
}

/// The export timeout as the SDK resolves it: the traces variable, then the
/// general one, in milliseconds, then ten seconds.
fn export_timeout() -> Duration {
    [
        OTEL_EXPORTER_OTLP_TRACES_TIMEOUT,
        OTEL_EXPORTER_OTLP_TIMEOUT,
    ]
    .into_iter()
    .find_map(|name| std::env::var(name).ok()?.parse().ok())
    .map_or(OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT, Duration::from_millis)
}

/// Counts the spans of every finished export under
/// [`TRACE_SPANS_EXPORTED_METRIC`], so delivery to the collector is a
/// metric and not only an SDK log line.
#[derive(Debug)]
struct Counted<E>(E);

impl<E: SpanExporter> SpanExporter for Counted<E> {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        let spans = u64::try_from(batch.len()).unwrap_or(u64::MAX);
        let result = self.0.export(batch).await;
        match &result {
            Ok(()) => metrics::counter!(TRACE_SPANS_EXPORTED_METRIC).increment(spans),
            Err(err) => {
                let error_type = match err {
                    OTelSdkError::Timeout(_) => "timeout",
                    OTelSdkError::AlreadyShutdown => "already_shutdown",
                    OTelSdkError::InternalFailure(_) => "internal_failure",
                };
                metrics::counter!(TRACE_SPANS_EXPORTED_METRIC, "error_type" => error_type)
                    .increment(spans);
            }
        }
        result
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.0.set_resource(resource);
    }
}

/// The SDK uses a programmatic endpoint verbatim. Apply the
/// `OTEL_EXPORTER_OTLP_ENDPOINT` rule to a collector root so
/// `http://collector:4318` posts to `/v1/traces`, not `/`.
fn traces_url(endpoint: &str) -> String {
    match endpoint.parse::<Uri>() {
        Ok(uri) if uri.path() == "/" && uri.query().is_none() => {
            format!("{}/v1/traces", endpoint.trim_end_matches('/'))
        }
        _ => endpoint.to_owned(),
    }
}

/// `key=value,key=value` into a map. Blank entries are skipped; any other
/// entry must be a valid header name and value, so a typo fails startup
/// instead of silently dropping a credential. The SDK percent-decodes
/// values. Errors name the entry position, never its content.
fn parse_headers(raw: &str) -> Result<HashMap<String, String>, TracingError> {
    let mut headers = HashMap::new();
    for (index, entry) in raw.split(',').enumerate() {
        if entry.trim().is_empty() {
            continue;
        }
        let invalid = TracingError::InvalidHeader {
            position: index + 1,
        };
        let Some((name, value)) = entry.split_once('=') else {
            return Err(invalid);
        };
        let (name, value) = (name.trim(), value.trim());
        if HeaderName::from_bytes(name.as_bytes()).is_err() || HeaderValue::from_str(value).is_err()
        {
            return Err(invalid);
        }
        headers.insert(name.to_owned(), value.to_owned());
    }
    Ok(headers)
}

/// Typed identity over the SDK detectors: detector values (including
/// `OTEL_RESOURCE_ATTRIBUTES`) fill in first, typed attributes override.
fn resource(options: &TracingOptions) -> Resource {
    let attributes = [
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
    let instance = (!options.instance_id.trim().is_empty())
        .then(|| KeyValue::new(attribute::SERVICE_INSTANCE_ID, options.instance_id.clone()));
    Resource::builder()
        .with_attributes(attributes.into_iter().chain(instance))
        .build()
}

impl ResolvedSampler {
    fn to_sdk(self) -> sdktrace::Sampler {
        match self {
            Self::AlwaysOn => sdktrace::Sampler::AlwaysOn,
            Self::AlwaysOff => sdktrace::Sampler::AlwaysOff,
            Self::TraceIdRatio(ratio) => sdktrace::Sampler::TraceIdRatioBased(ratio),
            Self::ParentBasedTraceIdRatio(ratio) => sdktrace::Sampler::ParentBased(Box::new(
                sdktrace::Sampler::TraceIdRatioBased(ratio),
            )),
        }
    }
}

/// Bound the degraded reason kept for the startup log and warning.
fn truncate(message: &str) -> String {
    let end = message
        .char_indices()
        .nth(200)
        .map_or(message.len(), |(end, _)| end);
    message[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    mod delivery;

    #[derive(Debug)]
    struct ShutdownExporter {
        attempted: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Duration>>>,
        release: Option<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
        finished: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl SpanExporter for ShutdownExporter {
        async fn export(&self, _batch: Vec<SpanData>) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
            self.attempted
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(timeout)
                .unwrap();
            if let Some(release) = &self.release {
                release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
            }
            Ok(())
        }
    }

    impl Drop for ShutdownExporter {
        fn drop(&mut self) {
            if let Some(finished) = self.finished.take() {
                let _ = finished.send(());
            }
        }
    }

    #[tokio::test]
    async fn shutdown_until_reserves_join_slack_inside_the_absolute_deadline() {
        for (sdk_budget, maximum) in [
            (Duration::from_secs(5), Duration::from_millis(500)),
            (Duration::from_millis(20), Duration::from_millis(20)),
        ] {
            let (attempted, budget) = tokio::sync::oneshot::channel();
            let handle = TracerProviderHandle {
                provider: SdkTracerProvider::builder()
                    .with_simple_exporter(ShutdownExporter {
                        attempted: std::sync::Mutex::new(Some(attempted)),
                        release: None,
                        finished: None,
                    })
                    .build(),
                exporter_state: ExporterState::Disabled,
            };
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            assert_eq!(
                handle.shutdown_until(sdk_budget, deadline).await,
                ProviderShutdown::Flushed
            );
            let budget = budget.await.unwrap();
            assert!(budget > Duration::ZERO);
            assert!(budget <= maximum);
        }
    }

    #[tokio::test]
    async fn expired_or_short_shutdown_still_attempts_sdk_and_reports_unfinished_work() {
        for remaining in [Duration::ZERO, Duration::from_millis(20)] {
            let (attempted, budget) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let (finished, joined) = tokio::sync::oneshot::channel();
            let handle = TracerProviderHandle {
                provider: SdkTracerProvider::builder()
                    .with_simple_exporter(ShutdownExporter {
                        attempted: std::sync::Mutex::new(Some(attempted)),
                        release: Some(std::sync::Mutex::new(wait)),
                        finished: Some(finished),
                    })
                    .build(),
                exporter_state: ExporterState::Disabled,
            };
            let deadline = tokio::time::Instant::now() + remaining;
            assert_eq!(
                handle
                    .shutdown_until(Duration::from_secs(5), deadline)
                    .await,
                ProviderShutdown::Incomplete
            );
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), budget)
                    .await
                    .unwrap()
                    .unwrap(),
                Duration::ZERO
            );
            release.send(()).unwrap();
            // Last-provider drop follows the SDK shutdown call, so the
            // fixture cannot leave its blocking exporter running.
            tokio::time::timeout(Duration::from_secs(2), joined)
                .await
                .unwrap()
                .unwrap();
        }
    }

    fn options(endpoint: &str) -> TracingOptions {
        TracingOptions {
            service_name: "svc".into(),
            service_version: "1.0.0".into(),
            vcs_revision: "abc".into(),
            instance_id: String::new(),
            deployment_environment: "test".into(),
            sampler: ResolvedSampler::AlwaysOn,
            otlp_endpoint: (!endpoint.is_empty()).then(|| endpoint.to_owned()),
            otlp_headers: None,
        }
    }

    #[test]
    fn typed_endpoint_wins_and_refuses_ambient_credentials() {
        let typed = options("http://collector:4318");
        assert_eq!(
            resolve_endpoint_source(&typed, |_| false).unwrap(),
            Some(EndpointSource::Typed)
        );
        let err = resolve_endpoint_source(&typed, |name| name == "OTEL_EXPORTER_OTLP_HEADERS")
            .unwrap_err();
        assert!(matches!(
            err,
            TracingError::AmbientCredential {
                name: "OTEL_EXPORTER_OTLP_HEADERS"
            }
        ));
        // Trust variables are not read by the exporter, so they cannot carry
        // a credential to the typed collector.
        assert_eq!(
            resolve_endpoint_source(&typed, |name| name == "OTEL_EXPORTER_OTLP_CERTIFICATE")
                .unwrap(),
            Some(EndpointSource::Typed)
        );
        // Ambient endpoint variables are fine beside a typed endpoint; the
        // typed one wins inside the SDK.
        assert_eq!(
            resolve_endpoint_source(&typed, |name| name == "OTEL_EXPORTER_OTLP_ENDPOINT").unwrap(),
            Some(EndpointSource::Typed)
        );
    }

    #[test]
    fn environment_endpoint_enables_export_and_nothing_disables_it() {
        let untyped = options("");
        assert_eq!(resolve_endpoint_source(&untyped, |_| false).unwrap(), None);
        assert_eq!(
            resolve_endpoint_source(&untyped, |name| name == "OTEL_EXPORTER_OTLP_ENDPOINT")
                .unwrap(),
            Some(EndpointSource::Environment)
        );
        // Ambient credentials are the platform's business when the platform
        // also supplies the endpoint.
        assert_eq!(
            resolve_endpoint_source(&untyped, |name| name.starts_with("OTEL_EXPORTER_OTLP_"))
                .unwrap(),
            Some(EndpointSource::Environment)
        );
    }

    #[test]
    fn trust_files_come_from_the_standard_variables_traces_variant_first() {
        let trust = CollectorTrust::from_env(|name| match name {
            "OTEL_EXPORTER_OTLP_CERTIFICATE" => Some("/general/ca.pem".into()),
            "OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE" => Some("/traces/ca.pem".into()),
            "OTEL_EXPORTER_OTLP_TRACES_CLIENT_CERTIFICATE" => Some("  ".into()),
            "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE" => Some("/general/client.pem".into()),
            _ => None,
        });
        assert_eq!(
            trust,
            CollectorTrust {
                certificate: Some(TrustFile {
                    variable: "OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE",
                    path: "/traces/ca.pem".into(),
                }),
                // A blank traces variant is vacant, so the general one holds.
                client_certificate: Some(TrustFile {
                    variable: "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
                    path: "/general/client.pem".into(),
                }),
                client_key: None,
            }
        );
        assert_eq!(
            CollectorTrust::from_env(|_| None),
            CollectorTrust::default()
        );
    }

    #[test]
    fn header_pairs_parse_and_blank_entries_are_skipped() {
        let headers = parse_headers("authorization=Basic%20x, x-tenant = t1 ,").unwrap();
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Basic%20x")
        );
        assert_eq!(headers.get("x-tenant").map(String::as_str), Some("t1"));
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn malformed_header_entries_fail_by_position() {
        for (raw, position) in [
            ("authorization: Bearer x", 1),
            ("a=b,=novalue", 2),
            ("a=b,,bad name=x", 3),
            ("a=line\nbreak", 1),
        ] {
            let err = parse_headers(raw).unwrap_err();
            assert!(
                matches!(err, TracingError::InvalidHeader { position: p } if p == position),
                "{raw:?}: {err}"
            );
        }
    }

    #[test]
    fn collector_root_gets_the_traces_path() {
        assert_eq!(
            traces_url("http://collector:4318"),
            "http://collector:4318/v1/traces"
        );
        assert_eq!(
            traces_url("https://collector:4318/"),
            "https://collector:4318/v1/traces"
        );
        assert_eq!(
            traces_url("https://gateway.test/otlp/v1/traces"),
            "https://gateway.test/otlp/v1/traces"
        );
    }

    #[test]
    fn https_otlp_endpoint_is_admitted_by_the_exporter_client() {
        use futures_util::FutureExt;

        // Without `reqwest-rustls`, reqwest rejects `https://` at export
        // with "URL scheme is not allowed". Construction succeeding is not
        // that proof; the request must run. The batch processor hosts the
        // blocking client off the async workers; a `#[tokio::test]` runtime
        // cannot drop reqwest's inner runtime. Nothing listens on :1, so
        // the accepted failure is a transport error, not collector delivery.
        let exporter = span_exporter(
            Some("https://127.0.0.1:1/v1/traces"),
            HashMap::new(),
            &CollectorTrust::default(),
        )
        .expect("https endpoint must build");
        let result = {
            use opentelemetry_sdk::trace::SpanExporter as _;
            exporter
                .export(Vec::new())
                .now_or_never()
                .expect("blocking OTLP client must finish in one poll")
        };
        let err = result.expect_err("nothing listens on 127.0.0.1:1");
        let message = err.to_string();
        assert!(
            !message.contains("URL scheme is not allowed"),
            "https must be a permitted scheme: {message}"
        );
    }

    #[test]
    fn gzip_compression_is_available_to_the_exporter() {
        // `OTEL_EXPORTER_OTLP_COMPRESSION=gzip` reaches the same check; without
        // the `gzip-http` feature the build fails and tracing degrades.
        use opentelemetry_otlp::WithHttpConfig as _;

        opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_compression(opentelemetry_otlp::Compression::Gzip)
            .build()
            .expect("gzip must be a supported compression");
    }

    #[derive(Debug)]
    struct Fixed(fn() -> OTelSdkResult);

    impl SpanExporter for Fixed {
        fn export(&self, _batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
            std::future::ready((self.0)())
        }
    }

    #[test]
    fn finished_exports_are_counted_by_outcome() {
        use futures_util::FutureExt;
        use opentelemetry::trace::{SpanContext, SpanKind, Status};

        let span = || SpanData {
            span_context: SpanContext::empty_context(),
            parent_span_id: opentelemetry::trace::SpanId::INVALID,
            parent_span_is_remote: false,
            span_kind: SpanKind::Internal,
            name: "span".into(),
            start_time: std::time::SystemTime::UNIX_EPOCH,
            end_time: std::time::SystemTime::UNIX_EPOCH,
            attributes: Vec::new(),
            dropped_attributes_count: 0,
            events: sdktrace::SpanEvents::default(),
            links: sdktrace::SpanLinks::default(),
            status: Status::Unset,
            instrumentation_scope: opentelemetry::InstrumentationScope::default(),
        };
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        metrics::with_local_recorder(&recorder, || {
            let export = |exporter: Fixed, spans: usize| {
                let _ = Counted(exporter)
                    .export((0..spans).map(|_| span()).collect())
                    .now_or_never()
                    .expect("the fixed exporter is ready");
            };
            export(Fixed(|| Ok(())), 3);
            export(Fixed(|| Err(OTelSdkError::Timeout(Duration::ZERO))), 2);
            export(
                Fixed(|| Err(OTelSdkError::InternalFailure("refused".into()))),
                1,
            );
        });
        let rendered = recorder.handle().render();
        for series in [
            "otel_sdk_exporter_span_exported_total 3",
            "otel_sdk_exporter_span_exported_total{error_type=\"timeout\"} 2",
            "otel_sdk_exporter_span_exported_total{error_type=\"internal_failure\"} 1",
        ] {
            assert!(rendered.contains(series), "{series}: {rendered}");
        }
    }

    #[test]
    fn resource_carries_typed_identity() {
        let mut options = options("");
        options.instance_id = "instance-1".to_owned();
        let resource = resource(&options);
        let get = |key: &str| {
            resource
                .get(&opentelemetry::Key::new(key.to_owned()))
                .map(|v| v.to_string())
        };
        assert_eq!(get(attribute::SERVICE_NAME).as_deref(), Some("svc"));
        assert_eq!(get(attribute::SERVICE_VERSION).as_deref(), Some("1.0.0"));
        assert_eq!(
            get(attribute::SERVICE_INSTANCE_ID).as_deref(),
            Some("instance-1")
        );
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

    #[test]
    fn degraded_reason_keeps_at_most_200_unicode_characters() {
        for (input, expected) in [
            (String::new(), String::new()),
            ("short error".to_owned(), "short error".to_owned()),
            ("a".repeat(200), "a".repeat(200)),
            ("a".repeat(201), "a".repeat(200)),
            ("🙂".repeat(201), "🙂".repeat(200)),
            (
                format!("{}Ж🙂", "a".repeat(199)),
                format!("{}Ж", "a".repeat(199)),
            ),
        ] {
            assert_eq!(truncate(&input), expected);
        }
    }
}
