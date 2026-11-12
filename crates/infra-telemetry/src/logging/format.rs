//! Bounded JSON/text capture. Only complete records reach the output owner.
//!
//! Event fields precede sorted span fields; event values win over spans and
//! the nearest span wins over its ancestors. Reserved keys remain unique.
//!
//! A record that does not fit its 16 KiB is cut, not dropped: a string value
//! is cut at a character boundary, any other field that no longer fits is left
//! out with every later one, and the record carries `truncated=true`. So does
//! an event whose span has no cached fields, which is written without them.
//!
//! Custom `Debug`/`Display` callbacks must be finite, trusted and nonblocking.
//! Emitters own source size, collection cardinality and concurrent work within
//! their operation deadline. Bounded output cannot preempt a callback or bound
//! its private allocations; borrowed strings are refused before avoidable copying.

use std::cell::Cell;
use std::fmt::{self, Write as _};
use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use arrayvec::ArrayVec;
use opentelemetry::trace::TraceContextExt as _;
use tracing::dispatcher::WeakDispatch;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record as SpanRecord};
use tracing::{Dispatch, Event, Subscriber};
use tracing_log::NormalizeEvent as _;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use super::LoggingFormat;
use super::output::{DropReason, MAX_RECORD_BYTES, Output, Record};

/// First allocation of a value buffer and an output line; both grow to their limit.
const SCRATCH_CAPACITY: usize = 256;
const LINE_CAPACITY: usize = 512;
const MAX_EVENT_FIELDS: usize = 128;
const MAX_SPAN_BYTES: usize = 4096;
const MAX_SPAN_FIELDS: usize = 64;
/// Callbacks formatting at once across threads; one more is dropped as `busy`.
const MAX_ACTIVE_CALLBACKS: usize = 32;
/// Spans holding a field cache at once; one more is left without one.
const MAX_CACHED_SPANS: usize = 1024;
const RESERVED: [&str; 7] = [
    "level",
    "target",
    "timestamp",
    "trace_id",
    "span_id",
    "trace_flags",
    "truncated",
];
static ACTIVE_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static CACHED_SPANS: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    static FORMATTING: Cell<bool> = const { Cell::new(false) };
}

pub(super) struct FormatLayer {
    output: Output,
    format: LoggingFormat,
    dispatch: OnceLock<WeakDispatch>,
}

impl FormatLayer {
    pub(super) fn new(output: Output, format: LoggingFormat) -> Self {
        Self {
            output,
            format,
            dispatch: OnceLock::new(),
        }
    }
}

/// Reservations precede allocations; rejected callbacks never access extensions.
struct Permit(&'static AtomicUsize);

impl Permit {
    fn take(counter: &'static AtomicUsize, limit: usize) -> Option<Self> {
        counter
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < limit).then_some(count + 1)
            })
            .ok()
            .map(|_| Self(counter))
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A callback admitted to format: not reentrant on its thread, and within
/// [`MAX_ACTIVE_CALLBACKS`].
struct FormatGuard {
    _permit: Permit,
}

impl FormatGuard {
    fn enter() -> Result<Self, DropReason> {
        // A thread whose local is gone is treated as already formatting.
        let already = FORMATTING
            .try_with(|active| active.replace(true))
            .unwrap_or(true);
        if already {
            return Err(DropReason::Reentrant);
        }
        let Some(permit) = Permit::take(&ACTIVE_CALLBACKS, MAX_ACTIVE_CALLBACKS) else {
            let _ = FORMATTING.try_with(|active| active.set(false));
            return Err(DropReason::Busy);
        };
        Ok(Self { _permit: permit })
    }
}

impl Drop for FormatGuard {
    fn drop(&mut self) {
        let _ = FORMATTING.try_with(|active| active.set(false));
    }
}

/// Grows with what is written; `limit` bounds it, escaped strings and
/// `collect_str` streaming included.
struct Bytes {
    buf: Vec<u8>,
    limit: usize,
    failed: bool,
}

