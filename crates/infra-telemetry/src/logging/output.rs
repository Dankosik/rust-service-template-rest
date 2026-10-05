//! Bounded local output. Only the owned worker touches the sink.

use std::cell::Cell;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(super) const MAX_RECORD_BYTES: usize = 16 * 1024;
const QUEUE_RECORDS: usize = 512;
const RECEIVE_POLL: Duration = Duration::from_millis(10);
const JOIN_POLL: Duration = Duration::from_millis(1);
const DRAINING: u32 = 1 << 31;
const DROP_LABELS: [&str; 6] = [
    "queue_full",
    "oversize",
    "span_capacity",
    "busy",
    "reentrant",
    "closed",
];
const ERROR_LABELS: [&str; 3] = ["write", "flush", "worker"];
static TOTALS: Counters = Counters::new();

thread_local! {
    static WRITER_CONTEXT: Cell<bool> = const { Cell::new(false) };
}

// The shared hook must not recursively submit the writer's own panic.
pub(super) fn on_writer_thread() -> bool {
    WRITER_CONTEXT.try_with(Cell::get).unwrap_or(false)
}

pub(super) struct Record {
    pub(super) bytes: Box<[u8; MAX_RECORD_BYTES]>,
    pub(super) len: usize,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum DropReason {
    QueueFull,
    Oversize,
    SpanCapacity,
    Busy,
    Reentrant,
    Closed,
}

#[derive(Clone, Copy)]
enum SinkError {
    Write,
    Flush,
    Worker,
}

/// Local observations, independent of recorder installation or scrape timing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LogSnapshot {
    /// Counts ordered as queue_full, oversize, span_capacity, busy, reentrant, closed.
    pub dropped: [u64; 6],
    /// Counts ordered as write, flush, worker.
    pub sink_errors: [u64; 3],
}

/// Finite reasons why local completion could not be established.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoggerIncomplete(u32);

impl LoggerIncomplete {
    /// The absolute caller deadline expired.
    pub const DEADLINE: u32 = 1;
    /// The writer panicked or its completion channel disconnected.
    pub const WORKER: u32 = 1 << 1;
    /// A write completed with an error during the final drain.
    pub const WRITE: u32 = 1 << 2;
    /// A flush completed with an error during the final drain.
    pub const FLUSH: u32 = 1 << 3;
    /// A local record was rejected during the final drain.
    pub const DROPPED: u32 = 1 << 4;

    /// Returns the union of the documented reason bits.
    #[must_use]
    pub const fn bits(&self) -> u32 {
        self.0
    }
}

/// A local writer result; completion makes no remote-delivery claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoggerShutdown {
    Completed(LogSnapshot),
    Incomplete {
        reasons: LoggerIncomplete,
        snapshot: LogSnapshot,
    },
}

#[derive(Debug)]
struct Counters {
    dropped: [AtomicU64; 6],
    sink_errors: [AtomicU64; 3],
}

impl Counters {
    const fn new() -> Self {
        Self {
            dropped: [const { AtomicU64::new(0) }; 6],
            sink_errors: [const { AtomicU64::new(0) }; 3],
        }
    }

    fn snapshot(&self) -> LogSnapshot {
        LogSnapshot {
            dropped: std::array::from_fn(|i| self.dropped[i].load(Ordering::Relaxed)),
            sink_errors: std::array::from_fn(|i| self.sink_errors[i].load(Ordering::Relaxed)),
        }
    }
}

#[derive(Debug)]
struct Shared {
    // No formatting, sink calls, or waits occur under this admission lock.
    closed: Mutex<bool>,
    drain: AtomicU32,
    counters: Counters,
}

impl Shared {
    fn close(&self) {
        *self
            .closed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    }

    fn is_closed(&self) -> bool {
        *self
            .closed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn final_failure(&self, reason: u32) {
        // The marker and each completed observation share one atomic ordering.
        // A failure already in flight is included if it finishes after begin.
        let _ = self
            .drain
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |state| {
                (state & DRAINING != 0).then_some(state | reason)
            });
    }

