use std::cell::Cell;
use std::fmt::{self, Write as _};
use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use arrayvec::ArrayVec;
use opentelemetry::trace::TraceContextExt as _;
use smallvec::SmallVec;
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

const SCRATCH_CAPACITY: usize = 256;
const LINE_CAPACITY: usize = 512;
const MAX_EVENT_FIELDS: usize = 128;
const MAX_SPAN_BYTES: usize = 4096;
const MAX_SPAN_FIELDS: usize = 64;
const MAX_ACTIVE_CALLBACKS: usize = 32;
const MAX_CACHED_SPANS: usize = 1024;
const TRACE_ID_FIELD: &str = "trace_id";
const SPAN_ID_FIELD: &str = "span_id";
const TRACE_FLAGS_FIELD: &str = "trace_flags";
const RESERVED_KEYS: [&str; 7] = [
    "level",
    "target",
    "timestamp",
    TRACE_ID_FIELD,
    SPAN_ID_FIELD,
    TRACE_FLAGS_FIELD,
    "truncated",
];
static ACTIVE_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static CACHED_SPANS: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    static FORMATTING: Cell<bool> = const { Cell::new(false) };
}

struct TraceDispatch {
    global: OnceLock<Dispatch>,
    registered: OnceLock<WeakDispatch>,
}

impl TraceDispatch {
    fn new() -> Self {
        Self {
            global: OnceLock::new(),
            registered: OnceLock::new(),
        }
    }

    fn register(&self, subscriber: &Dispatch) {
        let _ = self.registered.set(subscriber.downgrade());
    }

    fn pin_global(&self, dispatch: Dispatch) {
        let _ = self.global.set(dispatch);
    }

    fn otel_context(&self, id: &Id) -> Option<opentelemetry::Context> {
        if let Some(dispatch) = self.global.get() {
            return tracing_opentelemetry::get_otel_context(id, dispatch);
        }
        let dispatch = self.registered.get()?.upgrade()?;
        tracing_opentelemetry::get_otel_context(id, &dispatch)
    }
}

pub(super) struct FormatLayer {
    output: Output,
    format: LoggingFormat,
    trace_dispatch: Option<TraceDispatch>,
}

impl FormatLayer {
    pub(super) fn new(output: Output, format: LoggingFormat, trace_context: bool) -> Self {
        Self {
            output,
            format,
            trace_dispatch: trace_context.then(TraceDispatch::new),
        }
    }

    pub(super) fn pin_global_dispatch(&self, dispatch: &Dispatch) {
        if let Some(trace_dispatch) = &self.trace_dispatch {
            trace_dispatch.pin_global(dispatch.clone());
        }
    }

    #[cfg(test)]
    pub(super) fn has_pinned_global_dispatch(&self) -> bool {
        self.trace_dispatch
            .as_ref()
            .is_some_and(|dispatch| dispatch.global.get().is_some())
    }
}

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

fn enter_format() -> Result<impl Drop, DropReason> {
    let was_formatting = FORMATTING
        .try_with(|active| active.replace(true))
        .unwrap_or(true);
    if was_formatting {
        return Err(DropReason::Reentrant);
    }
    let formatting = std::mem::DropGuard::new((), |()| {
        let _ = FORMATTING.try_with(|active| active.set(false));
    });
    let permit = Permit::take(&ACTIVE_CALLBACKS, MAX_ACTIVE_CALLBACKS).ok_or(DropReason::Busy)?;
    Ok(std::mem::DropGuard::new(permit, move |_permit| {
        drop(formatting);
    }))
}

struct Bytes<const INLINE: usize = 0> {
    buf: SmallVec<[u8; INLINE]>,
    limit: usize,
    failed: bool,
}

impl Bytes<0> {
    fn with_capacity(limit: usize, capacity: usize) -> Self {
        Self {
            buf: SmallVec::with_capacity(capacity),
            limit,
            failed: false,
        }
    }
}

