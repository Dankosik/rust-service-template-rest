mod exporter;

use exporter::{CollectorConfig, CollectorTrust, compression, export_timeout, span_exporter};
use std::ffi::OsString;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use opentelemetry::trace::TracerProvider;
use opentelemetry::{KeyValue, global};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{
    self as sdktrace, SdkTracer, SdkTracerProvider, SpanData, SpanExporter,
};
use opentelemetry_semantic_conventions::attribute;
use parking_lot::Mutex;
use secrecy::SecretString;
use uuid::Uuid;

const TRACE_EXPORT_SDK_RESULT_SPANS_METRIC: &str =
    "telemetry_trace_exporter_sdk_result_spans_total";

static EXPORT_METRIC_METADATA: metrics::Metadata<'static> =
    metrics::Metadata::new(module_path!(), metrics::Level::INFO, Some(module_path!()));

pub(crate) const TRACE_EXPORTER_ACTIVE_METRIC: &str = "service_startup_trace_exporter_active";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResolvedSampler {
    AlwaysOn,
    AlwaysOff,
    TraceIdRatio(f64),
    ParentBasedTraceIdRatio(f64),
}

#[derive(derive_more::Debug)]
pub struct TracingOptions<'a> {
    pub service_name: &'a str,
    #[debug(skip)]
    pub service_version: &'a str,
    #[debug(skip)]
    pub vcs_revision: &'a str,
    #[debug(skip)]
    pub instance_id: Option<&'a str>,
    #[debug(skip)]
    pub deployment_environment: &'a str,
    pub sampler: ResolvedSampler,
    #[debug(skip)]
    pub otlp_endpoint: Option<&'a str>,
    #[debug(skip)]
    pub otlp_headers: Option<&'a SecretString>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointSource {
    Typed,
    Environment,
}

impl EndpointSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Typed => "typed",
            Self::Environment => "environment",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExporterState {
    Initialized {
        endpoint_source: EndpointSource,
        has_certificate_file: bool,
        has_client_certificate: bool,
        ignored_compression: Vec<&'static str>,
        invalid_batch_sizes: Vec<&'static str>,
    },
    Disabled,
}

