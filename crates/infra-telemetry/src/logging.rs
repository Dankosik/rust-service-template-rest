//! The process logger.
//!
//! One subscriber per process: an `EnvFilter` built from the typed level
//! directive, a JSON or text formatting layer, and, when a tracer provider
//! exists, the OpenTelemetry layer that gives every span an OpenTelemetry context so
//! JSON records carry `traceId` and `spanId`. `log` records are bridged by
//! `tracing-subscriber`'s `tracing-log` feature during `try_init`.

use crate::traces::TracerProviderHandle;
use opentelemetry::trace::TraceContextExt as _;
use std::sync::{Arc, OnceLock};
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
    // Source location, thread, and busy/idle timings would be added to every
    // span, sampled or not, at about 2% of a small request's instructions;
    // no dashboard or runbook reads them.
    let otel = options.tracer_provider.map(|handle| {
        tracing_opentelemetry::layer()
            .with_tracer(handle.tracer())
            .with_location(false)
            .with_threads(false)
            .with_tracked_inactivity(false)
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
    dispatch
        .try_init()
        .map_err(|_| LoggingError::AlreadyInstalled)
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
    layer.add_multiple_dynamic_fields(move |event, context, writer| {
        let Some(span) = context.event_span(event) else {
            return;
        };
        let Some(dispatch) = dispatch
            .get()
            .and_then(tracing::dispatcher::WeakDispatch::upgrade)
        else {
            return;
        };
        let Some(otel) = tracing_opentelemetry::get_otel_context(&span.id(), &dispatch) else {
            return;
        };
        let span = otel.span();
        let ids = span.span_context();
        let _ = writer.write_field(
            "openTelemetry",
            std::collections::BTreeMap::from([
                ("traceId", ids.trace_id().to_string()),
                ("spanId", ids.span_id().to_string()),
            ]),
        );
    });
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "test-only JSON fixtures fail closed with precise local setup context"
    )]

    use super::*;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
    use std::io;
    use std::sync::Mutex;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Buffer {
        fn records(&self) -> String {
            String::from_utf8(self.0.lock().expect("test writer mutex").clone())
                .expect("json subscriber only writes UTF-8")
        }
    }

    struct BufferWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for BufferWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut buffer = self
                .0
                .lock()
                .map_err(|_| io::Error::other("test writer mutex"))?;
            io::Write::write(&mut *buffer, bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for Buffer {
        type Writer = BufferWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            BufferWriter(Arc::clone(&self.0))
        }
    }

    #[derive(Clone, Copy)]
    enum EventParent {
        Current,
        Explicit,
    }

    #[derive(Clone, Copy)]
    enum Sampling {
        Sampled,
        NotSampled,
    }

    fn emit_correlated_event(
        sampling: Sampling,
        event_parent: EventParent,
    ) -> (String, String, String, bool) {
        let buffer = Buffer::default();
        let provider = SdkTracerProvider::builder()
            .with_sampler(match sampling {
                Sampling::Sampled => Sampler::AlwaysOn,
                Sampling::NotSampled => Sampler::AlwaysOff,
            })
            .build();
        let trace_dispatch = Arc::new(OnceLock::new());
        let mut layer = json_subscriber::layer()
            .with_writer(buffer.clone())
            .flatten_event(true)
            .with_current_span(false)
            .flatten_span_list_on_top_level(true);
        add_trace_ids(layer.inner_layer_mut(), Arc::clone(&trace_dispatch));
        let subscriber = Registry::default()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")))
            .with(layer);
        let dispatch = tracing::Dispatch::new(subscriber);
        trace_dispatch
            .set(dispatch.downgrade())
            .expect("trace dynamic field receives one dispatch");

        let (trace_id, span_id, sampled) = tracing::dispatcher::with_default(&dispatch, || {
            let span = tracing::info_span!("correlation_parent");
            let context = span.context();
            let otel_span = context.span();
            let span_context = otel_span.span_context();
            let expected = (
                span_context.trace_id().to_string(),
                span_context.span_id().to_string(),
                span_context.is_sampled(),
            );

            match event_parent {
                EventParent::Current => span.in_scope(|| tracing::info!("correlation probe")),
                EventParent::Explicit => tracing::info!(parent: &span, "correlation probe"),
            }

            expected
        });

        (buffer.records(), trace_id, span_id, sampled)
    }

    fn emit_event_without_otel_context(with_span: bool) -> String {
        let buffer = Buffer::default();
        let trace_dispatch = Arc::new(OnceLock::new());
        let mut layer = json_subscriber::layer()
            .with_writer(buffer.clone())
            .flatten_event(true)
            .with_current_span(false)
            .flatten_span_list_on_top_level(true);
        add_trace_ids(layer.inner_layer_mut(), Arc::clone(&trace_dispatch));
        let dispatch = tracing::Dispatch::new(Registry::default().with(layer));
        trace_dispatch
            .set(dispatch.downgrade())
            .expect("trace dynamic field receives one dispatch");

        tracing::dispatcher::with_default(&dispatch, || {
            if with_span {
                tracing::info_span!("plain_parent").in_scope(|| tracing::info!("plain probe"));
            } else {
                tracing::info!("plain probe");
            }
        });

        buffer.records()
    }

    #[test]
    fn json_logs_keep_otel_correlation_for_supported_event_parents_and_sampling() {
        for (name, sampling, event_parent, expected_sampled) in [
            (
                "active sampled parent",
                Sampling::Sampled,
                EventParent::Current,
                true,
            ),
            (
                "explicit sampled parent",
                Sampling::Sampled,
                EventParent::Explicit,
                true,
            ),
            (
                "active non-sampled parent",
                Sampling::NotSampled,
                EventParent::Current,
                false,
            ),
        ] {
            let (record, trace_id, span_id, sampled) =
                emit_correlated_event(sampling, event_parent);
            assert_eq!(sampled, expected_sampled, "{name}");
            assert!(
                record.contains(&format!(
                    r#""openTelemetry":{{"spanId":"{span_id}","traceId":"{trace_id}"}}"#
                )),
                "{name} must preserve the nested OpenTelemetry schema: {record}"
            );
        }
    }

    #[test]
    fn json_logs_omit_otel_correlation_without_a_span_or_provider() {
        for (name, with_span) in [("no span", false), ("no provider", true)] {
            let record = emit_event_without_otel_context(with_span);
            assert!(
                !record.contains("\"openTelemetry\""),
                "{name} must not emit empty or invalid trace ids: {record}"
            );
        }
    }
}
