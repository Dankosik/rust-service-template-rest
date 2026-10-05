//! Bounded JSON/text capture. Only complete records reach the output owner.
//!
//! Event fields precede sorted span fields; event values win over spans and
//! the nearest span wins over its ancestors. Reserved keys remain unique.

use std::cell::Cell;
use std::fmt::{self, Write as _};
use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

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

const MAX_FIELDS: usize = 128;
const SPAN_BYTES: usize = 4096;
const SPAN_FIELDS: usize = 64;
const RESERVED: [&str; 6] = [
    "level",
    "target",
    "timestamp",
    "trace_id",
    "span_id",
    "trace_flags",
];
static CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static SPANS: AtomicUsize = AtomicUsize::new(0);
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

struct Callback {
    _permit: Permit,
}

impl Callback {
    fn enter() -> Result<Self, DropReason> {
        if !FORMATTING
            .try_with(|active| !active.replace(true))
            .unwrap_or(false)
        {
            return Err(DropReason::Reentrant);
        }
        let Some(permit) = Permit::take(&CALLBACKS, 32) else {
            let _ = FORMATTING.try_with(|active| active.set(false));
            return Err(DropReason::Busy);
        };
        Ok(Self { _permit: permit })
    }
}

impl Drop for Callback {
    fn drop(&mut self) {
        let _ = FORMATTING.try_with(|active| active.set(false));
    }
}

/// Fixed backing capacity also bounds escaped strings and `collect_str` streaming.
struct Bytes<const N: usize> {
    storage: Box<[u8; N]>,
    len: usize,
    limit: usize,
    failed: bool,
}

