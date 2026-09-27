//! The process logger.
//!
//! One subscriber per process: an `EnvFilter` built from the typed level
//! directive, a JSON or text formatting layer, and, when a tracer provider
//! exists, the OpenTelemetry layer that gives every span an OpenTelemetry context so
//! JSON records carry `traceId` and `spanId`. `log` records are bridged by
//! `tracing-subscriber`'s `tracing-log` feature during `try_init`.

use crate::traces::TracerProviderHandle;
use std::sync::{Arc, OnceLock};
use opentelemetry::trace::TraceContextExt as _;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

/// Output format for the process logger, distinct from the configuration
/// snapshot's `json` / `text` wire enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoggingFormat {
    /// One JSON object per line, flattened, with OpenTelemetry ids.
    Json,
    /// Human-readable single-line output for local use.
    Text,
}

#[derive(Debug)]
pub struct LoggingOptions<'a> {
    /// An `EnvFilter` directive such as `info` or `info,hyper=warn`.
    pub level: &'a str,
    pub format: LoggingFormat,
    /// Installed tracer provider; `None` leaves spans without an
    /// OpenTelemetry context.
    pub tracer_provider: Option<&'a TracerProviderHandle>,
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
pub fn install_subscriber(options: &LoggingOptions<'_>) -> Result<(), LoggingError> {
    let filter = EnvFilter::try_new(options.level).map_err(|source| LoggingError::Directive {
        directive: options.level.to_owned(),
        source,
    })?;
    let otel = options.tracer_provider.map(|handle| {
        tracing_opentelemetry::layer().with_tracer(handle.tracer())
    });
    let trace_dispatch = Arc::new(OnceLock::new());
    let format: Box<dyn Layer<_> + Send + Sync> = match options.format {
        LoggingFormat::Json => {
            let mut layer = json_subscriber::layer()
                .flatten_event(true)
                .with_current_span(false)
                .flatten_span_list_on_top_level(true);
            add_trace_ids(layer.inner_layer_mut(), Arc::clone(&trace_dispatch));
            Box::new(layer)
        }
        LoggingFormat::Text => Box::new(tracing_subscriber::fmt::layer().with_target(false)),
    };
    let dispatch = tracing::Dispatch::new(Registry::default().with(filter).with(otel).with(format));
    let _ = trace_dispatch.set(dispatch.downgrade());
    dispatch.try_init().map_err(|_| LoggingError::AlreadyInstalled)
}

// json-subscriber's built-in bridge currently ends at tracing-opentelemetry
// 0.33. Its dynamic-field API preserves the same wire shape with our 0.34
// bridge. A weak dispatch avoids both subscriber recursion and a reference cycle.
fn add_trace_ids<S, W>(
    layer: &mut json_subscriber::JsonLayer<S, W>,
    dispatch: Arc<OnceLock<tracing::dispatcher::WeakDispatch>>,
) where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    layer.add_dynamic_field("openTelemetry", move |event, context| {
        let span = context.event_span(event)?;
        let dispatch = dispatch.get()?.upgrade()?;
        let otel = tracing_opentelemetry::get_otel_context(&span.id(), &dispatch)?;
        let span = otel.span();
        let ids = span.span_context();
        Some(std::collections::BTreeMap::from([
            ("traceId", ids.trace_id().to_string()),
            ("spanId", ids.span_id().to_string()),
        ]))
    });
}
