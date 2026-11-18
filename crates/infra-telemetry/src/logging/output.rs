use enum_map::{Enum, EnumMap, enum_len};
use parking_lot::RwLock;
use std::cell::Cell;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(super) const MAX_RECORD_BYTES: usize = 16 * 1024;
const QUEUE_RECORDS: usize = 512;
const DEEP_QUEUE_RECORDS: usize = QUEUE_RECORDS / 4;
const MAX_BATCH_BYTES: usize = 64 * 1024;
const BATCH_WINDOW: Duration = Duration::from_millis(2);
const FLUSH_INTERVAL: Duration = Duration::from_millis(10);
const JOIN_POLL: Duration = Duration::from_millis(1);
const DRAINING: u32 = 1 << 31;
const DROPPED_METRIC: &str = "telemetry_log_records_dropped_total";
const SINK_ERRORS_METRIC: &str = "telemetry_log_sink_errors_total";
static TOTALS: Counters = Counters::new();

thread_local! {
    static WRITER_CONTEXT: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn on_writer_thread() -> bool {
    WRITER_CONTEXT.try_with(Cell::get).unwrap_or(false)
}

pub(super) struct Record {
    pub(super) bytes: Box<[u8]>,
}

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
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct LoggerShutdownReasons: u32 {
        const DEADLINE = 1;
        const WORKER = 1 << 1;
        const WRITE = 1 << 2;
        const FLUSH = 1 << 3;
        const DROPPED = 1 << 4;
    }
}

const _: () = assert!(LoggerShutdownReasons::all().bits() & DRAINING == 0);

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
    closed: RwLock<Option<crossbeam_channel::Sender<()>>>,
    drain: AtomicU32,
    counters: Counters,
}

impl Shared {
    fn close(&self) {
        let _ = self.closed.write().take();
    }