    fn sink_error(&self, kind: SinkError) {
        self.counters.sink_errors[kind as usize].fetch_add(1, Ordering::Relaxed);
        TOTALS.sink_errors[kind as usize].fetch_add(1, Ordering::Relaxed);
        match kind {
            SinkError::Write => self.final_failure(LoggerIncomplete::WRITE),
            SinkError::Flush => self.final_failure(LoggerIncomplete::FLUSH),
            SinkError::Worker => {
                // A dead worker can never make a later clean drain.
                self.drain
                    .fetch_or(LoggerIncomplete::WORKER, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Output {
    sender: SyncSender<Record>,
    shared: Arc<Shared>,
}

impl Output {
    pub(super) fn submit(&self, record: Record) {
        let result = {
            let closed = self
                .shared
                .closed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *closed {
                Err(TrySendError::Disconnected(record))
            } else {
                self.sender.try_send(record)
            }
        };
        match result {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => self.dropped(DropReason::QueueFull),
            Err(TrySendError::Disconnected(_)) => self.dropped(DropReason::Closed),
        }
    }

    pub(super) fn dropped(&self, reason: DropReason) {
        self.shared.counters.dropped[reason as usize].fetch_add(1, Ordering::Relaxed);
        TOTALS.dropped[reason as usize].fetch_add(1, Ordering::Relaxed);
        self.shared.final_failure(LoggerIncomplete::DROPPED);
    }
}

/// Owns local output through explicit deadline-bounded shutdown.
/// Dropping this guard closes admission and detaches without waiting or doing I/O.
#[derive(Debug)]
#[must_use = "retain the logger guard and explicitly shut it down before process exit"]
pub struct LoggerGuard {
    shared: Arc<Shared>,
    completion: Receiver<()>,
    worker: Option<JoinHandle<()>>,
}

impl LoggerGuard {
    /// Marks the start of final-drain observation without closing admission.
    /// Repeated calls retain the original marker and all latched failures.
    pub fn begin_shutdown(&self) {
        self.shared.drain.fetch_or(DRAINING, Ordering::Relaxed);
    }

    /// Returns finite local counters, even when no metrics recorder exists.
    #[must_use]
    pub fn snapshot(&self) -> LogSnapshot {
        self.shared.counters.snapshot()
    }

    /// Closes admission and waits only within the absolute deadline.
    /// Requires no async runtime and performs no sink I/O on the caller thread.
    #[must_use]
    pub fn shutdown(mut self, deadline: Instant) -> LoggerShutdown {
        self.begin_shutdown();
        self.shared.close();
        let mut reasons = 0;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            reasons |= LoggerIncomplete::DEADLINE;
        } else {
            match self.completion.recv_timeout(remaining) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout) => reasons |= LoggerIncomplete::DEADLINE,
                Err(RecvTimeoutError::Disconnected) => reasons |= LoggerIncomplete::WORKER,
            }
        }
        if reasons & LoggerIncomplete::DEADLINE == 0 {
            while self
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
            {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    reasons |= LoggerIncomplete::DEADLINE;
                    break;
                }
                thread::sleep(remaining.min(JOIN_POLL));
            }
            if reasons & LoggerIncomplete::DEADLINE == 0
                && let Some(worker) = self.worker.take()
                && worker.join().is_err()
            {
                self.shared.sink_error(SinkError::Worker);
                reasons |= LoggerIncomplete::WORKER;
            }
        }
        if Instant::now() >= deadline {
            reasons |= LoggerIncomplete::DEADLINE;
        }
        reasons |= self.shared.drain.load(Ordering::Relaxed) & !DRAINING;
        let snapshot = self.snapshot();
        if reasons == 0 {
            LoggerShutdown::Completed(snapshot)
        } else {
            LoggerShutdown::Incomplete {
                reasons: LoggerIncomplete(reasons),
                snapshot,
            }
        }
    }
}

impl Drop for LoggerGuard {
    fn drop(&mut self) {
        self.shared.close();
        // Dropping a JoinHandle detaches; it never invokes a second wait.
        drop(self.worker.take());
    }
}

pub(super) fn start<W: Write + Send + 'static>(writer: W) -> io::Result<(Output, LoggerGuard)> {
    let shared = Arc::new(Shared {
        closed: Mutex::new(false),
        drain: AtomicU32::new(0),
        counters: Counters::new(),
    });
    let (sender, records) = mpsc::sync_channel(QUEUE_RECORDS);
    let (complete, completion) = mpsc::sync_channel(1);
    let worker_shared = Arc::clone(&shared);
    let worker = thread::Builder::new()
        .name("telemetry-writer".into())
        .spawn(move || {
            WRITER_CONTEXT.set(true);
            // The closure owns the sink/receiver so both are destroyed before ack,
            // including during an unwind. Never retain or format the panic payload.
            let observations = Arc::clone(&worker_shared);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                write_records(writer, records, &observations);
            }));
            if result.is_err() {
                worker_shared.sink_error(SinkError::Worker);
            }
            drop(result);
            let _ = complete.try_send(());
        })?;
    Ok((
        Output {
            sender,
            shared: Arc::clone(&shared),
        },
        LoggerGuard {
            shared,
            completion,
            worker: Some(worker),
        },
    ))
}

