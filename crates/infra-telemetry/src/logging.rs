//! The process logger.
//!
//! One subscriber per process: a filter from the typed level directive
//! (`EnvFilter` grammar), a JSON or text formatting layer, and, when a tracer provider
//! exists, the OpenTelemetry layer that gives every span an OpenTelemetry context so
//! JSON records carry `trace_id` and `span_id`. `log` records are bridged by
//! `tracing-log`'s `LogTracer`, installed before the subscriber.
//!
//! The directive chooses the records. Spans at INFO and above exist under
//! every directive, so a quieter `log.level` neither stops trace export nor
//! strips the request fields and trace context from the records that remain.

mod format;
mod output;
mod sdk_diagnostics;

pub use output::{
    DropReason, LogSnapshot, LoggerGuard, LoggerShutdown, LoggerShutdownReasons, SinkOperation,
};

use crate::traces::TracerProviderHandle;
use tracing::level_filters::LevelFilter;
use tracing::subscriber::{Interest, NoSubscriber};
use tracing::{Event, Level, Metadata, Subscriber, span};
use tracing_log::{AsLog as _, NormalizeEvent as _};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{EnvFilter, Layer, Registry};

/// Output format for the process logger, distinct from the configuration
/// snapshot's `json` / `text` wire enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoggingFormat {
    Json,
    Text,
}

/// Options for installing the global subscriber.
#[derive(Debug)]
pub struct LoggingOptions<'a> {
    /// An `EnvFilter` directive such as `info` or `info,sqlx=warn`.
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
    #[error("a global `log` logger is already installed")]
    LogLoggerInstalled,
    #[error("start the logging writer thread: {0}")]
    WorkerStart(#[source] std::io::Error),
}

/// Installs the process's global subscriber and starts its writer thread.
///
/// # Errors
///
/// Returns [`LoggingError::Directive`] for an unparsable directive,
/// [`LoggingError::WorkerStart`] when the writer thread cannot start,
/// [`LoggingError::AlreadyInstalled`] when a global subscriber exists (seen
/// as the current default, so call it outside a scoped default), and
/// [`LoggingError::LogLoggerInstalled`] when a `log` logger exists; on an
/// error nothing is installed.
#[expect(
    clippy::disallowed_methods,
    reason = "startup transfers the stdout handle to the owned writer thread; callbacks perform no sink IO"
)]
pub fn install_subscriber(options: &LoggingOptions<'_>) -> Result<LoggerGuard, LoggingError> {
    let (targets, filter) = directive_filters(options.level)?;
    // `has_been_set` stays true after a scoped default is dropped; outside a
    // scope the current default is the global one, `NoSubscriber` until set.
    if tracing::dispatcher::has_been_set()
        && tracing::dispatcher::get_default(|current| !current.is::<NoSubscriber>())
    {
        return Err(LoggingError::AlreadyInstalled);
    }
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
    let (output, guard) = output::start(std::io::stdout()).map_err(LoggingError::WorkerStart)?;
    let format = format::FormatLayer::new(output, options.format);
    let registry = Registry::default();
    #[cfg(feature = "hotpath")]
    let registry = registry.with(hotpath::sqlx_tracing_layer());
    let subscriber = registry
        .with(targets)
        .with(filter)
        // template:begin object-storage:telemetry-sdk-log-cap-apply
        .with(sdk_log_cap(options.level))
        // template:end object-storage:telemetry-sdk-log-cap-apply
        .with(otel)
        .with(format);
    // `try_init` sets the subscriber before the `log` bridge, so a taken
    // `log` slot would leave a subscriber whose writer this early return
    // closes. The bridge goes first; until the subscriber follows, bridged
    // records reach no subscriber, as before any installation.
    let max_level = subscriber.max_level_hint().unwrap_or(LevelFilter::TRACE);
    tracing_log::LogTracer::builder()
        .with_max_level(max_level.as_log())
        .init()
        .map_err(|_| LoggingError::LogLoggerInstalled)?;
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|_| LoggingError::AlreadyInstalled)?;
    Ok(guard)
}

/// Replace Rust's hook with a payload-free error event and build-authored location.
/// Runtime thread names and backtraces are withheld.
///
/// Install after the subscriber: the hook writes only through `tracing`, so
/// a panic before a subscriber exists is lost instead of reaching stderr.
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

pub(crate) fn describe_metrics() {
    output::describe_metrics();
    sdk_diagnostics::describe_metrics();
}

pub(crate) fn publish_observations() {
    output::publish();
    sdk_diagnostics::publish();
}