impl<const N: usize> Bytes<N> {
    fn new(limit: usize) -> Self {
        Self {
            storage: Box::new([0; N]),
            len: 0,
            limit,
            failed: false,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        if self.failed || bytes.len() > self.limit - self.len {
            self.failed = true;
            return;
        }
        self.storage[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    fn json(&mut self, value: &(impl serde::Serialize + ?Sized)) {
        if !self.failed && serde_json::to_writer(&mut *self, value).is_err() {
            self.failed = true;
        }
    }
}

impl<const N: usize> io::Write for Bytes<N> {
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

impl<const N: usize> fmt::Write for Bytes<N> {
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
const EMPTY: Entry = Entry {
    key: "",
    start: 0,
    end: 0,
};

struct Scratch {
    line: Bytes<MAX_RECORD_BYTES>,
    values: Bytes<MAX_RECORD_BYTES>,
    fields: [Entry; MAX_FIELDS],
    len: usize,
}

impl Scratch {
    fn new(value_limit: usize) -> Self {
        Self {
            line: Bytes::new(MAX_RECORD_BYTES),
            values: Bytes::new(value_limit),
            fields: [EMPTY; MAX_FIELDS],
            len: 0,
        }
    }

    fn push(&mut self, key: &'static str, value: &(impl serde::Serialize + ?Sized), limit: usize) {
        if self.values.failed {
            return;
        }
        if self.len == limit {
            self.values.failed = true;
            return;
        }
        let start = self.values.len;
        self.values.json(value);
        self.fields[self.len] = Entry {
            key,
            start,
            end: self.values.len,
        };
        self.len += 1;
    }

    fn copy(&mut self, field: Entry, values: &[u8]) {
        if self.values.failed {
            return;
        }
        if self.len == MAX_FIELDS {
            self.values.failed = true;
            return;
        }
        let start = self.values.len;
        self.values.append(&values[field.start..field.end]);
        self.fields[self.len] = Entry {
            key: field.key,
            start,
            end: self.values.len,
        };
        self.len += 1;
    }

    /// Last delta value wins. Old values absent from the delta follow it.
    /// The output scratch doubles as replacement storage: no third byte buffer.
    fn compact_span(&mut self) -> bool {
        if self.values.failed {
            return false;
        }
        self.line.limit = SPAN_BYTES;
        let mut kept = 0;
        for index in 0..self.len {
            let field = self.fields[index];
            if self.fields[index + 1..self.len]
                .iter()
                .any(|next| next.key == field.key)
            {
                continue;
            }
            if kept == SPAN_FIELDS {
                return false;
            }
            let start = self.line.len;
            self.line.append(&self.values.storage[field.start..field.end]);
            self.fields[kept] = Entry {
                key: field.key,
                start,
                end: self.line.len,
            };
            kept += 1;
        }
        self.len = kept;
        !self.line.failed
    }
}

struct SpanFields {
    values: Bytes<SPAN_BYTES>,
    fields: [Entry; SPAN_FIELDS],
    len: usize,
    _slot: Permit,
}

impl SpanFields {
    fn replace(&mut self, scratch: &Scratch) {
        self.values.len = scratch.line.len;
        self.values.storage[..self.values.len]
            .copy_from_slice(&scratch.line.storage[..self.values.len]);
        self.len = scratch.len;
        self.fields[..self.len].copy_from_slice(&scratch.fields[..self.len]);
    }
}

struct Capture<'a> {
    scratch: &'a mut Scratch,
    span: bool,
    bridged: bool,
}

impl Capture<'_> {
    fn push(&mut self, key: &'static str, value: &(impl serde::Serialize + ?Sized)) {
        if self.bridged && key.starts_with("log.") {
            return;
        }
        self.scratch
            .push(key, value, if self.span { SPAN_FIELDS } else { MAX_FIELDS });
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
        self.push(field.name(), value);
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let mut name = field.name();
        if self.span {
            if name.starts_with("log.") {
                return;
            }
            name = name.strip_prefix("r#").unwrap_or(name);
        }
        self.push(name, &format_args!("{value:?}"));
    }
}

impl FormatLayer {
    fn callback(&self) -> Option<Callback> {
        match Callback::enter() {
            Ok(callback) => Some(callback),
            Err(reason) => {
                self.output.dropped(reason);
                None
            }
        }
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn format<S>(
        &self,
        event: &Event<'_>,
        ctx: &Context<'_, S>,
        scratch: &mut Scratch,
    ) -> Result<(), DropReason>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let normalized = event.normalized_metadata();
        let metadata = normalized.as_ref().unwrap_or_else(|| event.metadata());
        event.record(&mut Capture {
            scratch,
            span: false,
            bridged: normalized.is_some(),
        });
        if scratch.values.failed {
            return Err(DropReason::Oversize);
        }
        let event_fields = scratch.len;
        let span = ctx.event_span(event);
        if let Some(span) = &span {
            // Leaf first avoids Scope::from_root's growable ancestry buffer.
            for span in span.scope() {
                let extensions = span.extensions();
                let Some(cached) = extensions.get::<SpanFields>() else {
                    return Err(DropReason::SpanCapacity);
                };
                for field in &cached.fields[..cached.len] {
                    scratch.copy(*field, &cached.values.storage[..cached.values.len]);
                }
                if scratch.values.failed {
                    return Err(DropReason::Oversize);
                }
            }
        }
        let line = &mut scratch.line;
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
            self.write_trace_ids(&span.id(), line);
        }
        write_sorted(
            line,
            &scratch.values.storage[..scratch.values.len],
            &mut scratch.fields[..event_fields],
            &[],
            self.format,
            false,
        );
        let (event, spans) = scratch.fields[..scratch.len].split_at_mut(event_fields);
        write_sorted(
            line,
            &scratch.values.storage[..scratch.values.len],
            spans,
            event,
            self.format,
            true,
        );
        if matches!(self.format, LoggingFormat::Json) {
            line.append(b"}");
        }
        line.append(b"\n");
        if line.failed {
            Err(DropReason::Oversize)
        } else {
            Ok(())
        }
    }

    fn write_trace_ids(&self, id: &Id, line: &mut Bytes<MAX_RECORD_BYTES>) {
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
        write_key(line, "trace_id", self.format);
        line.append(b"\"");
        write_hex(line, &ids.trace_id().to_bytes());
        line.append(b"\"");
        write_key(line, "span_id", self.format);
        line.append(b"\"");
        write_hex(line, &ids.span_id().to_bytes());
        line.append(b"\"");
        write_key(line, "trace_flags", self.format);
        line.append(b"\"");
        write_hex(line, &[ids.trace_flags().to_u8()]);
        line.append(b"\"");
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
        let Some(_callback) = self.callback() else {
            return;
        };
        let Some(slot) = Permit::take(&SPANS, 1024) else {
            self.output.dropped(DropReason::SpanCapacity);
            return;
        };
        let mut scratch = Scratch::new(SPAN_BYTES);
        attrs.record(&mut Capture {
            scratch: &mut scratch,
            span: true,
            bridged: false,
        });
        scratch.push("name", attrs.metadata().name(), SPAN_FIELDS);
        if !scratch.compact_span() {
            self.output.dropped(DropReason::SpanCapacity);
            return;
        }
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut fields = SpanFields {
            values: Bytes::new(SPAN_BYTES),
            fields: [EMPTY; SPAN_FIELDS],
            len: 0,
            _slot: slot,
        };
        fields.replace(&scratch);
        span.extensions_mut().replace(fields);
    }

    fn on_record(&self, id: &Id, values: &SpanRecord<'_>, ctx: Context<'_, S>) {
        // Admission rejection leaves prior cached data intact and never touches extensions.
        let Some(_callback) = self.callback() else {
            return;
        };
        let mut scratch = Scratch::new(SPAN_BYTES);
        values.record(&mut Capture {
            scratch: &mut scratch,
            span: true,
            bridged: false,
        });
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        let Some(cached) = extensions.get_mut::<SpanFields>() else {
            return;
        };
        let delta_fields = scratch.len;
        scratch.values.limit = MAX_RECORD_BYTES;
        for field in &cached.fields[..cached.len] {
            if !scratch.fields[..delta_fields]
                .iter()
                .any(|delta| delta.key == field.key)
            {
                scratch.copy(*field, &cached.values.storage[..cached.values.len]);
            }
        }
        if scratch.compact_span() {
            cached.replace(&scratch);
        } else {
            // Absence is permanent for this span: updates never recreate the cache.
            extensions.remove::<SpanFields>();
            drop(extensions);
            self.output.dropped(DropReason::SpanCapacity);
        }
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let Some(_callback) = self.callback() else {
            return;
        };
        let mut scratch = Scratch::new(MAX_RECORD_BYTES);
        match self.format(event, &ctx, &mut scratch) {
            Ok(()) => self.output.submit(Record {
                bytes: scratch.line.storage,
                len: scratch.line.len,
            }),
            Err(reason) => self.output.dropped(reason),
        }
    }
}

fn write_sorted(
    line: &mut Bytes<MAX_RECORD_BYTES>,
    values: &[u8],
    fields: &mut [Entry],
    event: &[Entry],
    format: LoggingFormat,
    nearest_first: bool,
) {
    // A total ordering avoids stable-sort allocation while retaining capture order.
    fields.sort_unstable_by_key(|field| (field.key, field.start));
    for (index, field) in fields.iter().enumerate() {
        let duplicate = if nearest_first {
            index > 0 && fields[index - 1].key == field.key
        } else {
            fields
                .get(index + 1)
                .is_some_and(|next| next.key == field.key)
        };
        if duplicate
            || RESERVED.contains(&field.key)
            || event
                .binary_search_by_key(&field.key, |entry| entry.key)
                .is_ok()
        {
            continue;
        }
        write_key(line, field.key, format);
        line.append(&values[field.start..field.end]);
    }
}

fn write_key(line: &mut Bytes<MAX_RECORD_BYTES>, key: &str, format: LoggingFormat) {
    line.append(if matches!(format, LoggingFormat::Json) {
        b","
    } else {
        b" "
    });
    line.json(key);
    line.append(if matches!(format, LoggingFormat::Json) {
        b":"
    } else {
        b"="
    });
}

fn write_timestamp(line: &mut Bytes<MAX_RECORD_BYTES>) {
    let now = std::time::SystemTime::now();
    if now >= std::time::UNIX_EPOCH
        && write!(line, "{}", humantime::format_rfc3339_micros(now)).is_ok()
    {
        return;
    }
    let _ = SystemTime.format_time(&mut Writer::new(line));
}

fn write_hex(line: &mut Bytes<MAX_RECORD_BYTES>, bytes: &[u8]) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        line.append(&[
            DIGITS[usize::from(byte >> 4)],
            DIGITS[usize::from(byte & 15)],
        ]);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "bounded formatter fixtures require explicit setup and drain success"
    )]

    use super::super::output::{self, LogSnapshot, LoggerShutdown};
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt as _;

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("sink lock").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn capture(format: LoggingFormat, emit: impl FnOnce()) -> (String, LogSnapshot) {
        let sink = Sink::default();
        let (output, guard) = output::start(sink.clone()).expect("writer starts");
        let dispatch = Dispatch::new(Registry::default().with(FormatLayer::new(output, format)));
        tracing::dispatcher::with_default(&dispatch, emit);
        let result = guard.shutdown(Instant::now() + Duration::from_secs(2));
        let LoggerShutdown::Completed(snapshot) = result else {
            panic!("memory sink drain failed: {result:?}");
        };
        let records =
            String::from_utf8(sink.0.lock().expect("sink lock").clone()).expect("UTF-8 output");
        (records, snapshot)
    }

    fn payload(value: &str) {
        tracing::info!(payload = value);
    }

    #[test]
    fn complete_record_limit_includes_escaping_and_newline_in_both_formats() {
        for format in [LoggingFormat::Json, LoggingFormat::Text] {
            let (empty, _) = capture(format, || payload(""));
            let available = 16_384 - empty.len();
            let exact = "a".repeat(available);
            let over = "a".repeat(available + 1);
            let escaped = "\n".repeat(available / 2 + 1);
            let (records, snapshot) = capture(format, || {
                payload(&exact);
                payload(&over);
                payload(&escaped);
                payload("survivor");
            });
            let lines: Vec<_> = records.split_inclusive('\n').collect();
            assert_eq!(
                lines.len(),
                2,
                "oversized records are absent, not truncated"
            );
            assert_eq!(lines[0].len(), 16_384);
            assert!(lines[1].contains("survivor"));
            assert_eq!(snapshot.dropped[1], 2);
            if matches!(format, LoggingFormat::Json) {
                let json: serde_json::Value =
                    serde_json::from_str(lines[0]).expect("complete JSON");
                assert_eq!(json["payload"], exact);
            }
        }
    }

    struct Streaming<'a>(&'a AtomicUsize);

    impl fmt::Display for Streaming<'_> {
        fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
            for _ in 0..100_000 {
                self.0.fetch_add(1, Ordering::Relaxed);
                out.write_str("\n")?;
            }
            Ok(())
        }
    }

    impl fmt::Debug for Streaming<'_> {
        fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Display::fmt(self, out)
        }
    }

    #[test]
    fn debug_and_display_stop_serializing_at_the_owned_byte_limit() {
        for format in [LoggingFormat::Json, LoggingFormat::Text] {
            let debug_writes = AtomicUsize::new(0);
            let display_writes = AtomicUsize::new(0);
            let (records, snapshot) = capture(format, || {
                tracing::info!(value = ?Streaming(&debug_writes));
                tracing::info!(value = %Streaming(&display_writes));
                payload("survivor");
            });
            assert!(debug_writes.load(Ordering::Relaxed) <= 8192);
            assert!(display_writes.load(Ordering::Relaxed) <= 8192);
            assert_eq!(snapshot.dropped[1], 2);
            assert_eq!(records.lines().count(), 1);
            assert!(records.contains("survivor"));
        }
    }

    #[test]
    fn repeated_span_updates_compact_and_overflow_permanently_withholds_context() {
        for format in [LoggingFormat::Json, LoggingFormat::Text] {
            // JSON string quotes plus the span name consume six of 4096 bytes.
            let exact = "a".repeat(4090);
            let (records, snapshot) = capture(format, || {
                let span = tracing::info_span!("ok", value = "initial");
                for _ in 0..100 {
                    span.record("value", exact.as_str());
                }
                tracing::info!(parent: &span, "at_limit");
                span.record("value", "b".repeat(4091).as_str());
                span.record("value", "small again");
                tracing::info!(parent: &span, "must_stay_unavailable");
                span.in_scope(|| tracing::info!("current_parent_unavailable"));
                let rejected = tracing::info_span!("ok", value = "c".repeat(4091).as_str());
                tracing::info!(parent: &rejected, "initial_capture_unavailable");
                tracing::info_span!("healthy").in_scope(|| tracing::info!("survivor"));
            });
            assert_eq!(records.lines().count(), 2);
            assert!(records.contains("at_limit"));
            assert!(records.contains("survivor"));
            assert!(!records.contains("unavailable"));
            assert_eq!(
                snapshot.dropped[2], 5,
                "two capture failures and three unavailable events"
            );
        }
    }

    struct Reentrant<'a>(&'a Dispatch);

    impl fmt::Debug for Reentrant<'_> {
        fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
            use tracing::callsite::{DefaultCallsite, Identifier};
            use tracing::field::FieldSet;
            use tracing::metadata::Kind;

            static CALLSITE: DefaultCallsite = DefaultCallsite::new(&META);
            static META: tracing::Metadata<'static> = tracing::Metadata::new(
                "nested_event",
                "formatter_test",
                tracing::Level::WARN,
                None,
                None,
                None,
                FieldSet::new(&["message"], Identifier(&CALLSITE)),
                Kind::EVENT,
            );
            let message = "nested_formatter_event";
            let values = [Some(&message as &dyn tracing::field::Value)];
            // Explicit dispatch reaches the real layer even with scoped defaults,
            // whose get_default deliberately suppresses nested macro events.
            self.0
                .event(&Event::new(&META, &META.fields().value_set_all(&values)));
            out.write_str("admitted_outer_value")
        }
    }

    #[test]
    fn reentrant_event_and_span_delta_keep_outer_record_without_extension_deadlock() {
        for format in [LoggingFormat::Json, LoggingFormat::Text] {
            let (send, receive) = std::sync::mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                let result = capture(format, || {
                    let dispatch = tracing::dispatcher::get_default(Clone::clone);
                    tracing::info!(value = ?Reentrant(&dispatch));
                    let span = tracing::info_span!("update", value = tracing::field::Empty);
                    span.in_scope(|| {
                        span.record("value", tracing::field::debug(Reentrant(&dispatch)));
                    });
                    tracing::info!(parent: &span, "updated_span");
                });
                send.send(result).expect("test receiver remains alive");
            });
            let (records, snapshot) = receive
                .recv_timeout(Duration::from_secs(3))
                .expect("reentrant formatting must terminate");
            worker.join().expect("formatter thread completes");
            assert_eq!(records.lines().count(), 2);
            assert!(!records.contains("nested_formatter_event"));
            assert_eq!(records.matches("admitted_outer_value").count(), 2);
            assert_eq!(snapshot.dropped[4], 2);
        }
    }

    #[test]
    fn flattened_field_limit_counts_duplicate_span_names_before_deduplication() {
        for format in [LoggingFormat::Json, LoggingFormat::Text] {
            let (records, snapshot) = capture(format, || {
                let mut spans = vec![tracing::info_span!("root")];
                for _ in 1..127 {
                    let child = tracing::info_span!(parent: spans.last().expect("parent"), "child");
                    spans.push(child);
                }
                tracing::info!(parent: spans.last().expect("parent"), "at_field_limit");
                let overflow =
                    tracing::info_span!(parent: spans.last().expect("parent"), "overflow");
                tracing::info!(parent: &overflow, "must_be_dropped");
            });
            assert_eq!(records.lines().count(), 1);
            assert!(records.contains("at_field_limit"));
            assert_eq!(snapshot.dropped[1], 1);
        }
    }
    #[test]
    fn span_field_limit_includes_its_name_and_applies_to_later_additions() {
        use tracing::callsite::{DefaultCallsite, Identifier};
        use tracing::field::{FieldSet, Value};
        use tracing::metadata::Kind;

        static KEYS: [&str; 64] = [
            "f00", "f01", "f02", "f03", "f04", "f05", "f06", "f07", "f08", "f09", "f10", "f11",
            "f12", "f13", "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23",
            "f24", "f25", "f26", "f27", "f28", "f29", "f30", "f31", "f32", "f33", "f34", "f35",
            "f36", "f37", "f38", "f39", "f40", "f41", "f42", "f43", "f44", "f45", "f46", "f47",
            "f48", "f49", "f50", "f51", "f52", "f53", "f54", "f55", "f56", "f57", "f58", "f59",
            "f60", "f61", "f62", "f63",
        ];
        static CALLSITE: DefaultCallsite = DefaultCallsite::new(&META);
        static META: tracing::Metadata<'static> = tracing::Metadata::new(
            "many_fields",
            "formatter_test",
            tracing::Level::INFO,
            None,
            None,
            None,
            FieldSet::new(&KEYS, Identifier(&CALLSITE)),
            Kind::SPAN,
        );
        for format in [LoggingFormat::Json, LoggingFormat::Text] {
            let (records, snapshot) = capture(format, || {
                let number = 1_u64;
                let mut values: [Option<&dyn Value>; 64] = [Some(&number); 64];
                values[63] = None;
                let span = tracing::Span::new(&META, &META.fields().value_set_all(&values));
                tracing::info!(parent: &span, "at_span_field_limit");
                span.record("f00", 2_u64);
                tracing::info!(parent: &span, "existing_field_update");
                span.record("f63", 3_u64);
                tracing::info!(parent: &span, "addition_must_be_dropped");
                values[63] = Some(&number);
                let rejected = tracing::Span::new(&META, &META.fields().value_set_all(&values));
                tracing::info!(parent: &rejected, "initial_must_be_dropped");
            });
            assert_eq!(records.lines().count(), 2);
            assert!(records.contains("at_span_field_limit"));
            assert!(records.contains("existing_field_update"));
            assert!(!records.contains("must_be_dropped"));
            assert_eq!(snapshot.dropped[2], 4);
        }
    }
    #[test]
    fn escaped_field_keys_count_toward_the_complete_record_limit() {
        use tracing::callsite::{DefaultCallsite, Identifier};
        use tracing::field::FieldSet;
        use tracing::metadata::Kind;

        static KEY_BYTES: [u8; 8192] = [b'"'; 8192];
        static KEYS: [&str; 1] = [match std::str::from_utf8(&KEY_BYTES) {
            Ok(key) => key,
            Err(_) => unreachable!(),
        }];
        static CALLSITE: DefaultCallsite = DefaultCallsite::new(&META);
        static META: tracing::Metadata<'static> = tracing::Metadata::new(
            "large_key",
            "formatter_test",
            tracing::Level::INFO,
            None,
            None,
            None,
            FieldSet::new(&KEYS, Identifier(&CALLSITE)),
            Kind::EVENT,
        );
        for format in [LoggingFormat::Json, LoggingFormat::Text] {
            let (records, snapshot) = capture(format, || {
                let value = 1_u64;
                let values = [Some(&value as &dyn tracing::field::Value)];
                let fields = META.fields().value_set_all(&values);
                tracing::dispatcher::get_default(|dispatch| {
                    dispatch.event(&Event::new(&META, &fields));
                });
                payload("survivor");
            });
            assert_eq!(records.lines().count(), 1);
            assert!(records.contains("survivor"));
            assert_eq!(snapshot.dropped[1], 1);
        }
    }
    /// Runs the actual global limits in a fresh test process so saturation never
    /// rejects unrelated subscriber fixtures executing in the parent process.
    #[test]
    fn processwide_capacity_is_bounded_and_recovers() {
        const CHILD: &str = "RUST_TELEMETRY_FORMAT_CAPACITY_CHILD";
        if std::env::var_os(CHILD).as_deref() != Some(std::ffi::OsStr::new("1")) {
            let mut child =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args([
                        "--exact",
                        "logging::format::tests::processwide_capacity_is_bounded_and_recovers",
                        "--test-threads=1",
                    ])
                    .env(CHILD, "1")
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .expect("capacity test child starts");
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                if child.try_wait().expect("capacity child status").is_some() {
                    let output = child.wait_with_output().expect("capacity child reaped");
                    assert!(
                        String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
                        "the isolated test must run exactly its selected case: {}",
                        String::from_utf8_lossy(&output.stdout),
                    );
                    assert!(
                        output.status.success(),
                        "capacity child failed: stdout={} stderr={}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr),
                    );
                    return;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("capacity child exceeded its bounded deadline");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        for format in [LoggingFormat::Json, LoggingFormat::Text] {
            callback_capacity(format);
            span_capacity(format);
        }
    }

    fn callback_capacity(format: LoggingFormat) {
        use std::sync::{Condvar, mpsc};

        struct Held<'a> {
            entered: &'a mpsc::SyncSender<()>,
            release: &'a (Mutex<bool>, Condvar),
        }
        impl fmt::Display for Held<'_> {
            fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.entered
                    .send(())
                    .expect("callback entry receiver remains alive");
                let (lock, wake) = self.release;
                let (released, _) = wake
                    .wait_timeout_while(
                        lock.lock().expect("callback release lock"),
                        Duration::from_secs(5),
                        |released| !*released,
                    )
                    .expect("callback release wait");
                assert!(*released, "test must release every admitted callback");
                out.write_str("held_callback")
            }
        }

        let sink = Sink::default();
        let (output, guard) = output::start(sink.clone()).expect("writer starts");
        let dispatch = Dispatch::new(Registry::default().with(FormatLayer::new(output, format)));
        let (entered, observed) = mpsc::sync_channel(32);
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let mut workers = Vec::new();
        for _ in 0..32 {
            let dispatch = dispatch.clone();
            let entered = entered.clone();
            let release = Arc::clone(&release);
            workers.push(std::thread::spawn(move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    tracing::info!(value = %Held { entered: &entered, release: &release });
                });
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        for _ in 0..32 {
            observed
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("all 32 admitted callbacks reach source formatting");
        }
        let denied_writes = AtomicUsize::new(0);
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(value = %Streaming(&denied_writes));
        });
        // These observations are made while every admitted callback is stopped
        // inside its source Display, before it can finish or release a permit.
        assert!(!*release.0.lock().expect("callback release lock"));
        assert_eq!(guard.snapshot().dropped[3], 1, "the 33rd callback is busy");
        assert_eq!(
            denied_writes.load(Ordering::Relaxed),
            0,
            "busy rejects before formatting"
        );
        assert!(
            sink.0.lock().expect("sink lock").is_empty(),
            "no incomplete record reaches the writer"
        );
        *release.0.lock().expect("callback release lock") = true;
        release.1.notify_all();
        for worker in workers {
            worker.join().expect("released callback joins");
        }
        tracing::dispatcher::with_default(&dispatch, || payload("after_callback_release"));
        let result = guard.shutdown(Instant::now() + Duration::from_secs(2));
        let LoggerShutdown::Completed(snapshot) = result else {
            panic!("capacity writer drain failed: {result:?}");
        };
        let records =
            String::from_utf8(sink.0.lock().expect("sink lock").clone()).expect("UTF-8 output");
        assert_eq!(records.lines().count(), 33);
        assert_eq!(records.matches("held_callback").count(), 32);
        assert!(records.contains("after_callback_release"));
        assert!(
            records
                .split_inclusive('\n')
                .all(|line| line.len() <= 16_384)
        );
        assert_eq!(snapshot.dropped[3], 1);
    }

    fn span_capacity(format: LoggingFormat) {
        let (records, snapshot) = capture(format, || {
            // These are independent roots, so the event field-table ceiling
            // cannot be mistaken for cache-slot admission failure.
            let mut spans: Vec<_> = (0..1024)
                .map(|item| tracing::info_span!(parent: None, "held", item))
                .collect();
            tracing::info!(parent: spans.last().expect("1024th span"), "at_span_capacity");
            let rejected = tracing::info_span!(parent: None, "over_capacity", item = 1024);
            tracing::info!(parent: &rejected, "explicit_parent_must_be_dropped");
            rejected.in_scope(|| tracing::info!("current_parent_must_be_dropped"));
            drop(spans.pop());
            let recovered = tracing::info_span!(parent: None, "reused_slot");
            tracing::info!(parent: &recovered, "after_span_close");
            rejected.record("item", 0);
            tracing::info!(parent: &rejected, "rejected_span_must_remain_unavailable");
        });
        assert_eq!(records.lines().count(), 2);
        assert!(records.contains("at_span_capacity"));
        assert!(records.contains("after_span_close"));
        assert!(!records.contains("must_be_dropped"));
        assert!(!records.contains("must_remain_unavailable"));
        assert_eq!(
            snapshot.dropped[2], 4,
            "one rejected capture and three unavailable events"
        );
    }
}