impl<const INLINE: usize> Bytes<INLINE> {
    const fn inline(limit: usize) -> Self {
        Self {
            buf: SmallVec::new_const(),
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

    fn rewind(&mut self, mark: usize) {
        self.buf.truncate(mark);
        self.failed = false;
    }

    fn reserving<R>(&mut self, reserved: usize, write: impl FnOnce(&mut Self) -> R) -> R {
        let limit = self.limit;
        self.limit = limit - reserved;
        let written = write(self);
        self.limit = limit;
        written
    }

    fn append(&mut self, bytes: &[u8]) {
        if self.failed || bytes.len() > self.room() {
            self.failed = true;
            return;
        }
        let needed = self.buf.len() + bytes.len();
        if needed > self.buf.capacity() {
            self.buf.reserve_exact(bounded_reserve(
                self.buf.len(),
                self.buf.capacity(),
                needed,
                self.limit,
            ));
        }
        self.buf.extend_from_slice(bytes);
    }

    fn append_json(&mut self, value: &(impl serde::Serialize + ?Sized)) {
        if !self.failed && serde_json::to_writer(&mut *self, value).is_err() {
            self.failed = true;
        }
    }
}

impl<const INLINE: usize> io::Write for Bytes<INLINE> {
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

impl<const INLINE: usize> fmt::Write for Bytes<INLINE> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.append(text.as_bytes());
        if self.failed { Err(fmt::Error) } else { Ok(()) }
    }
}

const _: () = assert!(MAX_RECORD_BYTES <= u16::MAX as usize);

#[derive(Clone, Copy)]
struct Entry {
    key: &'static str,
    start: u16,
    end: u16,
}

impl Entry {
    fn from_range(key: &'static str, range: std::ops::Range<usize>) -> Self {
        let offset = |index: usize| u16::try_from(index).unwrap_or(u16::MAX);
        Self {
            key,
            start: offset(range.start),
            end: offset(range.end),
        }
    }

    fn range(self) -> std::ops::Range<usize> {
        usize::from(self.start)..usize::from(self.end)
    }
}

struct FieldBuf<const MAX_FIELDS: usize> {
    values: Bytes<SCRATCH_CAPACITY>,
    fields: ArrayVec<Entry, MAX_FIELDS>,
}

impl<const MAX_FIELDS: usize> FieldBuf<MAX_FIELDS> {
    fn new(limit: usize) -> Self {
        Self {
            values: Bytes::inline(limit),
            fields: ArrayVec::new(),
        }
    }

    fn add_field(&mut self, key: &'static str, start: usize) {
        self.fields
            .push(Entry::from_range(key, start..self.values.len()));
    }

    fn begin_field(&mut self) -> Option<usize> {
        if self.values.failed {
            return None;
        }
        if self.fields.is_full() {
            self.values.failed = true;
            return None;
        }
        Some(self.values.len())
    }

    fn push_json(&mut self, key: &'static str, value: &(impl serde::Serialize + ?Sized)) -> bool {
        let Some(start) = self.begin_field() else {
            return false;
        };
        self.values.append_json(value);
        if self.values.failed {
            self.values.buf.truncate(start);
            return false;
        }
        self.add_field(key, start);
        true
    }

    fn push_raw(&mut self, key: &'static str, bytes: &[u8]) -> bool {
        let Some(start) = self.begin_field() else {
            return false;
        };
        self.values.append(bytes);
        if self.values.failed {
            return false;
        }
        self.add_field(key, start);
        true
    }
}

struct EventScratch {
    captured: FieldBuf<MAX_EVENT_FIELDS>,
    truncated: bool,
}

impl EventScratch {
    fn new() -> Self {
        Self {
            captured: FieldBuf::new(MAX_RECORD_BYTES),
            truncated: false,
        }
    }

    fn truncate_rest(&mut self) {
        self.captured.values.failed = true;
        self.truncated = true;
    }

    fn push_json(&mut self, key: &'static str, value: &(impl serde::Serialize + ?Sized)) {
        if !self.captured.push_json(key, value) {
            self.truncated = true;
        }
    }

    fn push_truncating_str(&mut self, key: &'static str, text: fmt::Arguments<'_>) {
        if self.captured.values.failed {
            return;
        }
        if self.captured.fields.is_full() || self.captured.values.room() < 2 {
            self.truncate_rest();
            return;
        }
        let start = self.captured.values.len();
        self.captured.values.append(b"\"");
        let mut quoted = Quoted {
            values: &mut self.captured.values,
            cut: false,
        };
        let written = fmt::write(&mut quoted, text);
        let cut = quoted.cut;
        if written.is_err() && !cut {
            self.captured.values.buf.truncate(start);
            self.truncated = true;
            return;
        }
        self.captured.values.append(b"\"");
        self.captured.add_field(key, start);
        self.truncated |= cut;
    }

    fn copy_span_fields(&mut self, span: &SpanFields) -> bool {
        for field in &span.fields {
            if !self
                .captured
                .push_raw(field.key, &span.values[field.range()])
            {
                self.truncated = true;
            }
        }
        !self.captured.values.failed
    }

