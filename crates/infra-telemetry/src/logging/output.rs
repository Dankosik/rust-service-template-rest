//! Bounded local output. Only the owned worker touches the sink.

use enum_map::{Enum, EnumMap, enum_len};
use std::cell::Cell;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(super) const MAX_RECORD_BYTES: usize = 16 * 1024;
/// With [`MAX_RECORD_BYTES`] and the capture limits, bounds the memory the
/// log pipeline can hold.
const QUEUE_RECORDS: usize = 512;
/// The subscriber holds a sender until the process exits, so the channel
/// never disconnects on shutdown; the worker polls the closed flag instead.
const RECEIVE_POLL: Duration = Duration::from_millis(10);
const JOIN_POLL: Duration = Duration::from_millis(1);
/// Above every [`LoggerShutdownReasons`] bit, so the drain state holds both.
const DRAINING: u32 = 1 << 31;
const DROPPED_METRIC: &str = "telemetry_log_records_dropped_total";
const SINK_ERRORS_METRIC: &str = "telemetry_log_sink_errors_total";
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

/// Why the logger rejected a record; a label of `telemetry_log_records_dropped_total`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Enum)]
pub enum DropReason {
    QueueFull,
    Oversize,
    SpanCapacity,
    Busy,
    Reentrant,
    Closed,
}

impl DropReason {
    const fn label(self) -> &'static str {
        match self {
            Self::QueueFull => "queue_full",
            Self::Oversize => "oversize",
            Self::SpanCapacity => "span_capacity",
            Self::Busy => "busy",
            Self::Reentrant => "reentrant",
            Self::Closed => "closed",
        }
    }
}

/// The writer operation that failed; a label of `telemetry_log_sink_errors_total`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Enum)]
pub enum SinkOperation {
    Write,
    Flush,
    Worker,
}

impl SinkOperation {
    const fn label(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Flush => "flush",
            Self::Worker => "worker",
        }
    }
}

/// Local observations, independent of recorder installation or scrape timing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LogSnapshot {
    pub(super) dropped: EnumMap<DropReason, u64>,
    sink_errors: EnumMap<SinkOperation, u64>,
}

impl LogSnapshot {
    #[must_use]
    pub fn dropped(&self, reason: DropReason) -> u64 {
        self.dropped[reason]
    }

    #[must_use]
    pub fn sink_errors(&self, operation: SinkOperation) -> u64 {
        self.sink_errors[operation]
    }
}

bitflags::bitflags! {
    /// Finite reasons why local completion could not be established. The
    /// composition root receives them from [`LoggerGuard::shutdown`] in the
    /// `logger` shutdown stage and uses them for its degraded outcome; because
    /// that stage is last, they are not written as a record.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct LoggerShutdownReasons: u32 {
        /// The deadline passed before the writer exited and was joined.
        const DEADLINE = 1;
        /// The writer panicked or its completion channel disconnected; recorded
        /// whenever it happens, since a dead writer never drains.
        const WORKER = 1 << 1;
        /// A sink write failed after [`LoggerGuard::begin_shutdown`].
        const WRITE = 1 << 2;
        /// A sink flush failed after [`LoggerGuard::begin_shutdown`].
        const FLUSH = 1 << 3;
        /// A record was dropped after [`LoggerGuard::begin_shutdown`].
        const DROPPED = 1 << 4;
    }
}

const _: () = assert!(LoggerShutdownReasons::all().bits() & DRAINING == 0);

/// A local writer result; completion makes no remote-delivery claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoggerShutdown {
    Completed(LogSnapshot),
    Incomplete {
        reasons: LoggerShutdownReasons,
        snapshot: LogSnapshot,
    },
}

#[derive(Debug)]
struct Counters {
    dropped: EnumMap<DropReason, AtomicU64>,
    sink_errors: EnumMap<SinkOperation, AtomicU64>,
}

impl Counters {
    const fn new() -> Self {
        Self {
            dropped: EnumMap::from_array([const { AtomicU64::new(0) }; enum_len::<DropReason>()]),
            sink_errors: EnumMap::from_array(
                [const { AtomicU64::new(0) }; enum_len::<SinkOperation>()],
            ),
        }
    }

    fn snapshot(&self) -> LogSnapshot {
        LogSnapshot {
            dropped: EnumMap::from_fn(|reason| self.dropped[reason].load(Ordering::Relaxed)),
            sink_errors: EnumMap::from_fn(|operation| {
                self.sink_errors[operation].load(Ordering::Relaxed)
            }),
        }
    }
}