impl Bytes {
    fn new(limit: usize, capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity),
            limit,
            failed: false,
        }
    }

    fn len(&self) -> usize {
        self.buf.len()
    }

    fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    fn room(&self) -> usize {
        self.limit.saturating_sub(self.buf.len())
    }

    /// Back to `mark`, clearing a failure written after it.
    fn rewind(&mut self, mark: usize) {
        self.buf.truncate(mark);
        self.failed = false;
    }

    fn append(&mut self, bytes: &[u8]) {
        if self.failed || bytes.len() > self.room() {
            self.failed = true;
            return;
        }
        let needed = self.buf.len() + bytes.len();
        if needed > self.buf.capacity() {
            // Doubling stops at the limit: a cached span keeps this buffer,
            // and its memory bound is the limit, not the next power of two.
            let capacity = needed
                .max(self.buf.capacity().saturating_mul(2))
                .min(self.limit);
            self.buf.reserve_exact(capacity - self.buf.len());
        }
        self.buf.extend_from_slice(bytes);
    }

    fn json(&mut self, value: &(impl serde::Serialize + ?Sized)) {
        if !self.failed && serde_json::to_writer(&mut *self, value).is_err() {
            self.failed = true;
        }
    }
}

impl io::Write for Bytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.append(bytes);
        if self.failed {
            Err(io::ErrorKind::WriteZero.into())
        } else {
            Ok(bytes.len())
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl fmt::Write for Bytes {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.append(text.as_bytes());
        if self.failed { Err(fmt::Error) } else { Ok(()) }
    }
}

#[derive(Clone, Copy)]
struct Entry {
    key: &'static str,
    start: usize,
    end: usize,
}

struct Scratch {
    values: Bytes,
    fields: ArrayVec<Entry, MAX_EVENT_FIELDS>,
    /// The record lacks something its call site gave: a value or field that did
    /// not fit, or the fields of a span with no cache.
    truncated: bool,
}

impl Scratch {
    fn new(value_limit: usize) -> Self {
        Self {
            values: Bytes::new(value_limit, SCRATCH_CAPACITY),
            fields: ArrayVec::new(),
            truncated: false,
        }
    }

    /// Records the value written since `start` under `key`.
    fn add_field(&mut self, key: &'static str, start: usize) {
        self.fields.push(Entry {
            key,
            start,
            end: self.values.len(),
        });
    }

    /// Nothing more is captured. A span cache cannot be cut, so for a span this
    /// leaves it without one; an event keeps every field captured before it.
    fn stop(&mut self) {
        self.values.failed = true;
        self.truncated = true;
    }

    fn push(
        &mut self,
        key: &'static str,
        value: &(impl serde::Serialize + ?Sized),
        max_fields: usize,
    ) {
        if self.values.failed {
            return;
        }
        if self.fields.len() == max_fields {
            self.stop();
            return;
        }
        let start = self.values.len();
        self.values.json(value);
        if self.values.failed {
            // A partly written value is no value: the field stays out.
            self.values.buf.truncate(start);
            self.truncated = true;
            return;
        }
        self.add_field(key, start);
    }

    /// A string value of an event, written as the JSON string [`Self::push`] would
    /// write and cut at a character boundary where the room runs out.
    fn push_text(&mut self, key: &'static str, text: fmt::Arguments<'_>) {
        if self.values.failed {
            return;
        }
        if self.fields.is_full() || self.values.room() < 2 {
            self.stop();
            return;
        }
        let start = self.values.len();
        self.values.append(b"\"");
        let mut quoted = Quoted {
            values: &mut self.values,
            cut: false,
        };
        let written = fmt::write(&mut quoted, text);
        let cut = quoted.cut;
        if written.is_err() && !cut {
            // The value's own formatting failed: the field stays out.
            self.values.buf.truncate(start);
            self.truncated = true;
            return;
        }
        self.values.append(b"\"");
        self.add_field(key, start);
        self.truncated |= cut;
    }

    fn copy(&mut self, field: Entry, values: &[u8]) {
        if self.values.failed {
            return;
        }
        if self.fields.is_full() {
            self.stop();
            return;
        }
        let start = self.values.len();
        self.values.append(&values[field.start..field.end]);
        if self.values.failed {
            self.truncated = true;
            return;
        }
        self.add_field(field.key, start);
    }

