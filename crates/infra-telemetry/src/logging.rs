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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoggingFormat {
    Json,
    Text,
}

pub struct LoggingOptions<'a> {
    pub level: &'a str,
    pub format: LoggingFormat,
    pub tracer_provider: Option<&'a TracerProviderHandle>,
}

impl std::fmt::Debug for LoggingOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoggingOptions")
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("log.level is not a valid tracing filter directive")]
    Directive,
    #[error("a global tracing subscriber is already installed")]
    AlreadyInstalled,
    #[error("a global `log` logger is already installed")]
    LogLoggerInstalled,
    #[error("start the logging writer thread: {0}")]
    WorkerStart(#[source] std::io::Error),
}

#[expect(
    clippy::disallowed_methods,
    reason = "startup transfers the stdout handle to the owned writer thread; callbacks perform no sink IO"
)]
pub fn install_subscriber(options: &LoggingOptions<'_>) -> Result<LoggerGuard, LoggingError> {
    let (targets, filter) = directive_filters(options.level)?;
    if tracing::dispatcher::has_been_set()
        && tracing::dispatcher::get_default(|current| !current.is::<NoSubscriber>())
    {
        return Err(LoggingError::AlreadyInstalled);
    }
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
    let max_level = subscriber.max_level_hint().unwrap_or(LevelFilter::TRACE);
    tracing_log::LogTracer::builder()
        .with_max_level(max_level.as_log())
        .init()
        .map_err(|_| LoggingError::LogLoggerInstalled)?;
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|_| LoggingError::AlreadyInstalled)?;
    Ok(guard)
}

pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
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

type DirectiveFilters = (
    Option<WithInfoSpans<Targets>>,
    Option<WithInfoSpans<EnvFilter>>,
);

fn directive_filters(directive: &str) -> Result<DirectiveFilters, LoggingError> {
    let filter = EnvFilter::try_new(directive).map_err(|_| LoggingError::Directive)?;
    Ok(match static_targets(directive) {
        Some(targets) => (Some(WithInfoSpans(targets)), None),
        None => (None, Some(WithInfoSpans(filter))),
    })
}

struct WithInfoSpans<F>(F);

fn is_span_at_info_or_above(metadata: &Metadata<'_>) -> bool {
    metadata.is_span() && *metadata.level() <= Level::INFO
}

enum Admission {
    Always,
    Never,
    Deferred,
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
        Admission::Deferred
    } else if is_span_at_info_or_above(metadata) {
        Admission::Always
    } else {
        Admission::Directive
    }
}

impl<S: Subscriber, F: Layer<S>> Layer<S> for WithInfoSpans<F> {
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
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

fn within(target: &str, root: &str) -> bool {
    target
        .strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
}

// template:begin object-storage:telemetry-sdk-log-cap
fn sdk_log_cap<S: Subscriber>(directive: &str) -> impl Layer<S> + use<S> {
    let named: Vec<String> = directive
        .split(',')
        .filter_map(|segment| {
            let target = segment.split(['=', '[']).next()?.trim();
            target.starts_with("aws_").then(|| target.to_owned())
        })
        .collect();
    tracing_subscriber::filter::filter_fn(move |metadata: &Metadata<'_>| {
        let target = metadata.target();
        *metadata.level() <= tracing::Level::INFO
            || !target.starts_with("aws_")
            || named.iter().any(|name| within(target, name))
    })
}
// template:end object-storage:telemetry-sdk-log-cap

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "synchronous fixture polling waits for owned child completion within its existing timeout"
)]
pub(super) fn wait_for_child(
    mut child: std::process::Child,
    timeout: std::time::Duration,
) -> std::process::Output {
    let deadline = std::time::Instant::now() + timeout;
    while child.try_wait().expect("poll test child").is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("test child exceeded its bounded completion wait");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    child.wait_with_output().expect("collect test child")
}

#[cfg(test)]
pub(crate) mod tests;