#[derive(Debug)]
struct Shared {
    // No formatting, sink calls, or waits occur under this admission lock.
    // `submit` holds it across its check and send, so once `close` returns
    // no record is admitted.
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

    /// Latches `reason` only once [`LoggerGuard::begin_shutdown`] has marked
    /// the drain; earlier failures do not make a later drain incomplete.
    fn latch_drain_failure(&self, reason: LoggerShutdownReasons) {
        // The marker and each completed observation share one atomic ordering.
        // A failure already in flight is included if it finishes after begin.
        let _ = self
            .drain
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |state| {
                (state & DRAINING != 0).then_some(state | reason.bits())
            });
    }

    fn record_sink_error(&self, operation: SinkOperation) {
        self.counters.sink_errors[operation].fetch_add(1, Ordering::Relaxed);
        TOTALS.sink_errors[operation].fetch_add(1, Ordering::Relaxed);
        match operation {
            SinkOperation::Write => self.latch_drain_failure(LoggerShutdownReasons::WRITE),
            SinkOperation::Flush => self.latch_drain_failure(LoggerShutdownReasons::FLUSH),
            SinkOperation::Worker => {
                // A dead worker can never make a later clean drain.
                self.drain
                    .fetch_or(LoggerShutdownReasons::WORKER.bits(), Ordering::Relaxed);
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
        // `None` when admission is closed.
        let sent = {
            let closed = self
                .shared
                .closed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (!*closed).then(|| self.sender.try_send(record))
        };
        match sent {
            Some(Ok(())) => {}
            Some(Err(TrySendError::Full(_))) => self.record_drop(DropReason::QueueFull),
            None | Some(Err(TrySendError::Disconnected(_))) => {
                self.record_drop(DropReason::Closed);
            }
        }
    }

    pub(super) fn record_drop(&self, reason: DropReason) {
        self.shared.counters.dropped[reason].fetch_add(1, Ordering::Relaxed);
        TOTALS.dropped[reason].fetch_add(1, Ordering::Relaxed);
        self.shared
            .latch_drain_failure(LoggerShutdownReasons::DROPPED);
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
        let mut reasons = self.wait_for_worker(deadline);
        if Instant::now() >= deadline {
            reasons |= LoggerShutdownReasons::DEADLINE;
        }
        reasons |=
            LoggerShutdownReasons::from_bits_truncate(self.shared.drain.load(Ordering::Relaxed));
        let snapshot = self.snapshot();
        if reasons.is_empty() {
            LoggerShutdown::Completed(snapshot)
        } else {
            LoggerShutdown::Incomplete { reasons, snapshot }
        }
    }

    /// Waits within `deadline` for the writer's acknowledgement, then for it
    /// to exit, and joins it.
    fn wait_for_worker(&mut self, deadline: Instant) -> LoggerShutdownReasons {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return LoggerShutdownReasons::DEADLINE;
        }
        let mut reasons = LoggerShutdownReasons::empty();
        match self.completion.recv_timeout(remaining) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => return LoggerShutdownReasons::DEADLINE,
            // The writer may still be exiting; it is joined below.
            Err(RecvTimeoutError::Disconnected) => reasons = LoggerShutdownReasons::WORKER,
        }
        while self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return reasons | LoggerShutdownReasons::DEADLINE;
            }
            #[expect(
                clippy::disallowed_methods,
                reason = "logger shutdown runs after the runtime or on its owned blocking task; this poll retains the absolute join deadline"
            )]
            thread::sleep(remaining.min(JOIN_POLL));
        }
        // Taken only once finished: joining an unfinished writer would block
        // past the deadline, while `Drop` detaches it.
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            self.shared.record_sink_error(SinkOperation::Worker);
            reasons |= LoggerShutdownReasons::WORKER;
        }
        reasons
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
            let shared: &Shared = &worker_shared;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                write_records(writer, records, shared);
            }));
            if result.is_err() {
                worker_shared.record_sink_error(SinkOperation::Worker);
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
    // The sink accepted part of a line before failing; the next line starts
    // on a new one, or it would be glued onto that prefix.
    let mut torn = false;
    loop {
        let record = match records.recv_timeout(RECEIVE_POLL) {
            Ok(record) => record,
            // A sender could have committed between the timed receive and the
            // closed-state observation. Drain any remaining records.
            Err(RecvTimeoutError::Timeout) if shared.is_closed() => match records.try_recv() {
                Ok(record) => record,
                Err(_) => break,
            },
            Err(RecvTimeoutError::Timeout) => {
                if dirty {
                    if writer.flush().is_err() {
                        shared.record_sink_error(SinkOperation::Flush);
                    }
                    dirty = false;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if torn {
            if write_line(&mut writer, b"\n") != Written::Whole {
                shared.record_sink_error(SinkOperation::Write);
                dirty = true;
                continue;
            }
            torn = false;
        }
        let written = write_line(&mut writer, &record.bytes[..record.len]);
        if written != Written::Whole {
            shared.record_sink_error(SinkOperation::Write);
        }
        torn |= written == Written::Part;
        dirty = true;
    }
    if writer.flush().is_err() {
        shared.record_sink_error(SinkOperation::Flush);
    }
    drop(records);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Written {
    Whole,
    Part,
    Nothing,
}

/// [`Write::write_all`], reporting whether the sink accepted part of `line`
/// before it failed.
fn write_line(writer: &mut impl Write, line: &[u8]) -> Written {
    let mut rest = line;
    while !rest.is_empty() {
        match writer.write(rest) {
            Ok(0) => break,
            Ok(accepted) => rest = &rest[accepted..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    if rest.is_empty() {
        Written::Whole
    } else if rest.len() < line.len() {
        Written::Part
    } else {
        Written::Nothing
    }
}

pub(super) fn describe_metrics() {
    metrics::describe_counter!(
        DROPPED_METRIC,
        "Local records rejected by the bounded logger, by finite reason."
    );
    metrics::describe_counter!(
        SINK_ERRORS_METRIC,
        "Local writer failures, by finite operation."
    );
}

/// Publish absolute process totals so observations before recorder installation
/// and repeated upkeep calls neither disappear nor count twice.
pub(super) fn publish() {
    let snapshot = TOTALS.snapshot();
    for (reason, &dropped) in &snapshot.dropped {
        metrics::counter!(DROPPED_METRIC, "reason" => reason.label()).absolute(dropped);
    }
    for (operation, &errors) in &snapshot.sink_errors {
        metrics::counter!(SINK_ERRORS_METRIC, "operation" => operation.label()).absolute(errors);
    }
}

#[cfg(test)]
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
        assert_eq!(guard.snapshot().dropped.into_array(), [1, 0, 0, 0, 0, 0]);

        guard.begin_shutdown();
        gate.release.send(()).expect("resume sink");
        let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
        std::assert_matches!(
            result,
            LoggerShutdown::Completed(_),
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
                assert_eq!(reasons, LoggerShutdownReasons::WRITE);
                snapshot
            } else {
                let LoggerShutdown::Completed(snapshot) = result else {
                    panic!("historical write failure cannot poison clean drain: {result:?}");
                };
                snapshot
            };
            assert_eq!(snapshot.sink_errors.into_array(), [1, 0, 0]);
            assert_eq!(
                &*gate.bytes.lock().expect("test output mutex"),
                b"recovered\n"
            );
        }
    }

    /// Accepts three bytes of the first line, then fails once.
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
        // The failure may land after the shutdown marker, which makes it final.
        let (LoggerShutdown::Completed(snapshot) | LoggerShutdown::Incomplete { snapshot, .. }) =
            guard.shutdown(Instant::now() + TEST_TIMEOUT);

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
        assert_eq!(reasons, LoggerShutdownReasons::DROPPED);
        assert_eq!(snapshot.dropped.into_array(), [1, 0, 0, 0, 0, 0]);
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
                assert_eq!(reasons, LoggerShutdownReasons::DEADLINE);
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
            let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
            let LoggerShutdown::Incomplete { reasons, snapshot } = result else {
                panic!("sink failure cannot complete: {result:?}");
            };
            if panics {
                assert_eq!(reasons, LoggerShutdownReasons::WORKER);
                assert_eq!(snapshot.sink_errors.into_array(), [0, 0, 1]);
            } else {
                assert_eq!(reasons, LoggerShutdownReasons::FLUSH);
                assert_eq!(snapshot.sink_errors(SinkOperation::Write), 0);
                assert!(snapshot.sink_errors(SinkOperation::Flush) >= 1);
                assert_eq!(snapshot.sink_errors(SinkOperation::Worker), 0);
            }
            assert!(!format!("{result:?}").contains("private"));
        }
    }

    #[test]
    fn loss_before_recorder_installation_is_published_without_double_counting() {
        let (output, guard) = start(io::sink()).expect("start writer");
        let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
        std::assert_matches!(result, LoggerShutdown::Completed(_));
        output.submit(record(b"closed\n"));
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            for _ in 0..2 {
                let before = TOTALS.snapshot().dropped(DropReason::Closed);
                publish();
                let after = TOTALS.snapshot().dropped(DropReason::Closed);
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