fn write_records<W: Write>(mut writer: W, records: Receiver<Record>, shared: &Shared) {
    let mut dirty = false;
    loop {
        match records.recv_timeout(RECEIVE_POLL) {
            Ok(record) => {
                if writer.write_all(&record.bytes[..record.len]).is_err() {
                    shared.sink_error(SinkError::Write);
                }
                dirty = true;
            }
            Err(RecvTimeoutError::Timeout) => {
                if shared.is_closed() {
                    // A sender could have committed between the timed receive
                    // and our closed-state observation. Drain that last record.
                    if let Ok(record) = records.try_recv() {
                        if writer.write_all(&record.bytes[..record.len]).is_err() {
                            shared.sink_error(SinkError::Write);
                        }
                        dirty = true;
                        continue;
                    }
                    break;
                }
                if dirty {
                    if writer.flush().is_err() {
                        shared.sink_error(SinkError::Flush);
                    }
                    dirty = false;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    if writer.flush().is_err() {
        shared.sink_error(SinkError::Flush);
    }
}

/// Publish absolute process totals so observations before recorder installation
/// and repeated upkeep calls neither disappear nor count twice.
pub(crate) fn publish() {
    metrics::describe_counter!(
        "telemetry_log_records_dropped_total",
        "Local records rejected by the bounded logger, by finite reason."
    );
    metrics::describe_counter!(
        "telemetry_log_sink_errors_total",
        "Local writer failures, by finite operation."
    );
    let snapshot = TOTALS.snapshot();
    for (reason, count) in DROP_LABELS.into_iter().zip(snapshot.dropped) {
        metrics::counter!("telemetry_log_records_dropped_total", "reason" => reason)
            .absolute(count);
    }
    for (operation, count) in ERROR_LABELS.into_iter().zip(snapshot.sink_errors) {
        metrics::counter!("telemetry_log_sink_errors_total", "operation" => operation)
            .absolute(count);
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "bounded output boundary assertions"
)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    const TEST_TIMEOUT: Duration = Duration::from_secs(3);

    fn record(bytes: &[u8]) -> Record {
        let mut storage = Box::new([0; MAX_RECORD_BYTES]);
        storage[..bytes.len()].copy_from_slice(bytes);
        Record {
            bytes: storage,
            len: bytes.len(),
        }
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

        // One in progress plus the accepted 512 queue slots. Every record owns
        // the same fixed 16 KiB capacity, even when its logical length is small.
        for index in 0..512 {
            output.submit(record(format!("{index}\n").as_bytes()));
        }
        output.submit(record(b"lost\n"));
        assert!(
            !gate.released.load(Ordering::SeqCst),
            "admission did not wait for sink"
        );
        assert_eq!(guard.snapshot().dropped, [1, 0, 0, 0, 0, 0]);

        guard.begin_shutdown();
        gate.release.send(()).expect("resume sink");
        let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
        assert!(
            matches!(result, LoggerShutdown::Completed(_)),
            "historical loss: {result:?}"
        );
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
            let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
            let snapshot = if during_drain {
                let LoggerShutdown::Incomplete { reasons, snapshot } = result else {
                    panic!("in-flight failure cannot be hidden by recovery: {result:?}");
                };
                assert_eq!(reasons.bits(), LoggerIncomplete::WRITE);
                snapshot
            } else {
                let LoggerShutdown::Completed(snapshot) = result else {
                    panic!("historical write failure cannot poison clean drain: {result:?}");
                };
                snapshot
            };
            assert_eq!(snapshot.sink_errors, [1, 0, 0]);
            assert_eq!(
                &*gate.bytes.lock().expect("test output mutex"),
                b"recovered\n"
            );
        }
    }

    #[test]
    fn final_record_loss_survives_successful_sink_cleanup() {
        let (writer, gate) = gated(false);
        let (output, guard) = start(writer).expect("start writer");
        output.submit(record(b"first\n"));
        gate.entered
            .recv_timeout(TEST_TIMEOUT)
            .expect("writer entered sink");
        for _ in 0..512 {
            output.submit(record(b"queued\n"));
        }
        guard.begin_shutdown();
        output.submit(record(b"terminal\n"));
        gate.release.send(()).expect("resume sink");
        let LoggerShutdown::Incomplete { reasons, snapshot } =
            guard.shutdown(Instant::now() + TEST_TIMEOUT)
        else {
            panic!("a lost final record makes the drain incomplete");
        };
        assert_eq!(reasons.bits(), LoggerIncomplete::DROPPED);
        assert_eq!(snapshot.dropped, [1, 0, 0, 0, 0, 0]);
        assert!(gate.destroyed.load(Ordering::SeqCst));
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
            // Retain the real thread solely to join fixture cleanup after the
            // assertion. The deadline branch returns before a join is eligible.
            let worker = guard.worker.take().expect("owned writer thread");
            let began = Instant::now();
            if explicit_shutdown {
                let result = guard.shutdown(began + Duration::from_millis(20));
                let LoggerShutdown::Incomplete { reasons, .. } = result else {
                    panic!("blocked writer cannot complete: {result:?}");
                };
                assert_eq!(reasons.bits(), LoggerIncomplete::DEADLINE);
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
            // The fixture's own recv has a timeout, so even a broken output
            // cleanup cannot leave this join waiting on an unbounded gate.
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
                if self.0 {
                    panic!("private writer panic payload");
                }
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
            let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
            let LoggerShutdown::Incomplete { reasons, snapshot } = result else {
                panic!("sink failure cannot complete: {result:?}");
            };
            if panics {
                assert_eq!(reasons.bits(), LoggerIncomplete::WORKER);
                assert_eq!(snapshot.sink_errors, [0, 0, 1]);
            } else {
                assert_eq!(reasons.bits(), LoggerIncomplete::FLUSH);
                assert_eq!(snapshot.sink_errors[0], 0);
                assert!(snapshot.sink_errors[1] >= 1);
                assert_eq!(snapshot.sink_errors[2], 0);
            }
            assert!(!format!("{result:?}").contains("private"));
        }
    }

    #[test]
    fn loss_before_recorder_installation_is_published_without_double_counting() {
        let (output, guard) = start(io::sink()).expect("start writer");
        let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
        assert!(matches!(result, LoggerShutdown::Completed(_)));
        output.submit(record(b"closed\n"));
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            for _ in 0..2 {
                let before = TOTALS.snapshot().dropped[5];
                publish();
                let after = TOTALS.snapshot().dropped[5];
                let rendered = handle.render();
                let line = rendered
                    .lines()
                    .find(|line| {
                        line.starts_with("telemetry_log_records_dropped_total{reason=\"closed\"}")
                    })
                    .expect("pre-recorder loss has a finite metric");
                let value: u64 = line
                    .split_whitespace()
                    .nth(1)
                    .expect("counter value")
                    .parse()
                    .expect("integer counter");
                // Other independent logger tests may increment process totals.
                // Absolute publication must stay within the observed interval.
                assert!(
                    (before..=after).contains(&value),
                    "{line}; expected {before}..={after}"
                );
            }
        });
    }
}