    fn latch_drain_failure(&self, reason: LoggerShutdownReasons) {
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
                self.drain
                    .fetch_or(LoggerShutdownReasons::WORKER.bits(), Ordering::Relaxed);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Output {
    sender: crossbeam_channel::Sender<Record>,
    shared: Arc<Shared>,
    writer: thread::Thread,
}

impl Output {
    pub(super) fn submit(&self, record: Record) {
        let sent = {
            let open = self.shared.closed.read();
            open.is_some().then(|| self.sender.try_send(record))
        };
        match sent {
            Some(Ok(())) => {
                if self.sender.len() >= DEEP_QUEUE_RECORDS {
                    self.writer.unpark();
                }
            }
            Some(Err(crossbeam_channel::TrySendError::Full(_))) => {
                self.record_drop(DropReason::QueueFull);
            }
            None | Some(Err(crossbeam_channel::TrySendError::Disconnected(_))) => {
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

#[derive(Debug)]
#[must_use = "retain the logger guard and explicitly shut it down before process exit"]
pub struct LoggerGuard {
    shared: Arc<Shared>,
    completion: crossbeam_channel::Receiver<()>,
    worker: Option<JoinHandle<()>>,
}

impl LoggerGuard {
    pub fn begin_shutdown(&self) {
        self.shared.drain.fetch_or(DRAINING, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self) -> LogSnapshot {
        self.shared.counters.snapshot()
    }

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

    fn wait_for_worker(&mut self, deadline: Instant) -> LoggerShutdownReasons {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return LoggerShutdownReasons::DEADLINE;
        }
        let mut reasons = LoggerShutdownReasons::empty();
        match self.completion.recv_timeout(remaining) {
            Ok(()) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                return LoggerShutdownReasons::DEADLINE;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                reasons = LoggerShutdownReasons::WORKER;
            }
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
        drop(self.worker.take());
    }
}

pub(super) fn start<W: Write + Send + 'static>(writer: W) -> io::Result<(Output, LoggerGuard)> {
    let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(0);
    let shared = Arc::new(Shared {
        closed: RwLock::new(Some(shutdown_tx)),
        drain: AtomicU32::new(0),
        counters: Counters::new(),
    });
    let (sender, records) = crossbeam_channel::bounded(QUEUE_RECORDS);
    let (complete, completion) = crossbeam_channel::bounded(1);
    let worker_shared = Arc::clone(&shared);
    let worker = thread::Builder::new()
        .name("telemetry-writer".into())
        .spawn(move || {
            WRITER_CONTEXT.set(true);
            let shared: &Shared = &worker_shared;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                write_records(writer, records, &shutdown_rx, shared);
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
            writer: worker.thread().clone(),
        },
        LoggerGuard {
            shared,
            completion,
            worker: Some(worker),
        },
    ))
}

fn write_records<W: Write>(
    mut writer: W,
    records: crossbeam_channel::Receiver<Record>,
    shutdown: &crossbeam_channel::Receiver<()>,
    shared: &Shared,
) {
    let mut dirty = false;
    let mut batch = Batch::default();
    let mut carried: Option<Record> = None;
    let mut last_write: Option<Instant> = None;
    loop {
        let carried_over = carried.is_some();
        let first = if let Some(record) = carried.take() {
            record
        } else if let Ok(record) = records.try_recv() {
            record
        } else if dirty {
            crossbeam_channel::select_biased! {
                recv(records) -> received => match received {
                    Ok(record) => record,
                    Err(_) => break,
                },
                recv(shutdown) -> _ => match records.try_recv() {
                    Ok(record) => record,
                    Err(_) => break,
                },
                default(FLUSH_INTERVAL) => {
                    if writer.flush().is_err() {
                        shared.record_sink_error(SinkOperation::Flush);
                    }
                    dirty = false;
                    continue;
                }
            }
        } else {
            crossbeam_channel::select_biased! {
                recv(records) -> received => match received {
                    Ok(record) => record,
                    Err(_) => break,
                },
                recv(shutdown) -> _ => match records.try_recv() {
                    Ok(record) => record,
                    Err(_) => break,
                },
            }
        };
        if !carried_over && last_write.is_some_and(|at| at.elapsed() < BATCH_WINDOW) {
            gather(&records, BATCH_WINDOW);
        }
        batch.clear();
        batch.push(&first.bytes);
        while let Ok(record) = records.try_recv() {
            if !batch.fits(record.bytes.len()) {
                carried = Some(record);
                break;
            }
            batch.push(&record.bytes);
        }
        batch.write(&mut writer, shared);
        last_write = Some(Instant::now());
        dirty = true;
    }
    if writer.flush().is_err() {
        shared.record_sink_error(SinkOperation::Flush);
    }
    drop(records);
}

fn gather(records: &crossbeam_channel::Receiver<Record>, window: Duration) {
    let until = Instant::now() + window;
    while records.len() < DEEP_QUEUE_RECORDS {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        thread::park_timeout(left);
    }
}

#[derive(Default)]
struct Batch {
    bytes: Vec<u8>,
    ends: Vec<usize>,
    torn: bool,
}

impl Batch {
    fn clear(&mut self) {
        self.bytes.clear();
        self.ends.clear();
    }

    fn fits(&self, record_bytes: usize) -> bool {
        self.bytes.len() + record_bytes <= MAX_BATCH_BYTES
    }

    fn push(&mut self, record: &[u8]) {
        if self.ends.is_empty() && self.torn {
            self.bytes.push(b'\n');
        }
        self.bytes.extend_from_slice(record);
        self.ends.push(self.bytes.len());
    }

    // `self.ends[0]` includes the leading `\n` pushed when `self.torn` was true.
    // For a short write (`accepted < self.bytes.len()`), returns whether the sink
    // was left mid-line and every record after the failed one (record 0 if even the
    // separating `\n` prefix was not written).
    fn after_short_write(&self, accepted: usize) -> (bool, impl Iterator<Item = &[u8]>) {
        let prefix = usize::from(self.torn);
        let (torn, failed) = if accepted < prefix {
            (self.torn, 0)
        } else {
            let failed = self.ends.partition_point(|&end| end <= accepted);
            let start = if failed == 0 {
                prefix
            } else {
                self.ends[failed - 1]
            };
            (accepted > start, failed)
        };
        let mut start = self.ends[failed];
        let remaining = self.ends[failed + 1..].iter().map(move |&end| {
            let line = &self.bytes[start..end];
            start = end;
            line
        });
        (torn, remaining)
    }

    fn write(&mut self, writer: &mut impl Write, shared: &Shared) {
        let accepted = write_bytes(writer, &self.bytes);
        if accepted == self.bytes.len() {
            self.torn = false;
            return;
        }
        shared.record_sink_error(SinkOperation::Write);
        // Drop the failed record and write remaining ones individually to keep lines separated.
        let (mut torn, remaining) = self.after_short_write(accepted);
        for line in remaining {
            if torn && write_line(writer, b"\n") != Written::Whole {
                shared.record_sink_error(SinkOperation::Write);
                continue;
            }
            let written = write_line(writer, line);
            if written != Written::Whole {
                shared.record_sink_error(SinkOperation::Write);
            }
            torn = written == Written::Part;
        }
        self.torn = torn;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Written {
    Whole,
    Part,
    Nothing,
}

fn write_line(writer: &mut impl Write, line: &[u8]) -> Written {
    let accepted = write_bytes(writer, line);
    if accepted == line.len() {
        Written::Whole
    } else if accepted > 0 {
        Written::Part
    } else {
        Written::Nothing
    }
}

fn write_bytes(writer: &mut impl Write, bytes: &[u8]) -> usize {
    let mut rest = bytes;
    while !rest.is_empty() {
        match writer.write(rest) {
            Ok(0) => break,
            Ok(accepted) => rest = &rest[accepted..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    bytes.len() - rest.len()
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
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::{self, Receiver, SyncSender};

    const TEST_TIMEOUT: Duration = Duration::from_secs(3);

    fn record(bytes: &[u8]) -> Record {
        Record {
            bytes: bytes.into(),
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
            output.submit(Record {
                bytes: bytes.into(),
            });
        }
        assert_eq!(guard.snapshot().dropped.into_array(), [0; 6]);
        release.send(()).expect("resume sink");
        let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
        std::assert_matches!(result, LoggerShutdown::Completed(_), "{result:?}");

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
        std::assert_matches!(result, LoggerShutdown::Completed(_), "{result:?}");
        assert_eq!(counted.sizes.try_iter().count(), 0);
    }

    #[test]
    fn a_shallow_queue_is_gathered_for_the_whole_window() {
        let (sender, records) = crossbeam_channel::bounded(QUEUE_RECORDS);
        sender.send(record(b"one\n")).expect("queue a record");
        let window = Duration::from_millis(20);
        let started = Instant::now();

        gather(&records, window);

        assert!(started.elapsed() >= window);
    }

    #[test]
    fn a_queue_that_grows_deep_ends_the_gathering_before_its_window() {
        let (sender, records) = crossbeam_channel::bounded(QUEUE_RECORDS);
        let (open, _closed) = crossbeam_channel::bounded(0);
        let (gathered_tx, gathered) = mpsc::sync_channel(1);
        let waiter = thread::spawn(move || {
            gather(&records, Duration::from_secs(3600));
            gathered_tx.send(records.len()).expect("report the queue");
        });
        let output = Output {
            sender,
            shared: Arc::new(Shared {
                closed: RwLock::new(Some(open)),
                drain: AtomicU32::new(0),
                counters: Counters::new(),
            }),
            writer: waiter.thread().clone(),
        };

        for _ in 0..DEEP_QUEUE_RECORDS {
            output.submit(record(b"line\n"));
        }

        assert_eq!(gathered.recv_timeout(TEST_TIMEOUT), Ok(DEEP_QUEUE_RECORDS));
    }

    struct ScriptedSink {
        accepts: std::collections::VecDeque<Option<usize>>,
        bytes: Vec<u8>,
    }

    impl Write for ScriptedSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let accepted = match self.accepts.pop_front() {
                Some(None) => return Err(io::Error::other("private test sink failure")),
                Some(Some(limit)) => limit.min(bytes.len()),
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
        type Case = (bool, &'static [Option<usize>], &'static [u8]);
        let cases: [Case; 6] = [
            (false, &[None], b"bb\ncc\ndd\n"),
            (false, &[Some(3), None], b"aa\ncc\ndd\n"),
            (false, &[Some(4), None], b"aa\nb\ncc\ndd\n"),
            (false, &[Some(7), None], b"aa\nbb\nc\ndd\n"),
            (true, &[None], b"\nbb\ncc\ndd\n"),
            (true, &[Some(1), None], b"\nbb\ncc\ndd\n"),
        ];
        for (after_torn_line, accepts, expected) in cases {
            let shared = Shared {
                closed: RwLock::new(None),
                drain: AtomicU32::new(0),
                counters: Counters::new(),
            };
            let mut sink = ScriptedSink {
                accepts: accepts.iter().copied().collect(),
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
                "after_torn_line={after_torn_line} accepts={accepts:?}"
            );
            assert_eq!(
                shared.counters.snapshot().sink_errors.into_array(),
                [1, 0, 0],
                "after_torn_line={after_torn_line} accepts={accepts:?}"
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
        std::assert_matches!(result, LoggerShutdown::Completed(_), "{result:?}");
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

        const RESTORE_METADATA: Metadata<'static> =
            Metadata::new(module_path!(), Level::INFO, None);

        let (output, guard) = start(io::sink()).expect("start writer");
        let result = guard.shutdown(Instant::now() + TEST_TIMEOUT);
        std::assert_matches!(result, LoggerShutdown::Completed(_));
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
}
