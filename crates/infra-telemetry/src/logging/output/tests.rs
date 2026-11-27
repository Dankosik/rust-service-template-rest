use super::*;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, SyncSender};

const TEST_TIMEOUT: Duration = Duration::from_secs(3);

fn record(bytes: &[u8]) -> Record {
    bytes.into()
}

struct GatedWriter {
    entered: SyncSender<()>,
    release: Option<Receiver<()>>,
    bytes: Arc<Mutex<Vec<u8>>>,
    released: Arc<AtomicBool>,
    destroyed: Arc<AtomicBool>,
    fail_first: bool,
    written: SyncSender<()>,
}

impl Write for GatedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(release) = self.release.take() {
            self.entered.send(()).expect("signal first write");
            release
                .recv_timeout(TEST_TIMEOUT)
                .expect("release blocked test writer");
            self.released.store(true, Ordering::SeqCst);
            if self.fail_first {
                return Err(io::Error::other("private test sink failure"));
            }
        }
        self.bytes
            .lock()
            .expect("test output mutex")
            .extend_from_slice(bytes);
        let _ = self.written.try_send(());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for GatedWriter {
    fn drop(&mut self) {
        self.destroyed.store(true, Ordering::SeqCst);
    }
}

struct Gate {
    entered: Receiver<()>,
    release: SyncSender<()>,
    bytes: Arc<Mutex<Vec<u8>>>,
    released: Arc<AtomicBool>,
    destroyed: Arc<AtomicBool>,
    written: Receiver<()>,
}

fn gated(fail_first: bool) -> (GatedWriter, Gate) {
    let (entered_tx, entered) = mpsc::sync_channel(1);
    let (release, release_rx) = mpsc::sync_channel(1);
    let (written_tx, written) = mpsc::sync_channel(1);
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let released = Arc::new(AtomicBool::new(false));
    let destroyed = Arc::new(AtomicBool::new(false));
    (
        GatedWriter {
            entered: entered_tx,
            release: Some(release_rx),
            bytes: Arc::clone(&bytes),
            released: Arc::clone(&released),
            destroyed: Arc::clone(&destroyed),
            fail_first,
            written: written_tx,
        },
        Gate {
            entered,
            release,
            bytes,
            released,
            destroyed,
            written,
        },
    )
}

#[test]
fn stopped_sink_bounds_the_queue_and_resumes_fifo_without_retrying_losses() {
    let (writer, gate) = gated(false);
    let (output, guard) = start(writer).expect("start writer");
    output.submit(record(b"first\n"));
    gate.entered
        .recv_timeout(TEST_TIMEOUT)
        .expect("writer entered sink");

    for index in 0..512 {
        output.submit(record(format!("{index}\n").as_bytes()));
    }
    output.submit(record(b"lost\n"));
    assert!(
        !gate.released.load(Ordering::SeqCst),
        "admission did not wait for sink"
    );
    assert_eq!(guard.snapshot().dropped.into_array(), [1, 0, 0, 0, 0, 0]);

    guard.begin_shutdown();
    gate.release.send(()).expect("resume sink");
    let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
    assert_eq!(result, LoggerShutdown::Completed, "historical loss");
    assert!(
        gate.destroyed.load(Ordering::SeqCst),
        "ack follows sink destruction"
    );
    let expected = std::iter::once("first\n".to_owned())
        .chain((0..512).map(|index| format!("{index}\n")))
        .collect::<String>();
    assert_eq!(
        *gate.bytes.lock().expect("test output mutex"),
        expected.as_bytes()
    );
    output.submit(record(b"after closure\n"));
    assert_eq!(
        *gate.bytes.lock().expect("test output mutex"),
        expected.as_bytes()
    );
}

