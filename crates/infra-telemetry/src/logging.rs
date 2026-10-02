//! The process logger.
//!
//! One subscriber per process: a filter from the typed level directive
//! (`EnvFilter` grammar), a JSON or text formatting layer, and, when a tracer provider
//! exists, the OpenTelemetry layer that gives every span an OpenTelemetry context so
//! JSON records carry `trace_id` and `span_id`. `log` records are bridged by
//! `tracing-subscriber`'s `tracing-log` feature during `try_init`.
//!
//! The directive chooses the records. Spans at INFO and above exist under
//! every directive, so a quieter `log.level` neither stops trace export nor
//! strips the request fields and trace context from the records that remain.

mod json;

use crate::traces::TracerProviderHandle;
use tracing::level_filters::LevelFilter;
use tracing::subscriber::Interest;
use tracing::{Level, Metadata, Subscriber, span};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::{Context, SubscriberExt};
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
    let (targets, filter) = level_filter(options.level)?;
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
    let format: Box<dyn Layer<_> + Send + Sync> = match options.format {
        LoggingFormat::Json => Box::new(json::JsonLayer::new(std::io::stdout)),
        LoggingFormat::Text => Box::new(tracing_subscriber::fmt::layer().with_target(false)),
    };
    Registry::default()
        .with(targets)
        .with(filter)
        // template:begin object-storage:telemetry-sdk-log-cap-apply
        .with(sdk_log_cap(options.level))
        // template:end object-storage:telemetry-sdk-log-cap-apply
        .with(otel)
        .with(format)
        .try_init()
        .map_err(|_| LoggingError::AlreadyInstalled)
}

/// Whether the panic hook records the panic's message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanicMessage {
    /// Record it, as Rust's own hook prints it.
    Recorded,
    /// Leave it out: the panicking code may have formatted caller-controlled
    /// data into it.
    Withheld,
}

/// Replace Rust's panic hook with one that reports a panic as an ERROR
/// record: its place in the source, its thread, its message when `message`
/// records it, and a backtrace when `RUST_BACKTRACE` asks for one.
///
/// Rust's hook prints plain text to stderr, which a JSON log pipeline cannot
/// parse, and always prints the message. Install after the subscriber; a panic before that has nowhere to be recorded and
/// keeps Rust's hook. A later call replaces the earlier hook.
pub fn install_panic_hook(message: PanicMessage) {
    std::panic::set_hook(Box::new(move |info| {
        let location = info.location();
        let backtrace = std::backtrace::Backtrace::capture();
        let captured = backtrace.status() == std::backtrace::BacktraceStatus::Captured;
        tracing::error!(
            panic.message = match message {
                PanicMessage::Recorded => info.payload_as_str(),
                PanicMessage::Withheld => None,
            },
            panic.file = location.map(std::panic::Location::file),
            panic.line = location.map(std::panic::Location::line),
            panic.column = location.map(std::panic::Location::column),
            panic.thread = std::thread::current().name(),
            panic.backtrace = captured.then(|| tracing::field::display(&backtrace)),
            "panicked"
        );
    }));
}

/// The directive's filter with the spans every directive keeps: `Targets`
/// when that is exact, `EnvFilter` otherwise.
type LevelFilters = (
    Option<WithInfoSpans<Targets>>,
    Option<WithInfoSpans<EnvFilter>>,
);

fn level_filter(directive: &str) -> Result<LevelFilters, LoggingError> {
    let filter = EnvFilter::try_new(directive).map_err(|source| LoggingError::Directive {
        directive: directive.to_owned(),
        source,
    })?;
    Ok(match static_targets(directive) {
        Some(targets) => (Some(WithInfoSpans(targets)), None),
        None => (None, Some(WithInfoSpans(filter))),
    })
}

/// A level filter that also enables every span at INFO and above.
///
/// The HTTP and gRPC server spans, the job attempt span, and the client
/// spans are INFO. Under a plain filter `log.level = warn` disables them:
/// nothing is exported, and the remaining records lose the request id and
/// trace context those spans carry. A span more verbose than INFO still
/// follows the directive.
struct WithInfoSpans<F>(F);

fn info_span(metadata: &Metadata<'_>) -> bool {
    metadata.is_span() && *metadata.level() <= Level::INFO
}