    fn write_body(&mut self, line: &mut Bytes, event_field_count: usize, format: LoggingFormat) {
        let values = self.captured.values.as_slice();
        let (event, spans) = self.captured.fields.split_at_mut(event_field_count);
        event.sort_unstable_by_key(|field| (field.key, field.start));
        spans.sort_unstable_by_key(|field| (field.key, field.start));
        let (event, spans) = (&*event, &*spans);
        let write_fields = |line: &mut Bytes, truncated: bool| {
            line.reserving(record_tail(format, truncated).len(), |line| {
                write_all_fields(line, values, event, spans, format)
            })
        };
        let header = line.len();
        let fits = write_fields(line, self.truncated);
        if !fits && !self.truncated {
            line.rewind(header);
            write_fields(line, true);
        }
        line.append(record_tail(format, self.truncated || !fits));
    }
}

type SpanScratch = FieldBuf<MAX_SPAN_FIELDS>;

struct Quoted<'a, const INLINE: usize> {
    values: &'a mut Bytes<INLINE>,
    cut: bool,
}

impl<const INLINE: usize> fmt::Write for Quoted<'_, INLINE> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut rest = text;
        loop {
            let room = self.values.room().saturating_sub(1);
            let bytes = rest.as_bytes();
            let prefix = &bytes[..bytes.len().min(room)];
            let Some(plain) = prefix.iter().copied().position(needs_escape) else {
                if bytes.len() <= room {
                    self.values.append(bytes);
                    return Ok(());
                }
                self.values.append(&bytes[..rest.floor_char_boundary(room)]);
                return self.stop();
            };
            self.values.append(&bytes[..plain]);
            let mut unit = [0; 6];
            let escaped = escape(bytes[plain], &mut unit);
            if escaped.len() >= self.values.room() {
                return self.stop();
            }
            self.values.append(escaped);
            rest = rest.get(plain + 1..).ok_or(fmt::Error)?;
        }
    }
}

impl<const INLINE: usize> Quoted<'_, INLINE> {
    fn stop(&mut self) -> fmt::Result {
        self.cut = true;
        Err(fmt::Error)
    }
}

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
    values: Vec<u8>,
    fields: SmallVec<[Entry; 8]>,
    _permit: Permit,
}

impl SpanFields {
    fn from_capture(permit: Permit, capture: &SpanScratch) -> Option<Self> {
        let (values, fields) = merge_span_fields(&[], &[], capture)?;
        Some(Self {
            values,
            fields,
            _permit: permit,
        })
    }

    fn apply(&mut self, delta: &SpanScratch) -> bool {
        if delta.values.failed {
            return false;
        }
        if delta.fields.is_empty() {
            return true;
        }
        if let [field] = delta.fields.as_slice()
            && !self.fields.iter().any(|existing| existing.key == field.key)
        {
            return self.insert_new_field(field.key, &delta.values.as_slice()[field.range()]);
        }
        let Some((values, fields)) = merge_span_fields(&self.fields, &self.values, delta) else {
            return false;
        };
        self.values = values;
        self.fields = fields;
        true
    }

    fn insert_new_field(&mut self, key: &'static str, value: &[u8]) -> bool {
        if self.fields.len() == MAX_SPAN_FIELDS
            || value.len() > MAX_SPAN_BYTES.saturating_sub(self.values.len())
        {
            return false;
        }
        let start = self.values.len();
        append_bounded(&mut self.values, value, MAX_SPAN_BYTES);
        self.fields
            .insert(0, Entry::from_range(key, start..self.values.len()));
        true
    }
}

fn bounded_reserve(len: usize, capacity: usize, needed: usize, limit: usize) -> usize {
    needed.max(capacity.saturating_mul(2)).min(limit) - len
}

fn append_bounded(buf: &mut Vec<u8>, bytes: &[u8], limit: usize) {
    let needed = buf.len() + bytes.len();
    if needed > buf.capacity() {
        buf.reserve_exact(bounded_reserve(buf.len(), buf.capacity(), needed, limit));
    }
    buf.extend_from_slice(bytes);
}

fn merge_span_fields(
    base_fields: &[Entry],
    base_values: &[u8],
    update: &SpanScratch,
) -> Option<(Vec<u8>, SmallVec<[Entry; 8]>)> {
    if update.values.failed {
        return None;
    }
    let capacity = (update.values.len() + base_values.len()).min(MAX_SPAN_BYTES);
    let mut next_values = Bytes::with_capacity(MAX_SPAN_BYTES, capacity);
    let mut next_fields = SmallVec::new();
    let latest_updates = update
        .fields
        .iter()
        .enumerate()
        .filter(|&(index, field)| {
            !update.fields[index + 1..]
                .iter()
                .any(|next| next.key == field.key)
        })
        .map(|(_, field)| (field.key, &update.values.as_slice()[field.range()]));
    let unchanged = base_fields
        .iter()
        .filter(|field| !update.fields.iter().any(|next| next.key == field.key))
        .map(|field| (field.key, &base_values[field.range()]));
    for (key, value) in latest_updates.chain(unchanged) {
        if next_fields.len() == MAX_SPAN_FIELDS {
            return None;
        }
        let start = next_values.len();
        next_values.append(value);
        if next_values.failed {
            return None;
        }
        next_fields.push(Entry::from_range(key, start..next_values.len()));
    }
    Some((next_values.buf.into_vec(), next_fields))
}

