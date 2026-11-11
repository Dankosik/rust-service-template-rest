//! OpenTelemetry tracer provider.
//!
//! The provider is always installed so every request has a trace id for log
//! correlation. The OTLP exporter that [`exporter`] builds is added only
//! when an endpoint resolves.

mod exporter;

use exporter::{CollectorTrust, export_timeout, parse_headers, resolve_endpoint, span_exporter};

use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
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
use secrecy::{ExposeSecret, SecretString};

/// Counter: spans in batches whose SDK exporter returned success or failure.
/// `error_type` is finite. Success does not prove receiver acceptance or delivery;
/// SDK queue loss and reported rejection have separate observed metrics.
pub(crate) const TRACE_SPANS_EXPORTED_METRIC: &str = "otel_sdk_exporter_span_exported_total";

/// Gauge: 1 when the OTLP trace exporter was configured and initialized at
/// startup, 0 otherwise. A startup-configuration signal, not delivery health.
pub const TRACE_EXPORTER_ACTIVE_METRIC: &str = "service_startup_trace_exporter_active";

/// Trace sampler selected by typed configuration, with the ratio already
/// attached to the variants that use it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResolvedSampler {
    AlwaysOn,
    AlwaysOff,
    /// Sample the given fraction of traces by trace id, ignoring the parent.
    TraceIdRatio(f64),
    /// Follow the parent's sampling decision; sample root spans by the given ratio.
    ParentBasedTraceIdRatio(f64),
}

/// Resource identity, sampler, and exporter settings for the tracer provider.
#[derive(Debug)]
pub struct TracingOptions {
    pub service_name: String,
    pub service_version: String,
    pub vcs_revision: String,
    /// Reported as `service.instance.id`; `None` reports the host name, and
    /// blank omits the attribute.
    pub instance_id: Option<String>,
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
    Initialized {
        endpoint_source: EndpointSource,
        /// The collector is verified against the certificate file of a
        /// standard variable, not the platform trust store.
        certificate_file: bool,
        /// A client certificate from the standard variables is presented.
        client_certificate: bool,
    },
    Disabled,
    /// An endpoint resolved but the exporter could not be built.
    Degraded {
        /// Bounded label for why the exporter could not be built.
        reason: &'static str,
    },
}

impl ExporterState {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            ExporterState::Initialized { .. } => "initialized",
            ExporterState::Disabled => "disabled",
            ExporterState::Degraded { .. } => "degraded",
        }
    }

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

#[derive(Debug)]
pub struct TracerProviderHandle {
    provider: SdkTracerProvider,
    pub exporter_state: ExporterState,
    drain: Arc<Mutex<DrainObservation>>,
}

/// Instrumentation scope of the `tracing` bridge; the service itself is the
/// `service.name` resource attribute.
const INSTRUMENTATION_SCOPE: &str = env!("CARGO_PKG_NAME");

/// Spans the batch processor queues while an export runs, instead of the SDK's
/// 2048. The processor drops a span when its queue is full and exports from one
/// thread, so a slow or retried export (10 s timeout, three retries) leaves its
/// spans queued: a service at a thousand spans a second fills 2048 in two
/// seconds and 16384 in sixteen. The queue is allocated up front, one
/// [`SpanData`] a slot. An `OTEL_BSP_MAX_QUEUE_SIZE` the SDK accepts still
/// decides.
const SPAN_QUEUE_CAPACITY: usize = 16_384;

/// Join slack around `spawn_blocking` after the SDK's own shutdown timeout.
/// Not extra flush time.
pub const SHUTDOWN_JOIN_SLACK: Duration = Duration::from_millis(500);

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
    let handle = tracer_provider(options, |name| std::env::var_os(name), &trust)?;
    global::set_text_map_propagator(TraceContextPropagator::new());
    global::set_tracer_provider(handle.provider.clone());
    Ok(handle)
}