/// The directive's filter with the spans every directive keeps: `Targets`
/// when that is exact, `EnvFilter` otherwise.
type DirectiveFilters = (
    Option<WithInfoSpans<Targets>>,
    Option<WithInfoSpans<EnvFilter>>,
);

fn directive_filters(directive: &str) -> Result<DirectiveFilters, LoggingError> {
    let filter = EnvFilter::try_new(directive).map_err(|source| LoggingError::Directive {
        directive: directive.to_owned(),
        source,
    })?;
    Ok(match static_targets(directive) {
        Some(targets) => (Some(WithInfoSpans(targets)), None),
        None => (None, Some(WithInfoSpans(filter))),
    })
}

/// The directive's filter under the process-wide [`Admission`] rules: SDK
/// diagnostic targets are refused, except known diagnostics, which
/// [`Layer::event_enabled`] counts through [`sdk_diagnostics::observe`] as
/// their only output; `log` bridge records are decided per event; and every
/// span at INFO and above is enabled.
///
/// The HTTP and gRPC server spans, the job attempt span, and the client
/// spans are INFO. Under a plain filter `log.level = warn` disables them:
/// nothing is exported, and the remaining records lose the request id and
/// trace context those spans carry. A span more verbose than INFO still
/// follows the directive.
struct WithInfoSpans<F>(F);

fn is_span_at_info_or_above(metadata: &Metadata<'_>) -> bool {
    metadata.is_span() && *metadata.level() <= Level::INFO
}

/// How [`WithInfoSpans`] treats a callsite, in rule order.
enum Admission {
    Always,
    Never,
    /// Decided per event in [`Layer::event_enabled`].
    Deferred,
    /// The directive decides.
    Directive,
}

fn admission(metadata: &Metadata<'_>) -> Admission {
    if sdk_diagnostics::is_denied_target(metadata.target()) {
        if sdk_diagnostics::is_known_diagnostic(metadata) {
            Admission::Always
        } else {
            Admission::Never
        }
    } else if metadata.target() == "log" {
        // Bridged origin lives in fields; decide before either output in event_enabled.
        Admission::Deferred
    } else if is_span_at_info_or_above(metadata) {
        Admission::Always
    } else {
        Admission::Directive
    }
}

impl<S: Subscriber, F: Layer<S>> Layer<S> for WithInfoSpans<F> {
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        // `EnvFilter` records its span directives here, so it is asked first.
        let interest = self.0.register_callsite(metadata);
        match admission(metadata) {
            Admission::Always => Interest::always(),
            Admission::Never => Interest::never(),
            Admission::Deferred => Interest::sometimes(),
            Admission::Directive => interest,
        }
    }

    fn enabled(&self, metadata: &Metadata<'_>, ctx: Context<'_, S>) -> bool {
        match admission(metadata) {
            Admission::Always | Admission::Deferred => true,
            Admission::Never => false,
            Admission::Directive => self.0.enabled(metadata, ctx),
        }
    }

    fn event_enabled(&self, event: &Event<'_>, ctx: Context<'_, S>) -> bool {
        let normalized = event.normalized_metadata();
        let metadata = normalized.as_ref().unwrap_or_else(|| event.metadata());
        if sdk_diagnostics::is_denied_target(metadata.target()) {
            sdk_diagnostics::observe(event, metadata);
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
/// (`info,`) as `error` where `EnvFilter` skips it, and an empty level
/// (`app=`) as `error` where `EnvFilter` reads `trace`, so such a directive
/// stays with `EnvFilter` too.
fn static_targets(directive: &str) -> Option<Targets> {
    if directive.contains('[')
        || directive
            .split(',')
            .any(|segment| segment.is_empty() || segment.ends_with('='))
    {
        return None;
    }
    directive.parse().ok()
}

/// Whether `target` is `root` or a module under it.
fn within(target: &str, root: &str) -> bool {
    target
        .strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
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
fn sdk_log_cap(directive: &str) -> SdkCap {
    let named: Vec<String> = directive
        .split(',')
        .filter_map(|segment| {
            let target = segment.split(['=', '[']).next()?.trim();
            target.starts_with("aws_").then(|| target.to_owned())
        })
        .collect();
    tracing_subscriber::filter::filter_fn(Box::new(move |metadata: &tracing::Metadata<'_>| {
        let target = metadata.target();
        *metadata.level() <= tracing::Level::INFO
            || !target.starts_with("aws_")
            || named.iter().any(|name| within(target, name))
    }))
}
// template:end object-storage:telemetry-sdk-log-cap

#[cfg(test)]
pub(crate) mod tests;