#[test]
fn in_flight_failure_is_final_only_when_it_finishes_after_the_marker() {
    for during_drain in [false, true] {
        let (writer, gate) = gated(true);
        let (output, guard) = start(writer).expect("start writer");
        output.submit(record(b"failed\n"));
        gate.entered
            .recv_timeout(TEST_TIMEOUT)
            .expect("writer entered sink");
        output.submit(record(b"recovered\n"));
        if during_drain {
            guard.begin_shutdown();
        }
        gate.release.send(()).expect("resume sink");
        gate.written
            .recv_timeout(TEST_TIMEOUT)
            .expect("later record written");
        guard.begin_shutdown();
        let (result, snapshot) = guard.shutdown_with_snapshot(Instant::now() + TEST_TIMEOUT);
        if during_drain {
            assert_eq!(
                result,
                LoggerShutdown::Incomplete(LoggerShutdownReasons::WRITE),
                "in-flight failure cannot be hidden by recovery"
            );
        } else {
            assert_eq!(
                result,
                LoggerShutdown::Completed,
                "historical write failure cannot poison clean drain"
            );
        }
        assert_eq!(snapshot.sink_errors.into_array(), [1, 0, 0]);
        assert_eq!(
            &*gate.bytes.lock().expect("test output mutex"),
            b"recovered\n"
        );
    }
}

struct TornWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
    writes: usize,
}

impl Write for TornWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        let accepted = match self.writes {
            1 => 3.min(bytes.len()),
            2 => return Err(io::Error::other("private test sink failure")),
            _ => bytes.len(),
        };
        self.bytes
            .lock()
            .expect("test output mutex")
            .extend_from_slice(&bytes[..accepted]);
        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_line_after_a_partly_written_one_starts_on_its_own_line() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let writer = TornWriter {
        bytes: Arc::clone(&bytes),
        writes: 0,
    };
    let (output, guard) = start(writer).expect("start writer");
    output.submit(record(b"first\n"));
    output.submit(record(b"second\n"));
    guard.begin_shutdown();
    let (_result, snapshot) = guard.shutdown_with_snapshot(Instant::now() + TEST_TIMEOUT);

    assert_eq!(snapshot.sink_errors.into_array(), [1, 0, 0]);
    assert_eq!(&*bytes.lock().expect("test output mutex"), b"fir\nsecond\n");
}

#[test]
fn final_record_loss_survives_successful_sink_cleanup() {
    let (writer, gate) = gated(false);
    let (output, guard) = start(writer).expect("start writer");
    output.submit(record(b"first\n"));
    gate.entered
        .recv_timeout(TEST_TIMEOUT)
        .expect("writer entered sink");
    let queued = numbered(QUEUE_RECORDS);
    for line in &queued {
        output.submit(record(line.as_bytes()));
    }
    guard.begin_shutdown();
    output.submit(record(b"terminal\n"));
    gate.release.send(()).expect("resume sink");
    let (result, snapshot) = guard.shutdown_with_snapshot(Instant::now() + TEST_TIMEOUT);
    assert_eq!(
        result,
        LoggerShutdown::Incomplete(LoggerShutdownReasons::DROPPED),
        "a lost final record makes the drain incomplete"
    );
    assert_eq!(snapshot.dropped.into_array(), [1, 0, 0, 0, 0, 0]);
    assert!(gate.destroyed.load(Ordering::SeqCst));
    assert_eq!(
        *gate.bytes.lock().expect("test output mutex"),
        format!("first\n{}", queued.concat()).as_bytes()
    );
}

#[test]
fn deadline_and_guard_drop_return_while_the_sink_is_still_blocked() {
    for explicit_shutdown in [false, true] {
        let (writer, gate) = gated(false);
        let (output, mut guard) = start(writer).expect("start writer");
        output.submit(record(b"blocked\n"));
        gate.entered
            .recv_timeout(TEST_TIMEOUT)
            .expect("writer entered sink");
        let worker = guard.worker.take().expect("owned writer thread");
        let began = Instant::now();
        if explicit_shutdown {
            let result = guard.shutdown(began + Duration::from_millis(20));
            assert_eq!(
                result,
                LoggerShutdown::Incomplete(LoggerShutdownReasons::DEADLINE),
                "blocked writer cannot complete"
            );
        } else {
            drop(guard);
        }
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "bounded control wait"
        );
        assert!(
            !gate.released.load(Ordering::SeqCst),
            "sink remains blocked"
        );
        assert!(!worker.is_finished());
        output.submit(record(b"rejected after close\n"));
        gate.release.send(()).expect("release fixture for cleanup");
        drop(output);
        worker.join().expect("join released fixture");
        assert_eq!(
            &*gate.bytes.lock().expect("test output mutex"),
            b"blocked\n"
        );
    }
}

