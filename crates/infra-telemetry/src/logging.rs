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

mod diagnostics;
mod format;
mod output;

pub use output::{LogSnapshot, LoggerGuard, LoggerIncomplete, LoggerShutdown};

use crate::traces::TracerProviderHandle;
use tracing::level_filters::LevelFilter;
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber, span};
use tracing_log::NormalizeEvent as _;
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
    #[error("the local logging worker could not start")]
    WorkerStart,
}

/// Install the global subscriber.
///
/// # Errors
///
/// Returns an error for an unparsable directive or a second installation in
/// the same process.
#[allow(
    clippy::disallowed_methods,
    reason = "startup transfers the stdout handle to the owned writer thread; callbacks perform no sink IO"
)]
pub fn install_subscriber(options: &LoggingOptions<'_>) -> Result<LoggerGuard, LoggingError> {
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
    let (output, guard) =
        output::start(std::io::stdout()).map_err(|_| LoggingError::WorkerStart)?;
    let format = format::FormatLayer::new(output, options.format);
    let registry = Registry::default();
    #[cfg(feature = "hotpath")]
    let registry = registry.with(hotpath::sqlx_tracing_layer());
    registry
        .with(targets)
        .with(filter)
        // template:begin object-storage:telemetry-sdk-log-cap-apply
        .with(sdk_log_cap(options.level))
        // template:end object-storage:telemetry-sdk-log-cap-apply
        .with(otel)
        .with(format)
        .try_init()
        .map_err(|_| LoggingError::AlreadyInstalled)?;
    Ok(guard)
}

/// Replace Rust's hook with a payload-free error event and build-authored location.
/// Install after the subscriber. Runtime thread names and backtraces are withheld.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        // The writer reports its own failure through independent counters;
        // sending its panic back into the same output would recurse.
        if output::on_writer_thread() {
            return;
        }
        let location = info.location();
        tracing::error!(
            panic.file = location.map(std::panic::Location::file),
            panic.line = location.map(std::panic::Location::line),
            panic.column = location.map(std::panic::Location::column),
            "panicked"
        );
    }));
}

