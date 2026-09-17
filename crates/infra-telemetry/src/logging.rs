//! The process logger.
//!
//! One subscriber per process: an `EnvFilter` built from the typed level
//! directive, a JSON or text formatting layer, and, when a tracer provider
//! exists, the OpenTelemetry layer that gives every span an OpenTelemetry context so
//! JSON records carry `traceId` and `spanId`. `log` records are bridged by
//! `tracing-subscriber`'s `tracing-log` feature during `try_init`.

use opentelemetry_sdk::trace::SdkTracer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

/// Output format for the process logger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    /// One JSON object per line, flattened, with OpenTelemetry ids.
    Json,
    /// Human-readable single-line output for local use.
    Text,
}

#[derive(Debug)]
pub struct LoggingOptions {
    /// An `EnvFilter` directive such as `info` or `info,hyper=warn`.
    pub level: String,
    pub format: LogFormat,
    /// Tracer for the OpenTelemetry layer; `None` leaves spans without an
    /// OpenTelemetry context.
    pub tracer: Option<SdkTracer>,
}

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("log.level {directive:?} is not a valid filter directive: {source}")]
    Directive {
        directive: String,
        #[source]
        source: tracing_subscriber::filter::ParseError,
    },
    #[error("a global tracing subscriber is already installed")]
    AlreadyInstalled,
}

/// Install the global subscriber.
///
/// # Errors
///
/// Returns an error for an unparsable directive or a second installation in
/// the same process.
pub fn install_subscriber(options: LoggingOptions) -> Result<(), LoggingError> {
    let filter = EnvFilter::try_new(&options.level).map_err(|source| LoggingError::Directive {
        directive: options.level.clone(),
        source,
    })?;
    let otel = options
        .tracer
        .map(|tracer| tracing_opentelemetry::layer().with_tracer(tracer));
    let format: Box<dyn Layer<_> + Send + Sync> = match options.format {
        LogFormat::Json => Box::new(
            json_subscriber::layer()
                .flatten_event(true)
                .with_current_span(false)
                .flatten_span_list_on_top_level(true)
                .with_opentelemetry_ids(true),
        ),
        LogFormat::Text => Box::new(tracing_subscriber::fmt::layer().with_target(false)),
    };
    Registry::default()
        .with(filter)
        .with(otel)
        .with(format)
        .try_init()
        .map_err(|_| LoggingError::AlreadyInstalled)
}