    /// Writes the captured span fields into `cache`, the last value of a key
    /// winning; old values absent from a delta follow it. Whether they all
    /// fit: a `cache` that did not is partly overwritten and must be discarded.
    fn compact_into(&self, cache: &mut SpanFields) -> bool {
        if self.values.failed {
            return false;
        }
        cache.values.buf.clear();
        cache.fields.clear();
        let captured = self.fields.as_slice();
        for (index, field) in captured.iter().enumerate() {
            if captured[index + 1..]
                .iter()
                .any(|next| next.key == field.key)
            {
                continue;
            }
            if cache.fields.is_full() {
                return false;
            }
            let start = cache.values.len();
            cache
                .values
                .append(&self.values.buf[field.start..field.end]);
            cache.fields.push(Entry {
                key: field.key,
                start,
                end: cache.values.len(),
            });
        }
        !cache.values.failed
    }
}

/// The inside of a JSON string, escaped as `serde_json` escapes it, written
/// until one byte short of the buffer's limit: the closing quote's.
struct Quoted<'a> {
    values: &'a mut Bytes,
    cut: bool,
}

impl fmt::Write for Quoted<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut rest = text;
        loop {
            let plain = rest.bytes().position(needs_escape).unwrap_or(rest.len());
            let (run, tail) = rest.split_at(plain);
            if run.len() >= self.values.room() {
                let fits = run.floor_char_boundary(self.values.room().saturating_sub(1));
                self.values.append(&run.as_bytes()[..fits]);
                return self.stop();
            }
            self.values.append(run.as_bytes());
            let Some(&byte) = tail.as_bytes().first() else {
                return Ok(());
            };
            let mut unit = [0; 6];
            let escaped = escape(byte, &mut unit);
            if escaped.len() >= self.values.room() {
                return self.stop();
            }
            self.values.append(escaped);
            rest = &tail[1..];
        }
    }
}

impl Quoted<'_> {
    /// Stopping the formatter here also ends a source that streams.
    fn stop(&mut self) -> fmt::Result {
        self.cut = true;
        Err(fmt::Error)
    }
}

/// Whether JSON needs `byte` escaped. Every other byte, UTF-8 continuation
/// bytes included, is copied as it is.
const fn needs_escape(byte: u8) -> bool {
    matches!(byte, b'"' | b'\\' | 0..=0x1f)
}

fn escape(byte: u8, unit: &mut [u8; 6]) -> &[u8] {
    match byte {
        b'"' => b"\\\"",
        b'\\' => b"\\\\",
        0x08 => b"\\b",
        0x0c => b"\\f",
        b'\n' => b"\\n",
        b'\r' => b"\\r",
        b'\t' => b"\\t",
        _ => {
            let mut digits = const_hex::Buffer::<1, false>::new();
            digits.format(&[byte]);
            let [high, low] = *digits.as_byte_array::<2>();
            *unit = [b'\\', b'u', b'0', b'0', high, low];
            &unit[..]
        }
    }
}

struct SpanFields {
    values: Bytes,
    fields: ArrayVec<Entry, MAX_SPAN_FIELDS>,
    _slot: Permit,
}

/// What a [`Capture`] records the fields of.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Span,
    Event,
    /// An event bridged from `log`, whose origin also arrives as `log.*`
    /// fields that the normalized metadata already carries.
    BridgedEvent,
}

struct Capture<'a> {
    scratch: &'a mut Scratch,
    source: Source,
}

impl Capture<'_> {
    fn skips(&self, key: &str) -> bool {
        self.source == Source::BridgedEvent && key.starts_with("log.")
    }

    fn push(&mut self, key: &'static str, value: &(impl serde::Serialize + ?Sized)) {
        if self.skips(key) {
            return;
        }
        let max_fields = if self.source == Source::Span {
            MAX_SPAN_FIELDS
        } else {
            MAX_EVENT_FIELDS
        };
        self.scratch.push(key, value, max_fields);
    }

    fn push_text(&mut self, key: &'static str, text: fmt::Arguments<'_>) {
        if self.skips(key) {
            return;
        }
        self.scratch.push_text(key, text);
    }
}