#[test]
fn final_flush_failure_and_worker_panic_have_finite_receipts() {
    struct FailingWriter(bool);
    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            assert!(!self.0, "private writer panic payload");
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("private flush error"))
        }
    }
    for panics in [false, true] {
        let (output, guard) = start(FailingWriter(panics)).expect("start writer");
        guard.begin_shutdown();
        output.submit(record(b"terminal\n"));
        let (result, snapshot) = guard.shutdown_with_snapshot(Instant::now() + TEST_TIMEOUT);
        if panics {
            assert_eq!(
                result,
                LoggerShutdown::Incomplete(LoggerShutdownReasons::WORKER)
            );
            assert_eq!(snapshot.sink_errors.into_array(), [0, 0, 1]);
        } else {
            assert_eq!(
                result,
                LoggerShutdown::Incomplete(LoggerShutdownReasons::FLUSH)
            );
            assert_eq!(snapshot.sink_errors(SinkOperation::Write), 0);
            assert!(snapshot.sink_errors(SinkOperation::Flush) >= 1);
            assert_eq!(snapshot.sink_errors(SinkOperation::Worker), 0);
        }
        assert!(!format!("{result:?}").contains("private"));
    }
}

struct CountingWriter {
    sizes: mpsc::Sender<usize>,
    bytes: Arc<Mutex<Vec<u8>>>,
    first_write: Option<(SyncSender<()>, Receiver<()>)>,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some((entered, release)) = self.first_write.take() {
            entered.send(()).expect("signal first write");
            release
                .recv_timeout(TEST_TIMEOUT)
                .expect("release blocked test writer");
        }
        self.sizes.send(bytes.len()).expect("report sink write");
        self.bytes
            .lock()
            .expect("test output mutex")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Counted {
    sizes: Receiver<usize>,
    bytes: Arc<Mutex<Vec<u8>>>,
}

type TestGate = Option<(Receiver<()>, SyncSender<()>)>;

fn counting(block_first: bool) -> (CountingWriter, Counted, TestGate) {
    let (sizes_tx, sizes) = mpsc::channel();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (writer_gate, test_gate) = if block_first {
        let (entered_tx, entered) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        (Some((entered_tx, release_rx)), Some((entered, release)))
    } else {
        (None, None)
    };
    (
        CountingWriter {
            sizes: sizes_tx,
            bytes: Arc::clone(&bytes),
            first_write: writer_gate,
        },
        Counted { sizes, bytes },
        test_gate,
    )
}

fn numbered(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("{index}\n")).collect()
}

#[test]
fn queued_burst_reaches_the_sink_in_few_ordered_bounded_writes() {
    let (writer, counted, gate) = counting(true);
    let (entered, release) = gate.expect("blocking gate");
    let (output, guard) = start(writer).expect("start writer");
    output.submit(record(b"first\n"));
    entered
        .recv_timeout(TEST_TIMEOUT)
        .expect("writer entered sink");
    let lines = numbered(QUEUE_RECORDS);
    let padded = [b'x'; 10 * 1024];
    for line in &lines {
        let mut bytes = padded.to_vec();
        bytes.extend_from_slice(line.as_bytes());
        output.submit(bytes);
    }
    assert_eq!(guard.snapshot().dropped.into_array(), [0; 6]);
    release.send(()).expect("resume sink");
    let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
    assert_eq!(result, LoggerShutdown::Completed);

    let expected = std::iter::once(b"first\n".to_vec())
        .chain(lines.iter().map(|line| {
            let mut bytes = padded.to_vec();
            bytes.extend_from_slice(line.as_bytes());
            bytes
        }))
        .flatten()
        .collect::<Vec<u8>>();
    assert_eq!(*counted.bytes.lock().expect("test output mutex"), expected);
    let sizes: Vec<_> = counted.sizes.try_iter().collect();
    assert!(
        sizes.iter().all(|&size| size <= MAX_BATCH_BYTES),
        "{sizes:?}"
    );
    assert!(
        sizes.len() <= 1 + QUEUE_RECORDS / 4,
        "{} writes for {} records",
        sizes.len(),
        QUEUE_RECORDS + 1
    );
}