fn tracer_provider(
    options: &TracingOptions,
    env: impl Fn(&str) -> Option<OsString>,
    trust: &CollectorTrust,
) -> Result<TracerProviderHandle, TracingError> {
    let endpoint = resolve_endpoint(options, &env)?;
    let headers = match options.otlp_headers.as_ref() {
        Some(raw) => parse_headers(raw.expose_secret())?,
        None => HashMap::new(),
    };
    let timeout = export_timeout(&env);

    let mut builder = SdkTracerProvider::builder()
        .with_resource(resource(options))
        .with_sampler(options.sampler.to_sdk());

    let drain = Arc::new(Mutex::new(DrainObservation::default()));
    let exporter_state = match endpoint {
        None => ExporterState::Disabled,
        Some(endpoint) => match span_exporter(&endpoint, headers, trust, timeout, &env) {
            Ok(span_exporter) => {
                builder = builder.with_span_processor(
                    sdktrace::BatchSpanProcessor::builder(ObservedExporter {
                        inner: span_exporter,
                        timeout,
                        drain: Arc::clone(&drain),
                    })
                    .with_batch_config(batch_config(&env))
                    .build(),
                );
                ExporterState::Initialized {
                    endpoint_source: endpoint.source(),
                    certificate_file: trust.certificate.is_some(),
                    client_certificate: trust.client_certificate.is_some(),
                }
            }
            // The error's text may name files and SDK details, which stay
            // out of logs and metrics; the fixed category is reported.
            Err(_withheld) => ExporterState::Degraded {
                reason: "exporter_build",
            },
        },
    };

    Ok(TracerProviderHandle {
        provider: builder.build(),
        exporter_state,
        drain,
    })
}

/// The SDK's batch defaults, with the larger queue unless the operator set one
/// the SDK accepts.
///
/// The SDK ignores a value it cannot parse and clamps the batch size to its
/// own 2048 queue while reading the environment, so both are applied again
/// here, after the queue is final.
fn batch_config(env: &impl Fn(&str) -> Option<OsString>) -> sdktrace::BatchConfig {
    let size = |name| env(name)?.to_str()?.parse::<usize>().ok();
    let config = sdktrace::BatchConfigBuilder::default().with_max_queue_size(
        size(sdktrace::OTEL_BSP_MAX_QUEUE_SIZE).unwrap_or(SPAN_QUEUE_CAPACITY),
    );
    match size(sdktrace::OTEL_BSP_MAX_EXPORT_BATCH_SIZE) {
        Some(batch) => config.with_max_export_batch_size(batch),
        None => config,
    }
    .build()
}

impl TracerProviderHandle {
    #[must_use]
    pub(crate) fn tracer(&self) -> SdkTracer {
        self.provider.tracer(INSTRUMENTATION_SCOPE)
    }

    /// Stop the provider off the async workers within the caller's deadline.
    ///
    /// The SDK receives the time left when its blocking job starts, minus
    /// [`SHUTDOWN_JOIN_SLACK`]. The SDK's `shutdown_with_timeout` blocks, so it
    /// always runs on a blocking thread. An exhausted allowance skips the wait
    /// but still submits that call with a zero budget, so the shared span
    /// processors are told to stop; its unconfirmed completion is reported
    /// without extending the deadline.
    pub async fn shutdown(self, deadline: tokio::time::Instant) -> ProviderShutdown {
        self.shutdown_with_budget(Duration::MAX, deadline).await
    }

    /// [`Self::shutdown`] with the SDK's time also capped at `sdk_budget`.
    async fn shutdown_with_budget(
        self,
        sdk_budget: Duration,
        deadline: tokio::time::Instant,
    ) -> ProviderShutdown {
        self.drain
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .draining = true;
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
        // Tokio polls the inner future before its timeout. A result observed
        // after the deadline cannot turn an expired wait into completion.
        if tokio::time::Instant::now() >= deadline {
            observed_here |= ProviderShutdownReasons::DEADLINE;
        }
        let reasons = self
            .drain
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reasons
            | observed_here;
        if reasons.is_empty() {
            ProviderShutdown::Completed
        } else {
            ProviderShutdown::Incomplete(reasons)
        }
    }
}

/// Outcome of [`TracerProviderHandle::shutdown`], not a delivery receipt.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderShutdown {
    Completed,
    /// The drain failed or could not be confirmed within its deadline.
    Incomplete(ProviderShutdownReasons),
}

