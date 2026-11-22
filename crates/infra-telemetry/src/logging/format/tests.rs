use super::super::output::{self, LogSnapshot, LoggerShutdown};
use super::super::tests::Buffer;
use super::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt as _;

fn capture(format: LoggingFormat, emit: impl FnOnce()) -> (String, LogSnapshot) {
    let buffer = Buffer::default();
    let (output, guard) = output::start(buffer.clone()).expect("writer starts");
    let dispatch = Dispatch::new(Registry::default().with(FormatLayer::new(output, format, false)));
    tracing::dispatcher::with_default(&dispatch, emit);
    let (result, snapshot) = guard.shutdown_with_snapshot(Instant::now() + Duration::from_secs(2));
    assert_eq!(
        result,
        LoggerShutdown::Completed,
        "memory sink drain failed"
    );
    (buffer.records(), snapshot)
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
        assert_eq!(lines.len(), 4, "oversized records are cut, not dropped");
        assert_eq!(lines[0].len(), 16_384);
        assert!(!lines[0].contains("truncated"));
        assert!(lines[3].contains("survivor"));
        assert!(
            snapshot.dropped.values().all(|&count| count == 0),
            "{snapshot:?}"
        );
        for cut in &lines[1..3] {
            assert!(cut.len() <= 16_384);
            assert!(is_marked(cut, format), "{cut}");
        }
        if matches!(format, LoggingFormat::Json) {
            let json: serde_json::Value = serde_json::from_str(lines[0]).expect("complete JSON");
            assert_eq!(json["payload"], exact);
            for (cut, whole) in [(lines[1], &over), (lines[2], &escaped)] {
                let json: serde_json::Value =
                    serde_json::from_str(cut).expect("a cut record is still complete JSON");
                let kept = json["payload"]
                    .as_str()
                    .expect("the payload stays a string");
                assert!(!kept.is_empty() && kept.len() < whole.len());
                assert!(whole.starts_with(kept));
            }
        }
    }
}

fn is_marked(line: &str, format: LoggingFormat) -> bool {
    line.trim_end()
        .ends_with(if matches!(format, LoggingFormat::Json) {
            ",\"truncated\":true}"
        } else {
            " \"truncated\"=true"
        })
}

#[test]
fn a_value_cut_between_characters_stays_valid_text_and_names_the_cut() {
    for format in [LoggingFormat::Json, LoggingFormat::Text] {
        let escaped = "é\n".repeat(20_000);
        let plain = "😀".repeat(20_000);
        let (records, snapshot) = capture(format, || {
            payload(&escaped);
            payload(&plain);
            tracing::info!(truncated = false, "own_field_cannot_spoof_the_marker");
        });
        assert!(
            snapshot.dropped.values().all(|&count| count == 0),
            "{snapshot:?}"
        );
        let lines: Vec<_> = records.lines().collect();
        assert_eq!(lines.len(), 3);
        for (line, original) in lines[..2].iter().zip([&escaped, &plain]) {
            assert!(line.len() < 16_384 && is_marked(line, format));
            if matches!(format, LoggingFormat::Json) {
                let json: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
                let kept = json["payload"].as_str().expect("a string");
                assert!(kept.len() > 10_000 && original.starts_with(kept));
            }
        }
        assert!(!lines[2].contains("truncated"), "{}", lines[2]);
    }
}

#[test]
fn captured_strings_are_escaped_as_serde_json_escapes_them() {
    let mut all: String = (0_u8..=0x7f).map(char::from).collect();
    all.push_str("é€😀");
    let (records, _) = capture(LoggingFormat::Json, || payload(&all));
    let expected = format!("\"payload\":{}", serde_json::to_string(&all).unwrap());
    assert!(records.contains(&expected), "{records}");
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
        assert!(
            snapshot.dropped.values().all(|&count| count == 0),
            "{snapshot:?}"
        );
        let lines: Vec<_> = records.lines().collect();
        assert_eq!(lines.len(), 3, "the streamed values are cut, not dropped");
        assert!(lines[..2].iter().all(|line| is_marked(line, format)));
        assert!(lines[2].contains("survivor"));
    }
}