pub(crate) fn publish_observations() {
    output::publish();
    diagnostics::publish();
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
        if diagnostics::denied(metadata.target()) {
            if diagnostics::observed(metadata) {
                Interest::always()
            } else {
                Interest::never()
            }
        } else if metadata.target() == "log" {
            // Bridged origin lives in fields; decide before either output in event_enabled.
            Interest::sometimes()
        } else if info_span(metadata) {
            Interest::always()
        } else {
            interest
        }
    }

    fn enabled(&self, metadata: &Metadata<'_>, ctx: Context<'_, S>) -> bool {
        if diagnostics::denied(metadata.target()) {
            diagnostics::observed(metadata)
        } else {
            metadata.target() == "log" || info_span(metadata) || self.0.enabled(metadata, ctx)
        }
    }

    fn event_enabled(&self, event: &Event<'_>, ctx: Context<'_, S>) -> bool {
        let normalized = event.normalized_metadata();
        let metadata = normalized.as_ref().unwrap_or_else(|| event.metadata());
        if diagnostics::denied(metadata.target()) {
            diagnostics::observe(event, metadata);
            return false;
        }
        self.0.enabled(metadata, ctx.clone()) && self.0.event_enabled(event, ctx)
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        // Known SDK numeric facts remain observable even with log.level=off.
        Some(LevelFilter::TRACE)
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
pub(crate) mod tests {
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

    #[derive(Clone, Default)]
    pub(crate) struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Buffer {
        pub(crate) fn records(&self) -> String {
            String::from_utf8(self.0.lock().expect("test writer mutex").clone())
                .expect("json subscriber only writes UTF-8")
        }
    }

    impl io::Write for Buffer {
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

    impl Buffer {
        fn layer(&self, format: LoggingFormat) -> (format::FormatLayer, LoggerGuard) {
            let (output, guard) = output::start(self.clone()).expect("start the test writer");
            (format::FormatLayer::new(output, format), guard)
        }
    }

    pub(crate) fn capture(
        format: LoggingFormat,
        level: &str,
        provider: Option<&TracerProviderHandle>,
    ) -> (tracing::Dispatch, LoggerGuard, Buffer) {
        let buffer = Buffer::default();
        let (layer, guard) = buffer.layer(format);
        let (targets, filter) = level_filter(level).expect("valid capture filter");
        let otel =
            provider.map(|handle| tracing_opentelemetry::layer().with_tracer(handle.tracer()));
        let registry = Registry::default().with(targets).with(filter);
        (
            tracing::Dispatch::new(registry.with(otel).with(layer)),
            guard,
            buffer,
        )
    }

    fn drain(guard: LoggerGuard) {
        assert!(matches!(
            guard.shutdown(std::time::Instant::now() + std::time::Duration::from_secs(2)),
            LoggerShutdown::Completed(_)
        ));
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
        let (layer, guard) = buffer.layer(LoggingFormat::Json);
        let provider = SdkTracerProvider::builder()
            .with_sampler(match sampling {
                Sampling::Sampled => Sampler::AlwaysOn,
                Sampling::NotSampled => Sampler::AlwaysOff,
            })
            .build();
        let subscriber = Registry::default()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")))
            .with(layer);
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

        drain(guard);
        (buffer.records(), trace_id, span_id, sampled)
    }

    fn emit_event_without_otel_context(with_span: bool) -> String {
        let buffer = Buffer::default();
        let (layer, guard) = buffer.layer(LoggingFormat::Json);
        let dispatch = tracing::Dispatch::new(Registry::default().with(layer));

        tracing::dispatcher::with_default(&dispatch, || {
            if with_span {
                tracing::info_span!("plain_parent").in_scope(|| tracing::info!("plain probe"));
            } else {
                tracing::info!("plain probe");
            }
        });

        drain(guard);
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
        let (layer, guard) = buffer.layer(LoggingFormat::Json);
        let dispatch = tracing::Dispatch::new(Registry::default().with(layer));
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
        drain(guard);
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
        let (layer, guard) = buffer.layer(LoggingFormat::Json);
        let dispatch = tracing::Dispatch::new(Registry::default().with(layer));
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
        drain(guard);
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
        let (layer, guard) = buffer.layer(LoggingFormat::Json);
        let dispatch = tracing::Dispatch::new(Registry::default().with(layer));
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
        drain(guard);
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
            let (layer, guard) = buffer.layer(LoggingFormat::Json);
            let (targets, filter) = level_filter(directive).expect("valid directive");
            let dispatch =
                tracing::Dispatch::new(Registry::default().with(targets).with(filter).with(layer));
            tracing::dispatcher::with_default(&dispatch, || {
                let request = tracing::info_span!("request", request_id = "r-1");
                assert!(!request.is_disabled(), "{directive}");
                assert!(tracing::debug_span!("detail").is_disabled(), "{directive}");
                request.in_scope(|| {
                    tracing::info!("quiet");
                    tracing::error!("loud");
                });
            });
            drain(guard);
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
        let (layer, guard) = buffer.layer(LoggingFormat::Json);
        let dispatch = tracing::Dispatch::new(Registry::default().with(layer));
        tracing::dispatcher::with_default(&dispatch, || {
            let span = tracing::info_span!("job", attempt = 0_u64, kind = "email");
            for attempt in 1..=1000_u64 {
                span.record("attempt", attempt);
            }
            span.in_scope(|| tracing::info!("retrying"));
        });
        drain(guard);
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

    // Process isolation gives the cumulative SDK observations a fresh lifetime;
    // another exporter fixture must not supply the numeric facts asserted here.
    #[allow(
        clippy::disallowed_methods,
        reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
    )]
    fn in_diagnostic_child() -> bool {
        const CHILD: &str = "TELEMETRY_DIAGNOSTIC_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let mut child =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args([
                        "--exact",
                        "logging::tests::sdk_diagnostics_keep_numeric_facts_without_raw_output",
                        "--nocapture",
                    ])
                    .env(CHILD, "1")
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .expect("run the isolated diagnostic observation");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while child.try_wait().expect("poll diagnostic child").is_none() {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("diagnostic child exceeded its bounded completion wait");
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let result = child.wait_with_output().expect("collect diagnostic child");
            assert!(
                result.status.success(),
                "child output: {} {}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return false;
        }
        true
    }

    struct NeverFormat;
    impl std::fmt::Debug for NeverFormat {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            panic!("denied diagnostic Debug was invoked")
        }
    }

    fn emit_denied_diagnostics() {
        tracing::error!("ordinary_record");
        tracing::debug!(name: "HttpTraceClient.ResponseParseError", target: "opentelemetry-otlp", error = ?NeverFormat);
        tracing::trace!(target: "reqwest::connect", "SDK_SECRET_SENTINEL");
        tracing::warn!(target: "opentelemetry_sdk", "SDK_SECRET_SENTINEL");
        assert!(
            tracing::info_span!(target: "tracing_opentelemetry", "SDK_SECRET_SENTINEL")
                .is_disabled()
        );
        for target in [
            "rustls::client",
            "hyper_util::client",
            "opentelemetry-http",
            "h2::codec",
            "rustls_platform_verifier",
        ] {
            tracing_log::format_trace(
                &tracing_log::log::Record::builder()
                    .target(target)
                    .level(tracing_log::log::Level::Error)
                    .args(format_args!("SDK_SECRET_SENTINEL"))
                    .build(),
            )
            .expect("bridge diagnostic");
        }
    }

    #[test]
    fn sdk_diagnostics_keep_numeric_facts_without_raw_output() {
        if !in_diagnostic_child() {
            return;
        }
        for format in [LoggingFormat::Json, LoggingFormat::Text] {
            for level in ["debug", "trace", "off", "off,[request]=trace"] {
                let (dispatch, guard, buffer) = capture(format, level, None);
                tracing::dispatcher::with_default(&dispatch, || {
                    let span = tracing::info_span!("request", request_id = "safe-request");
                    assert!(!span.is_disabled());
                    span.in_scope(emit_denied_diagnostics);
                });
                drain(guard);
                let records = buffer.records();
                assert!(!records.contains("SDK_SECRET_SENTINEL"), "{records}");
                assert!(!records.contains("ResponseParseError"), "{records}");
                if level != "off" && !level.starts_with("off,") {
                    assert!(
                        records.contains("ordinary_record") && records.contains("safe-request"),
                        "{records}"
                    );
                }
            }
        }

        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            let (dispatch, guard, _) = capture(LoggingFormat::Json, "off", None);
            tracing::dispatcher::with_default(&dispatch, || {
                tracing::warn!(name: "HttpTraceClient.PartialSuccess", target: "opentelemetry-otlp", error_message = "SDK_SECRET_SENTINEL");
                tracing::warn!(name: "BatchSpanProcessor.SpansDropped", target: "opentelemetry_sdk", dropped_span_count = ?NeverFormat);
            });
            publish_observations();
            let absent = handle.render();
            assert!(
                !absent
                    .lines()
                    .any(|line| line.starts_with("telemetry_sdk_queue_dropped_spans "))
            );
            assert!(
                !absent
                    .lines()
                    .any(|line| line.starts_with("telemetry_sdk_reported_rejected_spans_total "))
            );
            tracing::dispatcher::with_default(&dispatch, || {
                tracing::debug!(name: "BatchSpanProcessor.SpanDroppingStarted", target: "opentelemetry_sdk", message = "SDK_SECRET_SENTINEL");
                for _ in 0..2 {
                    tracing::warn!(name: "BatchSpanProcessor.SpansDropped", target: "opentelemetry_sdk", dropped_span_count = 19_u64);
                }
                tracing::warn!(name: "HttpTraceClient.PartialSuccess", target: "opentelemetry-otlp", rejected_spans = 3_i64, error_message = "SDK_SECRET_SENTINEL");
                tracing::warn!(name: "HttpTraceClient.PartialSuccess", target: "opentelemetry-otlp", rejected_spans = -1_i64);
                tracing::debug!(name: "HttpClient.StatusError", target: "opentelemetry-otlp", status_code = 400_u64, url = ?NeverFormat);
            });
            publish_observations();
            publish_observations();
            let observed = handle.render();
            assert!(
                observed.contains("telemetry_sdk_queue_dropped_spans 19\n"),
                "{observed}"
            );
            assert!(
                observed.contains("telemetry_sdk_reported_rejected_spans_total 3\n"),
                "{observed}"
            );
            assert!(
                observed.contains(
                    "telemetry_sdk_diagnostics_total{event=\"response_parse_error\"} 8\n"
                ),
                "{observed}"
            );
            assert!(
                observed
                    .contains("telemetry_sdk_diagnostics_total{event=\"queue_spans_dropped\"} 3\n"),
                "{observed}"
            );
            assert!(!observed.contains("SDK_SECRET_SENTINEL"));
            drain(guard);
        });
    }

    // template:begin object-storage:telemetry-sdk-log-cap-test
    #[test]
    fn sdk_debug_records_stay_out_unless_the_directive_names_them() {
        let emit = |level: &str| {
            let buffer = Buffer::default();
            let (layer, guard) = buffer.layer(LoggingFormat::Text);
            let filter = EnvFilter::try_new(level).expect("valid directive");
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
            drain(guard);
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