#[test]
#[warn(clippy::let_underscore_must_use)]
fn records_further_apart_than_the_window_are_written_one_by_one() {
    let (writer, counted, _) = counting(false);
    let (output, guard) = start(writer).expect("start writer");
    for index in 0..3 {
        output.submit(record(format!("{index}\n").as_bytes()));
        assert_eq!(counted.sizes.recv_timeout(TEST_TIMEOUT), Ok(2));
        assert_eq!(
            counted.sizes.recv_timeout(BATCH_WINDOW * 5),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
    }
    let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
    assert_eq!(result, LoggerShutdown::Completed);
    assert_eq!(counted.sizes.try_iter().count(), 0);
}

#[test]
fn a_shallow_queue_is_gathered_for_the_whole_window() {
    let (sender, records) = crossbeam_channel::bounded(QUEUE_RECORDS);
    sender.send(record(b"one\n")).expect("queue a record");
    let window = Duration::from_millis(20);
    let started = Instant::now();

    wait_for_backlog(&records, window);

    assert!(started.elapsed() >= window);
}

#[test]
fn a_queue_that_grows_deep_ends_the_gathering_before_its_window() {
    let (sender, records) = crossbeam_channel::bounded(QUEUE_RECORDS);
    let (open, _closed) = crossbeam_channel::bounded(0);
    let (gathered_tx, gathered) = mpsc::sync_channel(1);
    let waiter = thread::spawn(move || {
        wait_for_backlog(&records, Duration::from_hours(1));
        gathered_tx.send(records.len()).expect("report the queue");
    });
    let output = Output {
        sender,
        shared: Arc::new(Shared {
            shutdown_gate: RwLock::new(Some(open)),
            drain_state: AtomicU32::new(0),
            counters: Counters::new(),
        }),
        worker_thread: waiter.thread().clone(),
    };

    for _ in 0..DEEP_QUEUE_RECORDS {
        output.submit(record(b"line\n"));
    }

    assert_eq!(gathered.recv_timeout(TEST_TIMEOUT), Ok(DEEP_QUEUE_RECORDS));
}

#[derive(Clone, Copy, Debug)]
enum WriteStep {
    Accept(usize),
    Fail,
}

struct ScriptedSink {
    steps: std::collections::VecDeque<WriteStep>,
    bytes: Vec<u8>,
}

impl Write for ScriptedSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let accepted = match self.steps.pop_front() {
            Some(WriteStep::Fail) => return Err(io::Error::other("private test sink failure")),
            Some(WriteStep::Accept(limit)) => limit.min(bytes.len()),
            None => bytes.len(),
        };
        self.bytes.extend_from_slice(&bytes[..accepted]);
        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_batch_cut_short_loses_one_record_and_keeps_later_ones_on_their_own_lines() {
    use WriteStep::{Accept, Fail};
    type Case = (bool, &'static [WriteStep], &'static [u8]);
    let cases: [Case; 6] = [
        (false, &[Fail], b"bb\ncc\ndd\n"),
        (false, &[Accept(3), Fail], b"aa\ncc\ndd\n"),
        (false, &[Accept(4), Fail], b"aa\nb\ncc\ndd\n"),
        (false, &[Accept(7), Fail], b"aa\nbb\nc\ndd\n"),
        (true, &[Fail], b"\nbb\ncc\ndd\n"),
        (true, &[Accept(1), Fail], b"\nbb\ncc\ndd\n"),
    ];
    for (after_torn_line, steps, expected) in cases {
        let shared = Shared {
            shutdown_gate: RwLock::new(None),
            drain_state: AtomicU32::new(0),
            counters: Counters::new(),
        };
        let mut sink = ScriptedSink {
            steps: steps.iter().copied().collect(),
            bytes: Vec::new(),
        };
        let mut batch = Batch {
            torn: after_torn_line,
            ..Batch::default()
        };

        for line in [b"aa\n", b"bb\n", b"cc\n"] {
            batch.push(line);
        }
        batch.write(&mut sink, &shared);
        batch.clear();
        batch.push(b"dd\n");
        batch.write(&mut sink, &shared);

        assert_eq!(
            sink.bytes.as_slice(),
            expected,
            "after_torn_line={after_torn_line} steps={steps:?}"
        );
        assert_eq!(
            shared.counters.snapshot().sink_errors.into_array(),
            [1, 0, 0],
            "after_torn_line={after_torn_line} steps={steps:?}"
        );
    }
}

#[test]
fn shutdown_writes_every_accepted_record_in_order() {
    let (writer, counted, _) = counting(false);
    let (output, guard) = start(writer).expect("start writer");
    let lines = numbered(400);
    for line in &lines {
        output.submit(record(line.as_bytes()));
    }
    let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
    assert_eq!(result, LoggerShutdown::Completed);
    assert_eq!(
        *counted.bytes.lock().expect("test output mutex"),
        lines.concat().as_bytes()
    );
    assert!(counted.sizes.try_iter().count() <= lines.len());
}

#[test]
fn loss_before_recorder_installation_is_published_without_double_counting() {
    use metrics::{Key, Level, Metadata, Recorder as _};
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    const RESTORE_METADATA: Metadata<'static> = Metadata::new(module_path!(), Level::INFO, None);

    let (output, guard) = start(io::sink()).expect("start writer");
    let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
    assert_eq!(result, LoggerShutdown::Completed);
    output.submit(record(b"closed\n"));
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let target = Key::from_parts(DROPPED_METRIC, &[("reason", "closed")]);
    metrics::with_local_recorder(&recorder, || {
        for _ in 0..2 {
            let before = TOTALS.snapshot().dropped(DropReason::Closed);
            publish();
            let after = TOTALS.snapshot().dropped(DropReason::Closed);
            let snapshot = snapshotter.snapshot().into_vec();
            let mut closed = None;
            for (composite, _, _, value) in &snapshot {
                let DebugValue::Counter(count) = value else {
                    continue;
                };
                if *count != 0 {
                    recorder
                        .register_counter(composite.key(), &RESTORE_METADATA)
                        .increment(*count);
                }
                if composite.key() == &target {
                    closed = Some(*count);
                }
            }
            let value = closed.expect("pre-recorder loss has a finite metric");
            assert!(
                (before..=after).contains(&value),
                "{value}; expected {before}..={after}"
            );
        }
    });
}

#[tokio::test]
async fn async_shutdown_completes_and_enforces_deadline() {
    let (writer, gate) = gated(false);
    let (output, mut guard) = start(writer).expect("start writer");
    output.submit(record(b"blocked\n"));
    gate.entered
        .recv_timeout(TEST_TIMEOUT)
        .expect("writer entered sink");
    let worker = guard.worker.take().expect("owned writer thread");
    let timed_out = guard
        .shutdown_async(tokio::time::Instant::now() + Duration::from_millis(20))
        .await;
    assert_eq!(
        timed_out,
        LoggerShutdown::Incomplete(LoggerShutdownReasons::DEADLINE)
    );
    gate.release.send(()).expect("release fixture for cleanup");
    drop(output);
    worker.join().expect("join released fixture");

    let (output, guard) = start(io::sink()).expect("start writer");
    output.submit(record(b"ok\n"));
    assert_eq!(
        guard
            .shutdown_async(tokio::time::Instant::now() + TEST_TIMEOUT)
            .await,
        LoggerShutdown::Completed
    );
}
