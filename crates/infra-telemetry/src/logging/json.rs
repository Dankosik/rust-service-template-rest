//! The JSON log line.
//!
//! One object per line: `level`, `target`, `timestamp`, the OpenTelemetry ids
//! of the event's span, the event's fields sorted by key, then the fields of
//! every span in the event's scope sorted by key, where a key repeated by a
//! nested span keeps the value nearest the event and each span's name is its
//! `name` field. This is the line `json-subscriber` 0.3 wrote with
//! `flatten_event` and `flatten_span_list_on_top_level`, byte for byte. That
//! crate built a JSON value map for the event and another for the span list
//! on every line and re-serialized all of a span's fields on every record;
//! here a span keeps its values serialized once, and a line is written into
//! a reused buffer.

use std::cell::RefCell;
use std::fmt::{self, Write as _};
use std::io::{self, Write as _};
use std::ops::Range;
use std::sync::OnceLock;

use opentelemetry::trace::TraceContextExt as _;
use tracing::dispatcher::WeakDispatch;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Dispatch, Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

pub(crate) struct JsonLayer<W = fn() -> io::Stdout> {
    make_writer: W,
    /// The dispatch this layer belongs to, for the OpenTelemetry context of
    /// a span; weak, so the layer does not keep its own subscriber alive.
    dispatch: OnceLock<WeakDispatch>,
}

impl<W> JsonLayer<W> {
    pub(crate) fn new(make_writer: W) -> Self {
        Self {
            make_writer,
            dispatch: OnceLock::new(),
        }
    }
}

/// A span's fields, each value already serialized as JSON.
#[derive(Default)]
struct SpanFields {
    values: Vec<u8>,
    fields: Vec<(&'static str, Range<usize>)>,
}

impl SpanFields {
    fn set(&mut self, key: &'static str, value: &(impl serde::Serialize + ?Sized)) {
        let start = self.values.len();
        write_json(&mut self.values, value);
        let range = start..self.values.len();
        match self.fields.iter_mut().find(|(name, _)| *name == key) {
            Some((_, old)) => {
                *old = range;
                self.compact();
            }
            None => self.fields.push((key, range)),
        }
    }

    /// A span recorded many times keeps only its current values.
    fn compact(&mut self) {
        let live: usize = self.fields.iter().map(|(_, range)| range.len()).sum();
        if self.values.len() <= 2 * live + 256 {
            return;
        }
        let mut values = Vec::with_capacity(live);
        for (_, range) in &mut self.fields {
            let start = values.len();
            values.extend_from_slice(&self.values[range.clone()]);
            *range = start..values.len();
        }
        self.values = values;
    }
}

/// Span values as `json-subscriber` recorded them: a debug value is a
/// string, the `log` bridge's `log.*` debug fields are dropped, and a raw
/// identifier prefix is removed from a debug field's name.
struct SpanVisitor<'a>(&'a mut SpanFields);

impl Visit for SpanVisitor<'_> {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.set(field.name(), &value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.set(field.name(), &value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.set(field.name(), &value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.set(field.name(), &value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.set(field.name(), value);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let name = field.name();
        if name.starts_with("log.") {
            return;
        }
        let name = name.strip_prefix("r#").unwrap_or(name);
        self.0.set(name, &format_args!("{value:?}"));
    }
}

/// Event values as `tracing-serde` serialized them: every field under its
/// own name, a debug value as a string.
struct EventVisitor<'a> {
    values: &'a mut Vec<u8>,
    fields: &'a mut Vec<(&'static str, Range<usize>)>,
}

impl EventVisitor<'_> {
    fn push(&mut self, field: &Field, value: &(impl serde::Serialize + ?Sized)) {
        let start = self.values.len();
        write_json(self.values, value);
        self.fields.push((field.name(), start..self.values.len()));
    }
}

impl Visit for EventVisitor<'_> {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push(field, &value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, &value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, &value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, &value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, &format_args!("{value:?}"));
    }
}

/// Per-thread buffers for one line, kept between events.
#[derive(Default)]
struct Scratch {
    line: Vec<u8>,
    values: Vec<u8>,
    fields: Vec<(&'static str, Range<usize>)>,
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::default();
}

impl<W> JsonLayer<W> {
    fn format<S>(&self, event: &Event<'_>, ctx: &Context<'_, S>, scratch: &mut Scratch)
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let Scratch {
            line,
            values,
            fields,
        } = scratch;
        // A value's Debug that panicked mid-line left its bytes behind.
        line.clear();
        values.clear();
        fields.clear();
        let metadata = event.metadata();
        line.extend_from_slice(b"{\"level\":\"");
        line.extend_from_slice(metadata.level().as_str().as_bytes());
        line.extend_from_slice(b"\",\"target\":");
        write_escaped(line, metadata.target());
        line.extend_from_slice(b",\"timestamp\":\"");
        write_timestamp(line);
        line.push(b'"');