impl Visit for Capture<'_> {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push(field.name(), &value);
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field.name(), &value);
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field.name(), &value);
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field.name(), &value);
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if self.source != Source::Span {
            self.push_text(field.name(), format_args!("{value}"));
        } else if value.len() > self.scratch.values.room() {
            self.scratch.stop();
        } else {
            self.push(field.name(), value);
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let name = field.name();
        if self.source == Source::Span {
            // As `tracing-subscriber` 0.3.23's `JsonVisitor::record_debug`
            // does: a `log.*` field is dropped and a raw identifier loses `r#`.
            if name.starts_with("log.") {
                return;
            }
            let name = name.strip_prefix("r#").unwrap_or(name);
            self.push(name, &format_args!("{value:?}"));
        } else {
            self.push_text(name, format_args!("{value:?}"));
        }
    }
}

impl FormatLayer {
    fn admit(&self) -> Option<FormatGuard> {
        match FormatGuard::enter() {
            Ok(guard) => Some(guard),
            Err(reason) => {
                self.output.record_drop(reason);
                None
            }
        }
    }

    fn format_record<S>(
        &self,
        event: &Event<'_>,
        ctx: &Context<'_, S>,
    ) -> Result<Record, DropReason>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        // Keep allocation outside the write_record hotpath measurement.
        let mut scratch = Scratch::new(MAX_RECORD_BYTES);
        let mut line = Bytes::new(MAX_RECORD_BYTES, LINE_CAPACITY);
        self.write_record(event, ctx, &mut scratch, &mut line)?;
        Ok(Record {
            bytes: line.buf.into_boxed_slice(),
        })
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn write_record<S>(
        &self,
        event: &Event<'_>,
        ctx: &Context<'_, S>,
        scratch: &mut Scratch,
        line: &mut Bytes,
    ) -> Result<(), DropReason>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let normalized = event.normalized_metadata();
        let metadata = normalized.as_ref().unwrap_or_else(|| event.metadata());
        event.record(&mut Capture {
            scratch,
            source: if normalized.is_some() {
                Source::BridgedEvent
            } else {
                Source::Event
            },
        });
        let event_fields = scratch.fields.len();
        let span = ctx.event_span(event);
        if let Some(span) = &span {
            // Leaf first avoids Scope::from_root's growable ancestry buffer.
            for span in span.scope() {
                let extensions = span.extensions();
                let Some(cached) = extensions.get::<SpanFields>() else {
                    // The span had no room for its fields, or outgrew it. The
                    // event is more useful without them than not at all.
                    scratch.truncated = true;
                    continue;
                };
                for field in &cached.fields {
                    scratch.copy(*field, cached.values.as_slice());
                }
                if scratch.values.failed {
                    break;
                }
            }
        }
        match self.format {
            LoggingFormat::Json => {
                line.append(b"{\"level\":");
                line.json(metadata.level().as_str());
                line.append(b",\"target\":");
                line.json(metadata.target());
                line.append(b",\"timestamp\":\"");
                write_timestamp(line);
                line.append(b"\"");
            }
            LoggingFormat::Text => {
                write_timestamp(line);
                line.append(b" ");
                line.append(metadata.level().as_str().as_bytes());
                line.append(b" ");
                line.json(metadata.target());
            }
        }
        if let Some(span) = &span {
            self.write_trace_context_fields(&span.id(), line);
        }
        if line.failed {
            return Err(DropReason::Oversize);
        }
        write_body(
            line,
            scratch.values.as_slice(),
            scratch.fields.as_mut_slice(),
            event_fields,
            scratch.truncated,
            self.format,
        );
        if line.failed {
            Err(DropReason::Oversize)
        } else {
            Ok(())
        }
    }

    fn write_trace_context_fields(&self, id: &Id, line: &mut Bytes) {
        let Some(dispatch) = self.dispatch.get().and_then(WeakDispatch::upgrade) else {
            return;
        };
        let Some(context) = tracing_opentelemetry::get_otel_context(id, &dispatch) else {
            return;
        };
        let span = context.span();
        let ids = span.span_context();
        if !ids.is_valid() {
            return;
        }
        write_hex_field(line, "trace_id", &ids.trace_id().to_bytes(), self.format);
        write_hex_field(line, "span_id", &ids.span_id().to_bytes(), self.format);
        write_hex_field(
            line,
            "trace_flags",
            &[ids.trace_flags().to_u8()],
            self.format,
        );
    }
}