#[test]
fn repeated_span_updates_compact_and_overflow_permanently_withholds_context() {
    for format in [LoggingFormat::Json, LoggingFormat::Text] {
        let exact = "a".repeat(4083);
        let previous = "b".repeat(4083);
        let (records, snapshot) = capture(format, || {
            let span = tracing::info_span!("ok", value = "initial", kind = "email");
            for _ in 0..100 {
                span.record("value", previous.as_str());
            }
            span.record("value", exact.as_str());
            tracing::info!(parent: &span, "at_limit");
            span.record("value", "b".repeat(4084).as_str());
            span.record("value", "small again");
            tracing::info!(parent: &span, "must_stay_unavailable");
            span.in_scope(|| tracing::info!("current_parent_unavailable"));
            let rejected = tracing::info_span!("ok", value = "c".repeat(4091).as_str());
            tracing::info!(parent: &rejected, "initial_capture_unavailable");
            tracing::info_span!("healthy").in_scope(|| tracing::info!("survivor"));
        });
        assert_eq!(records.lines().count(), 5);
        let (unavailable, whole): (Vec<_>, Vec<_>) = records
            .lines()
            .partition(|line| line.contains("unavailable"));
        let at_limit = whole.iter().find(|line| line.contains("at_limit")).unwrap();
        assert!(at_limit.contains(&exact));
        assert!(!at_limit.contains(&previous));
        assert!(at_limit.contains("email"));
        assert_eq!(unavailable.len(), 3);
        assert!(
            unavailable.iter().all(|line| is_marked(line, format)),
            "the events of a span without a cache are written without its fields"
        );
        assert!(whole.iter().all(|line| !line.contains("truncated")));
        assert!(records.contains("survivor"));
        assert_eq!(
            snapshot.dropped(DropReason::SpanCapacity),
            2,
            "two span caches refused"
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
        assert_eq!(snapshot.dropped(DropReason::Reentrant), 2);
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
            let overflow = tracing::info_span!(parent: spans.last().expect("parent"), "overflow");
            tracing::info!(parent: &overflow, "past_field_limit");
        });
        let lines: Vec<_> = records.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("at_field_limit") && !lines[0].contains("truncated"));
        assert!(lines[1].contains("past_field_limit") && is_marked(lines[1], format));
        assert!(
            snapshot.dropped.values().all(|&count| count == 0),
            "{snapshot:?}"
        );
    }
}
#[test]
fn span_field_limit_includes_its_name_and_applies_to_later_additions() {
    use tracing::callsite::{DefaultCallsite, Identifier};
    use tracing::field::{FieldSet, Value};
    use tracing::metadata::Kind;

    static KEYS: [&str; 64] = [
        "f00", "f01", "f02", "f03", "f04", "f05", "f06", "f07", "f08", "f09", "f10", "f11", "f12",
        "f13", "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24", "f25",
        "f26", "f27", "f28", "f29", "f30", "f31", "f32", "f33", "f34", "f35", "f36", "f37", "f38",
        "f39", "f40", "f41", "f42", "f43", "f44", "f45", "f46", "f47", "f48", "f49", "f50", "f51",
        "f52", "f53", "f54", "f55", "f56", "f57", "f58", "f59", "f60", "f61", "f62", "f63",
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
        let lines: Vec<_> = records.lines().collect();
        assert_eq!(lines.len(), 4);
        for (line, name, marked) in [
            (lines[0], "at_span_field_limit", false),
            (lines[1], "existing_field_update", false),
            (lines[2], "addition_must_be_dropped", true),
            (lines[3], "initial_must_be_dropped", true),
        ] {
            assert!(line.contains(name), "{line}");
            assert_eq!(is_marked(line, format), marked, "{line}");
        }
        assert_eq!(
            snapshot.dropped(DropReason::SpanCapacity),
            2,
            "two span caches refused"
        );
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
        let lines: Vec<_> = records.split_inclusive('\n').collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].len() <= 16_384 && is_marked(lines[0], format));
        assert!(!lines[0].contains("\\\"\\\""), "the oversize key stays out");
        assert!(lines[1].contains("survivor"));
        assert!(
            snapshot.dropped.values().all(|&count| count == 0),
            "{snapshot:?}"
        );
    }
}
#[test]
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "processwide capacity test isolates its global subscriber in an owned child process"
)]
fn processwide_capacity_is_bounded_and_recovers() {
    const CHILD: &str = "RUST_TELEMETRY_FORMAT_CAPACITY_CHILD";
    if std::env::var_os(CHILD).as_deref() != Some(std::ffi::OsStr::new("1")) {
        let child = std::process::Command::new(std::env::current_exe().expect("test executable"))
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
        let output = super::super::wait_for_child(child, Duration::from_secs(20));
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

    let buffer = Buffer::default();
    let (output, guard) = output::start(buffer.clone()).expect("writer starts");
    let dispatch = Dispatch::new(Registry::default().with(FormatLayer::new(output, format, false)));
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
    assert!(!*release.0.lock().expect("callback release lock"));
    assert_eq!(
        guard.snapshot().dropped(DropReason::Busy),
        1,
        "the 33rd callback is busy"
    );
    assert_eq!(
        denied_writes.load(Ordering::Relaxed),
        0,
        "busy rejects before formatting"
    );
    assert!(
        buffer.records().is_empty(),
        "no incomplete record reaches the writer"
    );
    *release.0.lock().expect("callback release lock") = true;
    release.1.notify_all();
    for worker in workers {
        worker.join().expect("released callback joins");
    }
    tracing::dispatcher::with_default(&dispatch, || payload("after_callback_release"));
    let (result, snapshot) = guard.shutdown_with_snapshot(Instant::now() + Duration::from_secs(2));
    assert_eq!(
        result,
        LoggerShutdown::Completed,
        "capacity writer drain failed"
    );
    let records = buffer.records();
    assert_eq!(records.lines().count(), 33);
    assert_eq!(records.matches("held_callback").count(), 32);
    assert!(records.contains("after_callback_release"));
    assert!(
        records
            .split_inclusive('\n')
            .all(|line| line.len() <= 16_384)
    );
    assert_eq!(snapshot.dropped(DropReason::Busy), 1);
}

fn span_capacity(format: LoggingFormat) {
    let (records, snapshot) = capture(format, || {
        let mut spans = Vec::from_fn(1024, |item| tracing::info_span!(parent: None, "held", item));
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
    let lines: Vec<_> = records.lines().collect();
    assert_eq!(lines.len(), 5);
    for line in lines {
        let marked = line.contains("_must_");
        assert_eq!(is_marked(line, format), marked, "{line}");
        assert!(marked || line.contains("at_span_capacity") || line.contains("after_span_close"));
    }
    assert_eq!(
        snapshot.dropped(DropReason::SpanCapacity),
        1,
        "one rejected capture"
    );
}

#[test]
fn a_buffer_reserves_no_more_than_its_limit() {
    let mut bytes = Bytes::with_capacity(100, 8);
    for _ in 0..40 {
        bytes.append(b"abc");
    }
    assert_eq!(bytes.len(), 99);
    assert!(bytes.failed);
    assert!(bytes.buf.capacity() <= 100, "{}", bytes.buf.capacity());
}

#[test]
fn single_new_span_field_appends_in_place_preserving_order_and_bounds() {
    let mut initial = SpanScratch::new(MAX_SPAN_BYTES);
    let large = "v".repeat(MAX_SPAN_BYTES / 2 + 512);
    assert!(initial.push_json("f0", large.as_str()));
    for key in ["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"] {
        assert!(initial.push_json(key, "v"));
    }
    let slot = Permit::take(&CACHED_SPANS, MAX_CACHED_SPANS).expect("span slot");
    let mut cached = SpanFields::from_capture(slot, &initial).expect("initial span cache");
    assert_eq!(cached.fields.len(), 9);
    assert!(cached.values.capacity() > MAX_SPAN_BYTES / 2);
    let fields_ptr = cached.fields.as_ptr();
    let fields_cap = cached.fields.capacity();
    assert!(fields_cap > cached.fields.len());

    let empty = SpanScratch::new(MAX_SPAN_BYTES);
    assert!(cached.apply(&empty));
    assert_eq!(cached.fields.as_ptr(), fields_ptr);
    assert_eq!(cached.fields.len(), 9);

    let mut failed_empty = SpanScratch::new(MAX_SPAN_BYTES);
    assert!(!failed_empty.push_json("too_big", "z".repeat(MAX_SPAN_BYTES).as_str()));
    assert!(failed_empty.values.failed && failed_empty.fields.is_empty());
    assert!(!cached.apply(&failed_empty));
    assert_eq!(cached.fields.len(), 9);

    let mut failed_after_one = SpanScratch::new(MAX_SPAN_BYTES);
    assert!(failed_after_one.push_json("kept", "ok"));
    assert!(!failed_after_one.push_json("too_big", "z".repeat(MAX_SPAN_BYTES).as_str()));
    assert!(failed_after_one.values.failed && failed_after_one.fields.len() == 1);
    assert!(!cached.apply(&failed_after_one));
    assert_eq!(cached.fields.len(), 9);

    let mut delta = SpanScratch::new(MAX_SPAN_BYTES);
    assert!(delta.push_json("cache.outcome", "hit"));
    assert!(cached.apply(&delta));
    assert_eq!(cached.fields.as_ptr(), fields_ptr);
    assert_eq!(cached.fields.capacity(), fields_cap);
    assert_eq!(cached.fields.len(), 10);
    assert_eq!(cached.fields[0].key, "cache.outcome");
    assert_eq!(&cached.values[cached.fields[0].range()], b"\"hit\"");
    assert_eq!(cached.fields[1].key, "f0");
    assert!(cached.values.capacity() <= MAX_SPAN_BYTES);

    let remaining = MAX_SPAN_BYTES - cached.values.len();
    let mut exact_delta = SpanScratch::new(MAX_SPAN_BYTES);
    let exact_str = "x".repeat(remaining - 2);
    assert!(exact_delta.push_json("fill", exact_str.as_str()));
    assert!(cached.apply(&exact_delta));
    assert_eq!(cached.values.len(), MAX_SPAN_BYTES);
    assert_eq!(cached.values.capacity(), MAX_SPAN_BYTES);
    assert_eq!(cached.fields[0].key, "fill");

    let mut overflow_delta = SpanScratch::new(MAX_SPAN_BYTES);
    assert!(overflow_delta.push_json("one_more", "y"));
    assert!(!cached.apply(&overflow_delta));

    let mut overwrite = SpanScratch::new(MAX_SPAN_BYTES);
    assert!(overwrite.push_json("fill", "tiny"));
    assert!(cached.apply(&overwrite));
    assert!(cached.values.len() < MAX_SPAN_BYTES);
    assert_eq!(cached.fields[0].key, "fill");
    assert_eq!(&cached.values[cached.fields[0].range()], b"\"tiny\"");
}

#[test]
fn span_field_filtering_preserves_precedence_with_duplicates_and_truncation() {
    use tracing::callsite::{DefaultCallsite, Identifier};
    use tracing::field::{FieldSet, Value};
    use tracing::metadata::Kind;

    static EVENT_KEYS: [&str; 6] = ["dup", "dup", "dup", "mid", "payload", "zz0"];
    static CALLSITE: DefaultCallsite = DefaultCallsite::new(&META);
    static META: tracing::Metadata<'static> = tracing::Metadata::new(
        "dup_event",
        "formatter_test",
        tracing::Level::INFO,
        None,
        None,
        None,
        FieldSet::new(&EVENT_KEYS, Identifier(&CALLSITE)),
        Kind::EVENT,
    );

    let (records, snapshot) = capture(LoggingFormat::Json, || {
        let zzz_value = "z".repeat(350);
        let span = tracing::info_span!(
            "outer",
            aaa = "before_event",
            dup = "shadowed_by_dup",
            mmm = "between_event",
            mid = "shadowed_by_mid",
            zzz = zzz_value.as_str(),
        );
        let first = 1_u64;
        let second = 2_u64;
        let third = 3_u64;
        let mid_val = "from_event";
        let zz0_val = "from_event_end";
        let short_payload = "ok";
        let rewind_payload = "p".repeat(15_900);
        span.in_scope(|| {
            for payload_val in [short_payload, rewind_payload.as_str()] {
                let values: [Option<&dyn Value>; 6] = [
                    Some(&first),
                    Some(&second),
                    Some(&third),
                    Some(&mid_val),
                    Some(&payload_val),
                    Some(&zz0_val),
                ];
                let fields = META.fields().value_set_all(&values);
                tracing::dispatcher::get_default(|dispatch| {
                    dispatch.event(&Event::new(&META, &fields));
                });
            }
        });
    });

    assert!(snapshot.dropped.values().all(|&count| count == 0));
    let lines: Vec<_> = records.lines().collect();
    assert_eq!(lines.len(), 2);

    let whole: serde_json::Value = serde_json::from_str(lines[0]).expect("valid whole JSON");
    assert_eq!(whole["dup"], 3);
    assert_eq!(whole["mid"], "from_event");
    assert_eq!(whole["zz0"], "from_event_end");
    assert_eq!(whole["aaa"], "before_event");
    assert_eq!(whole["mmm"], "between_event");
    assert_eq!(whole["zzz"].as_str().map(str::len), Some(350));
    assert!(!lines[0].contains("shadowed_by_"));

    assert!(is_marked(lines[1], LoggingFormat::Json));
    assert_eq!(lines[1].len() + 1, MAX_RECORD_BYTES);
    let cut: serde_json::Value = serde_json::from_str(lines[1]).expect("valid truncated JSON");
    assert_eq!(cut["dup"], 3);
    assert_eq!(cut["mid"], "from_event");
    assert_eq!(cut["zz0"], "from_event_end");
    assert_eq!(cut["aaa"], "before_event");
    assert_eq!(cut["mmm"], "between_event");
    let cut_zzz = cut["zzz"]
        .as_str()
        .expect("second pass must still reach and truncate zzz");
    assert!(!cut_zzz.is_empty() && cut_zzz.len() < 350 && cut_zzz.bytes().all(|b| b == b'z'));
    assert!(!lines[1].contains("shadowed_by_"));
}