        let span = ctx.event_span(event);
        if let Some(span) = &span {
            self.write_trace_ids(&span.id(), line);
        }

        event.record(&mut EventVisitor { values, fields });
        write_sorted(line, values, fields);

        if let Some(span) = span {
            for span in span.scope().from_root() {
                let extensions = span.extensions();
                let Some(span_fields) = extensions.get::<SpanFields>() else {
                    continue;
                };
                for (key, range) in &span_fields.fields {
                    let start = values.len();
                    values.extend_from_slice(&span_fields.values[range.clone()]);
                    fields.push((*key, start..values.len()));
                }
            }
            write_sorted(line, values, fields);
        }
        line.extend_from_slice(b"}\n");
    }

    fn write_trace_ids(&self, id: &Id, line: &mut Vec<u8>) {
        let Some(dispatch) = self.dispatch.get().and_then(WeakDispatch::upgrade) else {
            return;
        };
        let Some(context) = tracing_opentelemetry::get_otel_context(id, &dispatch) else {
            return;
        };
        let span = context.span();
        let ids = span.span_context();
        line.extend_from_slice(b",\"openTelemetry\":{\"spanId\":\"");
        write_hex(line, &ids.span_id().to_bytes());
        line.extend_from_slice(b"\",\"traceId\":\"");
        write_hex(line, &ids.trace_id().to_bytes());
        line.extend_from_slice(b"\"}");
    }
}

impl<S, W> Layer<S> for JsonLayer<W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + 'static,
{
    fn on_register_dispatch(&self, subscriber: &Dispatch) {
        let _ = self.dispatch.set(subscriber.downgrade());
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut fields = SpanFields::default();
        attrs.record(&mut SpanVisitor(&mut fields));
        fields.set("name", attrs.metadata().name());
        span.extensions_mut().replace(fields);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        if let Some(fields) = span.extensions_mut().get_mut::<SpanFields>() {
            values.record(&mut SpanVisitor(fields));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let write = |scratch: &mut Scratch| {
            self.format(event, &ctx, scratch);
            let mut writer = self.make_writer.make_writer_for(event.metadata());
            let _ = writer.write_all(&scratch.line);
            scratch.line.clear();
        };
        // A value's Debug may log; that nested event gets its own buffers.
        let reused = SCRATCH.try_with(|cell| match cell.try_borrow_mut() {
            Ok(mut scratch) => {
                write(&mut scratch);
                true
            }
            Err(_) => false,
        });
        if !matches!(reused, Ok(true)) {
            write(&mut Scratch::default());
        }
    }
}

/// Sort by key and keep the last value of a repeated key, as a map would.
fn write_sorted(
    line: &mut Vec<u8>,
    values: &mut Vec<u8>,
    fields: &mut Vec<(&'static str, Range<usize>)>,
) {
    fields.sort_by_key(|(key, _)| *key);
    for (index, (key, range)) in fields.iter().enumerate() {
        if fields.get(index + 1).is_some_and(|(next, _)| next == key) {
            continue;
        }
        line.push(b',');
        write_json(line, *key);
        line.push(b':');
        line.extend_from_slice(&values[range.clone()]);
    }
    fields.clear();
    values.clear();
}

/// The instant as `tracing-subscriber`'s `SystemTime` timer prints it,
/// RFC 3339 with microseconds in UTC. `humantime` fills one buffer where the
/// timer makes a `write!` call per field; it panics before the epoch and
/// refuses years past 9999, which the timer still prints.
fn write_timestamp(line: &mut Vec<u8>) {
    let now = std::time::SystemTime::now();
    if now >= std::time::UNIX_EPOCH
        && write!(Utf8(line), "{}", humantime::format_rfc3339_micros(now)).is_ok()
    {
        return;
    }
    let _ = SystemTime.format_time(&mut Writer::new(&mut Utf8(line)));
}

fn write_json(out: &mut Vec<u8>, value: &(impl serde::Serialize + ?Sized)) {
    let start = out.len();
    if serde_json::to_writer(&mut *out, value).is_err() {
        out.truncate(start);
        out.extend_from_slice(b"null");
    }
}

/// `json-subscriber`'s quoting for the target: only `"` and `\` are escaped.
fn write_escaped(out: &mut Vec<u8>, value: &str) {
    out.push(b'"');
    for &byte in value.as_bytes() {
        if byte == b'"' || byte == b'\\' {
            out.push(b'\\');
        }
        out.push(byte);
    }
    out.push(b'"');
}

fn write_hex(out: &mut Vec<u8>, bytes: &[u8]) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)]);
        out.push(DIGITS[usize::from(byte & 0x0f)]);
    }
}

struct Utf8<'a>(&'a mut Vec<u8>);

impl fmt::Write for Utf8<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}