impl<S> Layer<S> for FormatLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_register_dispatch(&self, subscriber: &Dispatch) {
        let _ = self.dispatch.set(subscriber.downgrade());
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(_guard) = self.admit() else {
            return;
        };
        let Some(slot) = Permit::take(&CACHED_SPANS, MAX_CACHED_SPANS) else {
            self.output.record_drop(DropReason::SpanCapacity);
            return;
        };
        let mut scratch = Scratch::new(MAX_SPAN_BYTES);
        attrs.record(&mut Capture {
            scratch: &mut scratch,
            source: Source::Span,
        });
        scratch.push("name", attrs.metadata().name(), MAX_SPAN_FIELDS);
        let mut cache = SpanFields {
            values: Bytes::new(MAX_SPAN_BYTES, scratch.values.len()),
            fields: ArrayVec::new(),
            _slot: slot,
        };
        if !scratch.compact_into(&mut cache) {
            self.output.record_drop(DropReason::SpanCapacity);
            return;
        }
        let Some(span) = ctx.span(id) else {
            return;
        };
        span.extensions_mut().replace(cache);
    }

    fn on_record(&self, id: &Id, values: &SpanRecord<'_>, ctx: Context<'_, S>) {
        // A refused guard leaves prior cached data intact and never touches extensions.
        let Some(_guard) = self.admit() else {
            return;
        };
        let mut scratch = Scratch::new(MAX_SPAN_BYTES);
        values.record(&mut Capture {
            scratch: &mut scratch,
            source: Source::Span,
        });
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        let Some(cached) = extensions.get_mut::<SpanFields>() else {
            return;
        };
        let delta_fields = scratch.fields.len();
        // Old values join the delta before its own repeated keys collapse, so
        // only the compacted fields need to fit the span's bytes.
        scratch.values.limit = MAX_RECORD_BYTES;
        for field in &cached.fields {
            if !scratch.fields[..delta_fields]
                .iter()
                .any(|delta| delta.key == field.key)
            {
                scratch.copy(*field, cached.values.as_slice());
            }
        }
        if !scratch.compact_into(cached) {
            // Absence is permanent for this span: updates never recreate the cache.
            extensions.remove::<SpanFields>();
            drop(extensions);
            self.output.record_drop(DropReason::SpanCapacity);
        }
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let Some(_guard) = self.admit() else {
            return;
        };
        match self.format_record(event, &ctx) {
            Ok(record) => self.output.submit(record),
            Err(reason) => self.output.record_drop(reason),
        }
    }
}

/// The record's fields after its header, then its tail. A record that fits
/// is written whole. One that does not is written again with room for the
/// truncation marker, so the marker always fits.
fn write_body(
    line: &mut Bytes,
    values: &[u8],
    fields: &mut [Entry],
    event_fields: usize,
    truncated: bool,
    format: LoggingFormat,
) {
    let (event, spans) = fields.split_at_mut(event_fields);
    // A total ordering avoids stable-sort allocation while retaining capture order.
    event.sort_unstable_by_key(|field| (field.key, field.start));
    spans.sort_unstable_by_key(|field| (field.key, field.start));
    let header = line.len();
    let mut truncated = truncated;
    if !write_fields(line, values, event, spans, truncated, format) && !truncated {
        truncated = true;
        line.rewind(header);
        // What fits is written; the marker says the rest is missing.
        write_fields(line, values, event, spans, truncated, format);
    }
    line.limit = MAX_RECORD_BYTES;
    line.append(record_tail(format, truncated));
}

/// Whether every field was written, within room for the record's tail: the
/// event's, then its spans'.
fn write_fields(
    line: &mut Bytes,
    values: &[u8],
    event: &[Entry],
    spans: &[Entry],
    truncated: bool,
    format: LoggingFormat,
) -> bool {
    line.limit = MAX_RECORD_BYTES - record_tail(format, truncated).len();
    write_sorted(line, values, event, Pass::Event, format)
        && write_sorted(line, values, spans, Pass::Spans { event }, format)
}