impl ExporterState {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            ExporterState::Initialized { .. } => "initialized",
            ExporterState::Disabled => "disabled",
        }
    }

    pub fn log(&self) {
        match self {
            ExporterState::Initialized {
                endpoint_source,
                has_certificate_file,
                has_client_certificate,
                ignored_compression,
                invalid_batch_sizes,
            } => {
                for name in ignored_compression {
                    tracing::warn!(
                        variable = *name,
                        "unsupported OTLP compression setting ignored"
                    );
                }
                for name in invalid_batch_sizes {
                    tracing::warn!(
                        variable = *name,
                        "invalid trace batch processor size ignored"
                    );
                }
                tracing::info!(
                    endpoint_source = endpoint_source.as_str(),
                    certificate_file = has_certificate_file,
                    client_certificate = has_client_certificate,
                    "trace exporter initialized"
                );
            }
            ExporterState::Disabled => {}
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct TracingError(TracingErrorKind);

#[derive(Debug, thiserror::Error)]
enum TracingErrorKind {
    #[error(
        "ambient OpenTelemetry header variable {name} is present while a typed \
         OTLP endpoint selects the collector; unset it or configure the \
         credential through the typed OTLP headers field"
    )]
    AmbientCredential { name: &'static str },
    #[error(
        "{name} entry {position} must specify a unique, valid HTTP header and preserve the collector host"
    )]
    InvalidHeader { name: &'static str, position: usize },
    #[error("{name} must be valid Unicode")]
    NonUnicode { name: &'static str },
    #[error("{name} must be an absolute HTTP(S) collector URL without userinfo, query or fragment")]
    InvalidEndpoint { name: &'static str },
    #[error("{name} requires an explicitly matching collector endpoint")]
    UnboundCredential { name: &'static str },
    #[error("{name} requires an HTTPS collector endpoint")]
    InsecureCollector { name: &'static str },
    #[error("could not read the OTLP trust file selected by {name} ({kind:?})")]
    TrustRead {
        name: &'static str,
        kind: std::io::ErrorKind,
    },
    #[error("{name} must select a regular OTLP trust file")]
    TrustFileType { name: &'static str },
    #[error("the OTLP trust file selected by {name} exceeds the {max_bytes}-byte limit")]
    TrustFileTooLarge { name: &'static str, max_bytes: u64 },
    #[error("{name} must select a usable PEM certificate bundle")]
    TrustPem { name: &'static str },
    #[error("the OTLP client certificate and key must form a usable PEM identity")]
    IdentityPem,
    #[error("{present} requires {missing} to configure an OTLP client identity")]
    IncompleteIdentity {
        present: &'static str,
        missing: &'static str,
    },
    #[error(
        "could not construct the OTLP collector HTTP client with the configured trust material"
    )]
    ClientBuild,
    #[error("the OTLP collector client initialization thread failed")]
    ClientThread,
    #[error("OTLP collector preparation timed out")]
    PreparationTimeout,
    #[error("could not initialize the OTLP trace exporter")]
    ExporterBuild,
}

#[derive(Debug)]
#[must_use = "retain the tracer provider handle and shut it down before process exit"]
pub struct TracerProviderHandle {
    provider: SdkTracerProvider,
    exporter_state: ExporterState,
    drain: Arc<Mutex<DrainObservation>>,
}

const INSTRUMENTATION_SCOPE: &str = env!("CARGO_PKG_NAME");

const SPAN_QUEUE_CAPACITY: usize = 16_384;

const EXPORTER_PREPARATION_TIMEOUT: Duration = Duration::from_secs(5);

pub const SHUTDOWN_JOIN_SLACK: Duration = Duration::from_millis(500);

pub async fn install_tracer_provider(
    options: &TracingOptions<'_>,
) -> Result<TracerProviderHandle, TracingError> {
    let trust = CollectorTrust::from_env(|name| std::env::var_os(name));
    let handle = prepare_tracer_provider(options, |name| std::env::var_os(name), trust).await?;
    global::set_text_map_propagator(TraceContextPropagator::new());
    global::set_tracer_provider(handle.provider.clone());
    Ok(handle)
}

async fn prepare_tracer_provider(
    options: &TracingOptions<'_>,
    env: impl Fn(&str) -> Option<OsString>,
    trust: CollectorTrust,
) -> Result<TracerProviderHandle, TracingError> {
    let collector = CollectorConfig::resolve(options, &trust, &env).map_err(TracingError)?;

    let builder = SdkTracerProvider::builder()
        .with_resource(resource(options))
        .with_sampler(options.sampler.to_sdk());

    let drain = Arc::new(Mutex::new(DrainObservation::default()));
    let Some(collector) = collector else {
        return Ok(TracerProviderHandle {
            provider: builder.build(),
            exporter_state: ExporterState::Disabled,
            drain,
        });
    };
    let timeout = export_timeout(&env);
    let (compression, ignored_compression) = compression(&env);
    let (batch_config, invalid_batch_sizes) = batch_config(&env);
    let has_certificate_file = trust.certificate.is_some();
    let has_client_certificate = trust.client_certificate.is_some();
    let deadline = tokio::time::Instant::now() + EXPORTER_PREPARATION_TIMEOUT;
    let preparation = tokio::task::spawn_blocking(move || {
        span_exporter(
            collector.url.as_str(),
            collector.headers,
            &trust,
            timeout,
            compression,
        )
    });
    let span_exporter = match tokio::time::timeout_at(deadline, preparation).await {
        Ok(result) if tokio::time::Instant::now() < deadline => result
            .map_err(|_| TracingError(TracingErrorKind::ClientThread))?
            .map_err(TracingError)?,
        _ => return Err(TracingError(TracingErrorKind::PreparationTimeout)),
    };
    let provider = builder
        .with_span_processor(
            sdktrace::BatchSpanProcessor::builder(ObservedExporter {
                inner: span_exporter,
                timeout,
                drain: Arc::clone(&drain),
            })
            .with_batch_config(batch_config)
            .build(),
        )
        .build();

    Ok(TracerProviderHandle {
        provider,
        exporter_state: ExporterState::Initialized {
            endpoint_source: collector.source,
            has_certificate_file,
            has_client_certificate,
            ignored_compression,
            invalid_batch_sizes,
        },
        drain,
    })
}

fn batch_config(
    env: &impl Fn(&str) -> Option<OsString>,
) -> (sdktrace::BatchConfig, Vec<&'static str>) {
    let mut invalid = Vec::new();
    let mut parse_size = |name: &'static str, default| {
        let Some(value) = env(name).filter(|value| !value.is_empty()) else {
            return default;
        };
        if let Some(size) = value
            .to_str()
            .and_then(|text| text.parse::<std::num::NonZeroUsize>().ok())
        {
            return size.get();
        }
        invalid.push(name);
        default
    };
    let queue_size = parse_size(sdktrace::OTEL_BSP_MAX_QUEUE_SIZE, SPAN_QUEUE_CAPACITY);
    let batch_size = parse_size(
        sdktrace::OTEL_BSP_MAX_EXPORT_BATCH_SIZE,
        sdktrace::OTEL_BSP_MAX_EXPORT_BATCH_SIZE_DEFAULT,
    );
    let config = sdktrace::BatchConfigBuilder::default()
        .with_max_queue_size(queue_size)
        .with_max_export_batch_size(batch_size)
        .build();
    (config, invalid)
}

impl TracerProviderHandle {
    #[must_use]
    pub const fn exporter_state(&self) -> &ExporterState {
        &self.exporter_state
    }

    #[must_use]
    pub(crate) fn tracer(&self) -> SdkTracer {
        self.provider.tracer(INSTRUMENTATION_SCOPE)
    }

    pub async fn shutdown(self, deadline: tokio::time::Instant) -> ProviderShutdown {
        self.shutdown_with_budget(Duration::MAX, deadline).await
    }

    async fn shutdown_with_budget(
        self,
        sdk_budget: Duration,
        deadline: tokio::time::Instant,
    ) -> ProviderShutdown {
        self.drain.lock().begin_drain();
        let allowance = deadline.saturating_duration_since(tokio::time::Instant::now());
        let provider = self.provider;
        let job = tokio::task::spawn_blocking(move || {
            let budget = sdk_budget.min(
                deadline
                    .saturating_duration_since(tokio::time::Instant::now())
                    .saturating_sub(SHUTDOWN_JOIN_SLACK),
            );
            let started = tokio::time::Instant::now();
            provider
                .shutdown_with_timeout(budget)
                .map_err(|error| SdkFailure::classify(&error, started.elapsed(), budget))
        });
        let mut observed_here = if allowance <= SHUTDOWN_JOIN_SLACK {
            ProviderShutdownReasons::DEADLINE
        } else {
            match tokio::time::timeout_at(deadline, job).await {
                Err(_elapsed) => ProviderShutdownReasons::DEADLINE,
                Ok(Err(_join)) => ProviderShutdownReasons::JOIN,
                Ok(Ok(Ok(()))) => ProviderShutdownReasons::empty(),
                Ok(Ok(Err(failure))) => {
                    ProviderShutdownReasons::PROVIDER_SHUTDOWN | failure.reason()
                }
            }
        };
        if tokio::time::Instant::now() >= deadline {
            observed_here |= ProviderShutdownReasons::DEADLINE;
        }
        let reasons = self.drain.lock().reasons() | observed_here;
        if reasons.is_empty() {
            ProviderShutdown::Completed
        } else {
            ProviderShutdown::Incomplete(reasons)
        }
    }
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderShutdown {
    Completed,
    Incomplete(ProviderShutdownReasons),
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct ProviderShutdownReasons: u32 {
        const SDK_TIMEOUT = 1;
        const SDK_ALREADY_SHUTDOWN = 1 << 1;
        const SDK_INTERNAL_FAILURE = 1 << 2;
        const EXPORTER_SHUTDOWN = 1 << 3;
        const PROVIDER_SHUTDOWN = 1 << 4;
        const JOIN = 1 << 5;
        const DEADLINE = 1 << 6;
    }
}

#[derive(Debug, Default)]
enum DrainObservation {
    #[default]
    Idle,
    Draining(ProviderShutdownReasons),
}

impl DrainObservation {
    fn begin_drain(&mut self) {
        *self = Self::Draining(ProviderShutdownReasons::empty());
    }

    fn reasons(&self) -> ProviderShutdownReasons {
        match self {
            Self::Idle => ProviderShutdownReasons::empty(),
            Self::Draining(reasons) => *reasons,
        }
    }

    fn latch(drain: &Mutex<Self>, failure: SdkFailure, operation: ProviderShutdownReasons) {
        if let Self::Draining(reasons) = &mut *drain.lock() {
            *reasons |= operation | failure.reason();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SdkFailure {
    Timeout,
    AlreadyShutdown,
    Internal,
}

impl SdkFailure {
    fn classify(error: &OTelSdkError, elapsed: Duration, budget: Duration) -> Self {
        match error {
            OTelSdkError::Timeout(_) => Self::Timeout,
            OTelSdkError::AlreadyShutdown => Self::AlreadyShutdown,
            OTelSdkError::InternalFailure(_) if elapsed >= budget => Self::Timeout,
            OTelSdkError::InternalFailure(_) => Self::Internal,
        }
    }

    const fn reason(self) -> ProviderShutdownReasons {
        match self {
            Self::Timeout => ProviderShutdownReasons::SDK_TIMEOUT,
            Self::AlreadyShutdown => ProviderShutdownReasons::SDK_ALREADY_SHUTDOWN,
            Self::Internal => ProviderShutdownReasons::SDK_INTERNAL_FAILURE,
        }
    }

    fn export_metric_key(self) -> &'static metrics::Key {
        match self {
            Self::Timeout => crate::metrics::static_metric_key!(
                TRACE_EXPORT_SDK_RESULT_SPANS_METRIC,
                ("outcome", "error"),
                ("error_type", "timeout"),
            ),
            Self::AlreadyShutdown => crate::metrics::static_metric_key!(
                TRACE_EXPORT_SDK_RESULT_SPANS_METRIC,
                ("outcome", "error"),
                ("error_type", "already_shutdown"),
            ),
            Self::Internal => crate::metrics::static_metric_key!(
                TRACE_EXPORT_SDK_RESULT_SPANS_METRIC,
                ("outcome", "error"),
                ("error_type", "internal_failure"),
            ),
        }
    }
}

pub(crate) fn describe_metrics() {
    metrics::describe_gauge!(
        TRACE_EXPORTER_ACTIVE_METRIC,
        "1 when the OTLP trace exporter is configured and initialized, 0 otherwise."
    );
    metrics::describe_counter!(
        TRACE_EXPORT_SDK_RESULT_SPANS_METRIC,
        "Spans in batches grouped by the SDK export call result. An ok outcome can include partial rejection or a response decoding error."
    );
}

#[derive(Debug)]
struct ObservedExporter<E> {
    inner: E,
    timeout: Duration,
    drain: Arc<Mutex<DrainObservation>>,
}

impl<E: SpanExporter> SpanExporter for ObservedExporter<E> {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        let spans = u64::try_from(batch.len()).unwrap_or(u64::MAX);
        let started = std::time::Instant::now();
        let result = self.inner.export(batch).await;
        let key = match &result {
            Ok(()) => crate::metrics::static_metric_key!(
                TRACE_EXPORT_SDK_RESULT_SPANS_METRIC,
                ("outcome", "ok"),
            ),
            Err(error) => {
                let failure = SdkFailure::classify(error, started.elapsed(), self.timeout);
                DrainObservation::latch(&self.drain, failure, ProviderShutdownReasons::empty());
                failure.export_metric_key()
            }
        };
        metrics::with_recorder(|recorder| recorder.register_counter(key, &EXPORT_METRIC_METADATA))
            .increment(spans);
        result
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        let started = std::time::Instant::now();
        let result = self.inner.shutdown_with_timeout(timeout);
        if let Err(error) = &result {
            DrainObservation::latch(
                &self.drain,
                SdkFailure::classify(error, started.elapsed(), timeout),
                ProviderShutdownReasons::EXPORTER_SHUTDOWN,
            );
        }
        result
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.inner.set_resource(resource);
    }
}

fn resource(options: &TracingOptions<'_>) -> Resource {
    static PROCESS_INSTANCE_ID: OnceLock<Uuid> = OnceLock::new();
    let instance_id = options
        .instance_id
        .filter(|value| !value.trim().is_empty())
        .map_or_else(
            || PROCESS_INSTANCE_ID.get_or_init(Uuid::new_v4).to_string(),
            str::to_owned,
        );
    let attributes = [
        KeyValue::new(attribute::SERVICE_NAME, options.service_name.to_owned()),
        KeyValue::new(
            attribute::SERVICE_VERSION,
            options.service_version.to_owned(),
        ),
        KeyValue::new(
            attribute::VCS_REF_HEAD_REVISION,
            options.vcs_revision.to_owned(),
        ),
        KeyValue::new(
            attribute::DEPLOYMENT_ENVIRONMENT_NAME,
            options.deployment_environment.to_owned(),
        ),
        KeyValue::new(attribute::SERVICE_INSTANCE_ID, instance_id),
    ];
    Resource::builder().with_attributes(attributes).build()
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

#[cfg(test)]
mod tests;