impl<S: Subscriber, F: Layer<S>> Layer<S> for WithInfoSpans<F> {
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        // `EnvFilter` records its span directives here, so it is asked first.
        let interest = self.0.register_callsite(metadata);
        if info_span(metadata) {
            Interest::always()
        } else {
            interest
        }
    }

    fn enabled(&self, metadata: &Metadata<'_>, ctx: Context<'_, S>) -> bool {
        info_span(metadata) || self.0.enabled(metadata, ctx)
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        self.0
            .max_level_hint()
            .map(|hint| hint.max(LevelFilter::INFO))
    }

    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        self.0.on_new_span(attrs, id, ctx);
    }

    fn on_record(&self, id: &span::Id, values: &span::Record<'_>, ctx: Context<'_, S>) {
        self.0.on_record(id, values, ctx);
    }

    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        self.0.on_enter(id, ctx);
    }

    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        self.0.on_exit(id, ctx);
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        self.0.on_close(id, ctx);
    }
}

/// The directive as `Targets` when that filters exactly like `EnvFilter`.
///
/// `EnvFilter` takes a shared lock on every span creation, enter, exit,
/// record, and close to track span directives (`target[span{field}]`);
/// `Targets` decides once per callsite. `Targets` reads an empty segment
/// (`info,`) as `error` where `EnvFilter` skips it, so such a directive stays
/// with `EnvFilter` too.
fn static_targets(directive: &str) -> Option<Targets> {
    if directive.contains('[') || directive.split(',').any(str::is_empty) {
        return None;
    }
    directive.parse().ok()
}