bitflags::bitflags! {
    /// Finite failure observations; no exporter error text is retained.
    /// Operators see [`Self::bits`] in the `telemetry` shutdown stage.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct ProviderShutdownReasons: u32 {
        /// An SDK operation during the drain (an export, or the exporter's or
        /// provider's shutdown) reported a timeout or failed after its whole
        /// budget.
        const SDK_TIMEOUT = 1;
        /// An SDK operation during the drain found its target already shut down.
        const SDK_ALREADY_SHUTDOWN = 1 << 1;
        /// An SDK operation during the drain reported an internal failure.
        const SDK_INTERNAL_FAILURE = 1 << 2;
        /// The exporter's shutdown failed; an `SDK_*` bit says how.
        const EXPORTER_SHUTDOWN = 1 << 3;
        /// The provider's shutdown failed; an `SDK_*` bit says how.
        const PROVIDER_SHUTDOWN = 1 << 4;
        /// The blocking shutdown task panicked or was cancelled.
        const JOIN = 1 << 5;
        /// The caller's deadline left no time for the SDK or passed before its
        /// shutdown was confirmed.
        const DEADLINE = 1 << 6;
    }
}

#[derive(Debug, Default)]
struct DrainObservation {
    draining: bool,
    reasons: ProviderShutdownReasons,
}

impl DrainObservation {
    /// Records a failure once draining, with `operation` naming the operation.
    fn latch(drain: &Mutex<Self>, failure: SdkFailure, operation: ProviderShutdownReasons) {
        let mut drain = drain
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if drain.draining {
            drain.reasons |= operation | failure.reason();
        }
    }
}

/// The category of a failed SDK operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SdkFailure {
    Timeout,
    AlreadyShutdown,
    Internal,
}

impl SdkFailure {
    /// The SDK's category, except that a failure after the whole `budget`
    /// is a timeout: `opentelemetry-otlp` 0.33.1 returns a timed-out export
    /// as [`OTelSdkError::InternalFailure`], and
    /// [`SdkTracerProvider::shutdown_with_timeout`]
    /// wraps its processor's `Timeout` in one.
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

    /// The `error_type` label of [`TRACE_SPANS_EXPORTED_METRIC`].
    const fn label(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::AlreadyShutdown => "already_shutdown",
            Self::Internal => "internal_failure",
        }
    }
}

pub(crate) fn describe_metrics() {
    metrics::describe_gauge!(
        TRACE_EXPORTER_ACTIVE_METRIC,
        "1 when the OTLP trace exporter is configured and initialized, 0 otherwise."
    );
    metrics::describe_counter!(
        TRACE_SPANS_EXPORTED_METRIC,
        "Spans in batches whose SDK exporter returned success or failure; error_type marks failure, not receiver acceptance or persistence."
    );
}

/// The batch processor's exporter: counts the spans of every finished export
/// under [`TRACE_SPANS_EXPORTED_METRIC`], classified by the SDK exporter
/// result, and latches the drain's failures into [`ProviderShutdownReasons`].
/// These counts do not establish collector acceptance or persistence.
#[derive(Debug)]
struct ObservedExporter<E> {
    inner: E,
    /// The exporter's own timeout, which also bounds its retries.
    timeout: Duration,
    drain: Arc<Mutex<DrainObservation>>,
}

impl<E: SpanExporter> SpanExporter for ObservedExporter<E> {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        let spans = u64::try_from(batch.len()).unwrap_or(u64::MAX);
        let started = std::time::Instant::now();
        let result = self.inner.export(batch).await;
        match &result {
            Ok(()) => metrics::counter!(TRACE_SPANS_EXPORTED_METRIC).increment(spans),
            Err(error) => {
                let failure = SdkFailure::classify(error, started.elapsed(), self.timeout);
                DrainObservation::latch(&self.drain, failure, ProviderShutdownReasons::empty());
                metrics::counter!(TRACE_SPANS_EXPORTED_METRIC, "error_type" => failure.label())
                    .increment(spans);
            }
        }
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
    let instance_id = options
        .instance_id
        .clone()
        .unwrap_or_else(|| gethostname::gethostname().to_string_lossy().into_owned());
    let instance = (!instance_id.trim().is_empty())
        .then(|| KeyValue::new(attribute::SERVICE_INSTANCE_ID, instance_id));
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

#[cfg(test)]
mod tests;