/// The bytes that close a record: the truncation marker when it lacks
/// something, the JSON object's brace, and the newline.
fn record_tail(format: LoggingFormat, truncated: bool) -> &'static [u8] {
    match (format, truncated) {
        (LoggingFormat::Json, false) => b"}\n",
        (LoggingFormat::Json, true) => b",\"truncated\":true}\n",
        (LoggingFormat::Text, false) => b"\n",
        (LoggingFormat::Text, true) => b" \"truncated\"=true\n",
    }
}

/// Which fields a [`write_sorted`] pass writes, and which value of a key wins.
#[derive(Clone, Copy)]
enum Pass<'a> {
    /// The event's own fields: the last value captured for a key wins.
    Event,
    /// Span fields, copied leaf first: the nearest span's value wins, and a
    /// key the event has is the event's.
    Spans { event: &'a [Entry] },
}

/// Whether every field was written. `fields` and a pass's `event` are sorted
/// by key, then capture order. The first that does not fit is written cut
/// when it is a string, and left out otherwise; every later field stays out.
/// No key is left without its value.
fn write_sorted(
    line: &mut Bytes,
    values: &[u8],
    fields: &[Entry],
    pass: Pass<'_>,
    format: LoggingFormat,
) -> bool {
    for (index, field) in fields.iter().enumerate() {
        let shadowed = match pass {
            Pass::Event => fields
                .get(index + 1)
                .is_some_and(|next| next.key == field.key),
            Pass::Spans { event } => {
                (index > 0 && fields[index - 1].key == field.key)
                    || event
                        .binary_search_by_key(&field.key, |entry| entry.key)
                        .is_ok()
            }
        };
        if shadowed || RESERVED.contains(&field.key) {
            continue;
        }
        let kept = line.len();
        let value = &values[field.start..field.end];
        write_key(line, field.key, format);
        line.append(value);
        if line.failed {
            line.rewind(kept);
            if value.first() == Some(&b'"') {
                write_key(line, field.key, format);
                write_cut_string(line, value);
                if line.failed {
                    line.rewind(kept);
                }
            }
            return false;
        }
    }
    true
}

/// As much of the JSON string `value`, quotes included, as the line has room
/// for, closed. It is cut between characters and between escape sequences, so
/// what is written is a valid string.
fn write_cut_string(line: &mut Bytes, value: &[u8]) {
    let inner = &value[1..value.len() - 1];
    let room = line.room().saturating_sub(2);
    let mut end = 0;
    while end < inner.len() {
        let width = match inner[end] {
            b'\\' if inner.get(end + 1) == Some(&b'u') => 6,
            b'\\' => 2,
            lead if lead >= 0xf0 => 4,
            lead if lead >= 0xe0 => 3,
            lead if lead >= 0xc0 => 2,
            _ => 1,
        };
        if end + width > room {
            break;
        }
        end += width;
    }
    line.append(b"\"");
    line.append(&inner[..end]);
    line.append(b"\"");
}

fn write_key(line: &mut Bytes, key: &str, format: LoggingFormat) {
    let (separator, assign) = match format {
        LoggingFormat::Json => (b",", b":"),
        LoggingFormat::Text => (b" ", b"="),
    };
    line.append(separator);
    line.json(key);
    line.append(assign);
}

fn write_timestamp(line: &mut Bytes) {
    // `humantime` panics before the Unix epoch and refuses years after 9999;
    // `tracing-subscriber`'s formatter writes the same RFC 3339 form there.
    let now = std::time::SystemTime::now();
    if now >= std::time::UNIX_EPOCH
        && write!(line, "{}", humantime::format_rfc3339_micros(now)).is_ok()
    {
        return;
    }
    let _ = SystemTime.format_time(&mut Writer::new(line));
}

/// `key` with `bytes` as a quoted lowercase hex string.
fn write_hex_field<const N: usize>(
    line: &mut Bytes,
    key: &str,
    bytes: &[u8; N],
    format: LoggingFormat,
) {
    write_key(line, key, format);
    line.append(b"\"");
    line.append(
        const_hex::Buffer::<N, false>::new()
            .format(bytes)
            .as_bytes(),
    );
    line.append(b"\"");
}

#[cfg(test)]
mod tests;