// template:begin object-storage:telemetry-sdk-log-cap
/// A global filter that caps the AWS SDK's targets.
type SdkCap =
    tracing_subscriber::filter::FilterFn<Box<dyn Fn(&tracing::Metadata<'_>) -> bool + Send + Sync>>;

/// The AWS SDK logs S3 endpoint parameters, which include object keys, at
/// DEBUG and whole requests at TRACE. Its `aws_*` targets stay at INFO or
/// quieter, so a global `debug` never records or exports a key. A target
/// the directive names (`aws_smithy_runtime=debug`) is exempt with
/// everything under it; an unnamed sibling stays capped.
fn sdk_log_cap(level: &str) -> SdkCap {
    let named: Vec<String> = level
        .split(',')
        .filter_map(|directive| {
            let target = directive.split(['=', '[']).next()?.trim();
            target.starts_with("aws_").then(|| target.to_owned())
        })
        .collect();
    tracing_subscriber::filter::filter_fn(Box::new(move |metadata: &tracing::Metadata<'_>| {
        let target = metadata.target();
        *metadata.level() <= tracing::Level::INFO
            || !target.starts_with("aws_")
            || named.iter().any(|name| {
                target
                    .strip_prefix(name.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
            })
    }))
}
// template:end object-storage:telemetry-sdk-log-cap

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "test-only JSON fixtures fail closed with precise local setup context"
    )]

    use super::*;
    use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
    use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
    use std::io;
    use std::sync::{Arc, Mutex};
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
        let subscriber = Registry::default()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")))
            .with(json::JsonLayer::new(buffer.clone()));
        let dispatch = tracing::Dispatch::new(subscriber);

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
        let dispatch =
            tracing::Dispatch::new(Registry::default().with(json::JsonLayer::new(buffer.clone())));

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
                    r#""trace_id":"{trace_id}","span_id":"{span_id}","trace_flags":"{flags}""#,
                    flags = if sampled { "01" } else { "00" }
                )),
                "{name} must carry the trace context: {record}"
            );
        }
    }

    #[test]
    fn json_logs_omit_otel_correlation_without_a_span_or_provider() {
        for (name, with_span) in [("no span", false), ("no provider", true)] {
            let record = emit_event_without_otel_context(with_span);
            assert!(
                !record.contains("\"trace_id\"") && !record.contains("\"span_id\""),
                "{name} must not emit empty or invalid trace ids: {record}"
            );
        }
    }

    #[test]
    fn json_line_sorts_event_fields_then_merged_span_fields() {
        let buffer = Buffer::default();
        let dispatch =
            tracing::Dispatch::new(Registry::default().with(json::JsonLayer::new(buffer.clone())));
        tracing::dispatcher::with_default(&dispatch, || {
            let outer = tracing::info_span!(
                "outer",
                request_id = "r-1",
                shared = "outer",
                name = "overwritten by the span name",
                later = tracing::field::Empty,
                log.target = ?"dropped like a log bridge field",
                r#kind = ?"k",
            );
            let _outer = outer.enter();
            outer.record("later", 7_u64);
            tracing::info_span!("inner", shared = "inner").in_scope(|| {
                tracing::info!(
                    target: "a\"b\n",
                    zeta = 1,
                    alpha = true,
                    ratio = f64::NAN,
                    text = "q\"\n",
                    "done"
                );
            });
        });
        let record = buffer.records();
        let (head, rest) = record
            .split_once(r#""timestamp":""#)
            .expect("the line has a timestamp");
        let (timestamp, tail) = rest.split_once('"').expect("the timestamp is quoted");
        assert_eq!(
            head, r#"{"level":"INFO","target":"a\"b\n","#,
            "level and target come first, the target escaped like any string: {record}"
        );
        assert_eq!(
            tail,
            concat!(
                r##","alpha":true,"message":"done","ratio":null,"text":"q\"\n","zeta":1,"##,
                r##""kind":"\"k\"","later":7,"name":"inner","request_id":"r-1","shared":"inner"}"##,
                "\n"
            ),
            "event fields, then span fields with the nearest value of a repeated key: {record}"
        );
        // RFC 3339 in UTC with microseconds, as tracing-subscriber's timer.
        let shape: String = timestamp
            .chars()
            .map(|c| if c.is_ascii_digit() { '0' } else { c })
            .collect();
        assert_eq!(shape, "0000-00-00T00:00:00.000000Z", "{timestamp}");
    }

    #[test]
    fn json_line_writes_each_key_once() {
        let buffer = Buffer::default();
        let dispatch =
            tracing::Dispatch::new(Registry::default().with(json::JsonLayer::new(buffer.clone())));
        tracing::dispatcher::with_default(&dispatch, || {
            let span = tracing::info_span!(
                "HTTP request",
                request_id = "from-span",
                route = "/items",
                level = "span field named like the line's own key",
            );
            span.in_scope(|| {
                tracing::info!(
                    request_id = "from-event",
                    name = "from-event",
                    timestamp = 1,
                    trace_id = "not a trace id",
                    "http_request"
                );
            });
        });
        let record = buffer.records();
        let line: serde_json::Value = serde_json::from_str(&record).expect("one JSON object");
        assert_eq!(line["request_id"], "from-event", "{record}");
        assert_eq!(line["name"], "from-event", "{record}");
        assert_eq!(line["route"], "/items", "{record}");
        assert_eq!(line["level"], "INFO", "{record}");
        for key in ["level", "timestamp", "trace_id", "request_id", "name"] {
            assert_eq!(
                record.matches(&format!("\"{key}\":")).count(),
                usize::from(key != "trace_id"),
                "{key}: {record}"
            );
        }
    }

    #[test]
    fn json_line_names_the_real_target_of_a_log_crate_record() {
        let buffer = Buffer::default();
        let dispatch =
            tracing::Dispatch::new(Registry::default().with(json::JsonLayer::new(buffer.clone())));
        tracing::dispatcher::with_default(&dispatch, || {
            tracing_log::format_trace(
                &tracing_log::log::Record::builder()
                    .level(tracing_log::log::Level::Warn)
                    .target("rustls::client")
                    .module_path(Some("rustls::client::hs"))
                    .file(Some("hs.rs"))
                    .line(Some(7))
                    .args(format_args!("bridged"))
                    .build(),
            )
            .expect("the record is dispatched");
        });
        let record = buffer.records();
        assert!(
            record.starts_with(r#"{"level":"WARN","target":"rustls::client","timestamp":""#),
            "{record}"
        );
        assert!(
            record.ends_with("Z\",\"message\":\"bridged\"}\n"),
            "{record}"
        );
    }

    #[test]
    fn info_spans_survive_a_quieter_directive() {
        // `Targets` for the first two, `EnvFilter` for the span directive.
        for directive in ["warn", "off", "warn,[never]=trace"] {
            let buffer = Buffer::default();
            let (targets, filter) = level_filter(directive).expect("valid directive");
            let dispatch = tracing::Dispatch::new(
                Registry::default()
                    .with(targets)
                    .with(filter)
                    .with(json::JsonLayer::new(buffer.clone())),
            );
            tracing::dispatcher::with_default(&dispatch, || {
                let request = tracing::info_span!("request", request_id = "r-1");
                assert!(!request.is_disabled(), "{directive}");
                assert!(tracing::debug_span!("detail").is_disabled(), "{directive}");
                request.in_scope(|| {
                    tracing::info!("quiet");
                    tracing::error!("loud");
                });
            });
            let record = buffer.records();
            assert!(!record.contains("quiet"), "{directive}: {record}");
            assert_eq!(
                record.contains(r#""message":"loud","name":"request","request_id":"r-1"}"#),
                directive != "off",
                "{directive}: {record}"
            );
        }
    }

    #[test]
    fn a_verbose_directive_still_enables_verbose_spans() {
        let (targets, filter) = level_filter("info,app=debug").expect("valid directive");
        let dispatch = tracing::Dispatch::new(Registry::default().with(targets).with(filter));
        tracing::dispatcher::with_default(&dispatch, || {
            assert!(!tracing::debug_span!(target: "app", "detail").is_disabled());
            assert!(tracing::debug_span!(target: "other", "detail").is_disabled());
        });
    }

    #[test]
    fn json_line_keeps_the_last_of_many_span_records() {
        let buffer = Buffer::default();
        let dispatch =
            tracing::Dispatch::new(Registry::default().with(json::JsonLayer::new(buffer.clone())));
        tracing::dispatcher::with_default(&dispatch, || {
            let span = tracing::info_span!("job", attempt = 0_u64, kind = "email");
            for attempt in 1..=1000_u64 {
                span.record("attempt", attempt);
            }
            span.in_scope(|| tracing::info!("retrying"));
        });
        let record = buffer.records();
        assert!(
            record
                .trim_end()
                .ends_with(r#""message":"retrying","attempt":1000,"kind":"email","name":"job"}"#),
            "{record}"
        );
    }

    #[test]
    fn only_directives_without_span_filters_or_empty_segments_become_targets() {
        for directive in [
            "info",
            "info,hyper=warn",
            "warn,app=debug",
            "off",
            "app",
            "3",
        ] {
            assert!(static_targets(directive).is_some(), "{directive}");
        }
        for directive in [
            "info,",
            ",",
            "info,,hyper=warn",
            ",info",
            "info,[request]=debug",
        ] {
            assert!(static_targets(directive).is_none(), "{directive}");
        }
    }

    // template:begin object-storage:telemetry-sdk-log-cap-test
    #[test]
    fn sdk_debug_records_stay_out_unless_the_directive_names_them() {
        let emit = |level: &str| {
            let buffer = Buffer::default();
            let filter = EnvFilter::try_new(level).expect("valid directive");
            let layer = tracing_subscriber::fmt::layer()
                .with_writer(buffer.clone())
                .with_ansi(false);
            let dispatch = tracing::Dispatch::new(
                Registry::default()
                    .with(filter)
                    .with(sdk_log_cap(level))
                    .with(layer),
            );
            tracing::dispatcher::with_default(&dispatch, || {
                tracing::debug!(target: "aws_smithy_runtime::client::orchestrator::endpoints", "sdk debug with key");
                tracing::info!(target: "aws_smithy_runtime::client", "sdk info");
                tracing::debug!(target: "service::feature", "service debug");
            });
            buffer.records()
        };
        let capped = emit("debug");
        assert!(!capped.contains("sdk debug with key"), "{capped}");
        assert!(capped.contains("sdk info"), "{capped}");
        assert!(capped.contains("service debug"), "{capped}");
        let named = emit("debug,aws_smithy_runtime=debug");
        assert!(named.contains("sdk debug with key"), "{named}");
        let sibling = emit("debug,aws_config=warn");
        assert!(!sibling.contains("sdk debug with key"), "{sibling}");
        let quiet = emit("warn");
        assert!(!quiet.contains("sdk info"), "{quiet}");
    }
    // template:end object-storage:telemetry-sdk-log-cap-test
}