struct EventCapture<'a> {
    scratch: &'a mut EventScratch,
    bridged: bool,
}

impl EventCapture<'_> {
    fn is_bridged_log_field(&self, key: &str) -> bool {
        self.bridged && key.starts_with("log.")
    }

    fn push_json(&mut self, key: &'static str, value: &(impl serde::Serialize + ?Sized)) {
        if !self.is_bridged_log_field(key) {
            self.scratch.push_json(key, value);
        }
    }

    fn push_truncating_str(&mut self, key: &'static str, text: fmt::Arguments<'_>) {
        if !self.is_bridged_log_field(key) {
            self.scratch.push_truncating_str(key, text);
        }
    }
}

impl Visit for EventCapture<'_> {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push_json(field.name(), &value);
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push_json(field.name(), &value);
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push_json(field.name(), &value);
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push_json(field.name(), &value);
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push_truncating_str(field.name(), format_args!("{value}"));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push_truncating_str(field.name(), format_args!("{value:?}"));
    }
}

struct SpanCapture<'a>(&'a mut SpanScratch);

impl Visit for SpanCapture<'_> {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.push_json(field.name(), &value);
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.push_json(field.name(), &value);
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.push_json(field.name(), &value);
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.push_json(field.name(), &value);
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if value.len() > self.0.values.room() {
            self.0.values.failed = true;
        } else {
            self.0.push_json(field.name(), value);
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let name = field.name();
        if name.starts_with("log.") {
            return;
        }
        let name = name.trim_prefix("r#");
        self.0.push_json(name, &format_args!("{value:?}"));
    }
}

impl FormatLayer {
    fn admit(&self) -> Option<impl Drop> {
        match enter_format() {
            Ok(guard) => Some(guard),
            Err(reason) => {
                self.output.record_drop(reason);
                None
            }
        }
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn format_record<S>(
        &self,
        event: &Event<'_>,
        ctx: &Context<'_, S>,
    ) -> Result<Record, DropReason>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let mut scratch = EventScratch::new();
        let mut line = Bytes::with_capacity(MAX_RECORD_BYTES, LINE_CAPACITY);
        let normalized = event.normalized_metadata();
        let (metadata, bridged) = match &normalized {
            Some(metadata) => (metadata, true),
            None => (event.metadata(), false),
        };
        event.record(&mut EventCapture {
            scratch: &mut scratch,
            bridged,
        });
        let event_field_count = scratch.captured.fields.len();
        write_record_header(&mut line, metadata, self.format);
        if let Some(event_span) = ctx.event_span(event) {
            self.write_trace_context_fields(&event_span.id(), &mut line);
            for scope_span in event_span.scope() {
                let extensions = scope_span.extensions();
                let Some(cached) = extensions.get::<SpanFields>() else {
                    scratch.truncated = true;
                    continue;
                };
                if !scratch.copy_span_fields(cached) {
                    break;
                }
            }
        }
        if line.failed {
            return Err(DropReason::Oversize);
        }
        scratch.write_body(&mut line, event_field_count, self.format);
        if line.failed {
            Err(DropReason::Oversize)
        } else {
            Ok(line.buf.into_vec())
        }
    }

    fn write_trace_context_fields(&self, id: &Id, line: &mut Bytes) {
        let Some(context) = self
            .trace_dispatch
            .as_ref()
            .and_then(|dispatch| dispatch.otel_context(id))
        else {
            return;
        };
        let span = context.span();
        let ids = span.span_context();
        if !ids.is_valid() {
            return;
        }
        write_hex_field(
            line,
            TRACE_ID_FIELD,
            &ids.trace_id().to_bytes(),
            self.format,
        );
        write_hex_field(line, SPAN_ID_FIELD, &ids.span_id().to_bytes(), self.format);
        write_hex_field(
            line,
            TRACE_FLAGS_FIELD,
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
        if let Some(dispatch) = &self.trace_dispatch {
            dispatch.register(subscriber);
        }
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(_guard) = self.admit() else {
            return;
        };
        let Some(permit) = Permit::take(&CACHED_SPANS, MAX_CACHED_SPANS) else {
            self.output.record_drop(DropReason::SpanCapacity);
            return;
        };
        let mut scratch = SpanScratch::new(MAX_SPAN_BYTES);
        attrs.record(&mut SpanCapture(&mut scratch));
        scratch.push_json("name", attrs.metadata().name());
        let Some(cache) = SpanFields::from_capture(permit, &scratch) else {
            self.output.record_drop(DropReason::SpanCapacity);
            return;
        };
        let Some(span) = ctx.span(id) else {
            return;
        };
        span.extensions_mut().replace(cache);
    }

    fn on_record(&self, id: &Id, values: &SpanRecord<'_>, ctx: Context<'_, S>) {
        let Some(_guard) = self.admit() else {
            return;
        };
        let mut scratch = SpanScratch::new(MAX_SPAN_BYTES);
        values.record(&mut SpanCapture(&mut scratch));
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        let Some(cached) = extensions.get_mut::<SpanFields>() else {
            return;
        };
        if !cached.apply(&scratch) {
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

fn write_record_header(line: &mut Bytes, metadata: &tracing::Metadata<'_>, format: LoggingFormat) {
    match format {
        LoggingFormat::Json => {
            line.append(b"{\"level\":\"");
            line.append(metadata.level().as_str().as_bytes());
            line.append(b"\",\"target\":");
            line.append_json(metadata.target());
            line.append(b",\"timestamp\":\"");
            write_timestamp(line);
            line.append(b"\"");
        }
        LoggingFormat::Text => {
            write_timestamp(line);
            line.append(b" ");
            line.append(metadata.level().as_str().as_bytes());
            line.append(b" ");
            line.append_json(metadata.target());
        }
    }
}

fn record_tail(format: LoggingFormat, truncated: bool) -> &'static [u8] {
    match (format, truncated) {
        (LoggingFormat::Json, false) => b"}\n",
        (LoggingFormat::Json, true) => b",\"truncated\":true}\n",
        (LoggingFormat::Text, false) => b"\n",
        (LoggingFormat::Text, true) => b" \"truncated\"=true\n",
    }
}

fn write_all_fields(
    line: &mut Bytes,
    values: &[u8],
    event: &[Entry],
    spans: &[Entry],
    format: LoggingFormat,
) -> bool {
    let same_key = |a: &Entry, b: &Entry| a.key == b.key;
    let event_fields = event.chunk_by(same_key).filter_map(<[_]>::last);
    let span_fields = spans
        .chunk_by(same_key)
        .filter_map(<[_]>::first)
        .filter(|field| {
            event
                .binary_search_by_key(&field.key, |entry| entry.key)
                .is_err()
        });
    for field in event_fields.chain(span_fields) {
        if RESERVED_KEYS.contains(&field.key) {
            continue;
        }
        let field_start = line.len();
        let value = &values[field.range()];
        write_key(line, field.key, format);
        line.append(value);
        if line.failed {
            line.rewind(field_start);
            if let Some(inner) = value.strip_circumfix(b"\"", b"\"") {
                write_key(line, field.key, format);
                write_cut_string(line, inner);
                if line.failed {
                    line.rewind(field_start);
                }
            }
            return false;
        }
    }
    true
}

fn write_cut_string(line: &mut Bytes, inner: &[u8]) {
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

fn field_tokens(format: LoggingFormat) -> (&'static [u8], &'static [u8]) {
    match format {
        LoggingFormat::Json => (b",", b":"),
        LoggingFormat::Text => (b" ", b"="),
    }
}

fn write_key(line: &mut Bytes, key: &str, format: LoggingFormat) {
    let (separator, assign) = field_tokens(format);
    line.append(separator);
    line.append_json(key);
    line.append(assign);
}

fn write_timestamp(line: &mut Bytes) {
    if let Ok(now) = jiff::Timestamp::try_from(std::time::SystemTime::now())
        && now >= jiff::Timestamp::UNIX_EPOCH
        && write!(line, "{now:.6}").is_ok()
    {
        return;
    }
    let _ = SystemTime.format_time(&mut Writer::new(line));
}

fn write_hex_field<const N: usize>(
    line: &mut Bytes,
    key: &str,
    bytes: &[u8; N],
    format: LoggingFormat,
) {
    let (separator, assign) = field_tokens(format);
    line.append(separator);
    line.append(b"\"");
    line.append(key.as_bytes());
    line.append(b"\"");
    line.append(assign);
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
