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
const DRAINING_BIT: u32 = 1 << 31;
const DROPPED_METRIC: &str = "telemetry_log_records_dropped_total";
const SINK_ERRORS_METRIC: &str = "telemetry_log_sink_errors_total";
static METRIC_METADATA: metrics::Metadata<'static> =
    metrics::Metadata::new(module_path!(), metrics::Level::INFO, Some(module_path!()));
static TOTALS: Counters = Counters::new();

thread_local! {
    static WRITER_CONTEXT: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn on_writer_thread() -> bool {
    WRITER_CONTEXT.try_with(Cell::get).unwrap_or(false)
}

pub(super) type Record = Vec<u8>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Enum)]
pub(super) enum DropReason {
    QueueFull,
    Oversize,
    SpanCapacity,
    Busy,
    Reentrant,
    Closed,
}

impl DropReason {
    fn metric_key(self) -> &'static metrics::Key {
        match self {
            Self::QueueFull => {
                crate::metrics::static_metric_key!(DROPPED_METRIC, ("reason", "queue_full"))
            }
            Self::Oversize => {
                crate::metrics::static_metric_key!(DROPPED_METRIC, ("reason", "oversize"))
            }
            Self::SpanCapacity => {
                crate::metrics::static_metric_key!(DROPPED_METRIC, ("reason", "span_capacity"))
            }
            Self::Busy => crate::metrics::static_metric_key!(DROPPED_METRIC, ("reason", "busy")),
            Self::Reentrant => {
                crate::metrics::static_metric_key!(DROPPED_METRIC, ("reason", "reentrant"))
            }
            Self::Closed => {
                crate::metrics::static_metric_key!(DROPPED_METRIC, ("reason", "closed"))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Enum)]
enum SinkOperation {
    Write,
    Flush,
    Worker,
}

impl SinkOperation {
    fn metric_key(self) -> &'static metrics::Key {
        match self {
            Self::Write => {
                crate::metrics::static_metric_key!(SINK_ERRORS_METRIC, ("operation", "write"))
            }
            Self::Flush => {
                crate::metrics::static_metric_key!(SINK_ERRORS_METRIC, ("operation", "flush"))
            }
            Self::Worker => {
                crate::metrics::static_metric_key!(SINK_ERRORS_METRIC, ("operation", "worker"))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct LogSnapshot {
    pub(super) dropped: EnumMap<DropReason, u64>,
    sink_errors: EnumMap<SinkOperation, u64>,
}

#[cfg(test)]
impl LogSnapshot {
    pub(super) fn dropped(&self, reason: DropReason) -> u64 {
        self.dropped[reason]
    }

    fn sink_errors(&self, operation: SinkOperation) -> u64 {
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

const _: () = assert!(LoggerShutdownReasons::all().bits() & DRAINING_BIT == 0);

#[must_use]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoggerShutdown {
    Completed,
    Incomplete(LoggerShutdownReasons),
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
    shutdown_gate: RwLock<Option<crossbeam_channel::Sender<()>>>,
    drain_state: AtomicU32,
    #[cfg(test)]
    counters: Counters,
}

impl Shared {
    fn close(&self) {
        let _ = self.shutdown_gate.write().take();
    }

    fn latch_drain_failure(&self, reason: LoggerShutdownReasons) {
        let _ = self
            .drain_state
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |state| {
                (state & DRAINING_BIT != 0).then_some(state | reason.bits())
            });
    }

    fn record_sink_error(&self, operation: SinkOperation) {
        #[cfg(test)]
        self.counters.sink_errors[operation].fetch_add(1, Ordering::Relaxed);
        TOTALS.sink_errors[operation].fetch_add(1, Ordering::Relaxed);
        match operation {
            SinkOperation::Write => self.latch_drain_failure(LoggerShutdownReasons::WRITE),
            SinkOperation::Flush => self.latch_drain_failure(LoggerShutdownReasons::FLUSH),
            SinkOperation::Worker => {
                self.drain_state
                    .fetch_or(LoggerShutdownReasons::WORKER.bits(), Ordering::Relaxed);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Output {
    sender: crossbeam_channel::Sender<Record>,
    shared: Arc<Shared>,
    worker_thread: thread::Thread,
}

impl Output {
    pub(super) fn submit(&self, record: Record) {
        let gate = self.shared.shutdown_gate.read();
        match gate.is_some().then(|| self.sender.try_send(record)) {
            Some(Ok(())) => {
                drop(gate);
                if self.sender.len() >= DEEP_QUEUE_RECORDS {
                    self.worker_thread.unpark();
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
        #[cfg(test)]
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
    completion_rx: crossbeam_channel::Receiver<()>,
    worker: Option<JoinHandle<()>>,
}

impl LoggerGuard {
    pub fn begin_shutdown(&self) {
        self.shared
            .drain_state
            .fetch_or(DRAINING_BIT, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn snapshot(&self) -> LogSnapshot {
        self.shared.counters.snapshot()
    }

    pub fn shutdown(mut self, deadline: Instant) -> LoggerShutdown {
        self.begin_shutdown();
        self.shared.close();
        let mut reasons = self.wait_for_worker(deadline);
        if Instant::now() >= deadline {
            reasons |= LoggerShutdownReasons::DEADLINE;
        }
        reasons |= LoggerShutdownReasons::from_bits_truncate(
            self.shared.drain_state.load(Ordering::Relaxed),
        );
        if reasons.is_empty() {
            LoggerShutdown::Completed
        } else {
            LoggerShutdown::Incomplete(reasons)
        }
    }

    pub async fn shutdown_async(self, deadline: tokio::time::Instant) -> LoggerShutdown {
        let close = tokio::task::spawn_blocking(move || self.shutdown(deadline.into_std()));
        match tokio::time::timeout_at(deadline, close).await {
            Ok(Ok(shutdown)) => shutdown,
            Ok(Err(_)) => LoggerShutdown::Incomplete(LoggerShutdownReasons::WORKER),
            Err(_) => LoggerShutdown::Incomplete(LoggerShutdownReasons::DEADLINE),
        }
    }

    #[cfg(test)]
    pub(super) fn shutdown_with_snapshot(self, deadline: Instant) -> (LoggerShutdown, LogSnapshot) {
        let shared = Arc::clone(&self.shared);
        let outcome = self.shutdown(deadline);
        (outcome, shared.counters.snapshot())
    }

    fn wait_for_worker(&mut self, deadline: Instant) -> LoggerShutdownReasons {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return LoggerShutdownReasons::DEADLINE;
        }
        let reasons = match self.completion_rx.recv_timeout(remaining) {
            Ok(()) => LoggerShutdownReasons::empty(),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                return LoggerShutdownReasons::DEADLINE;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => LoggerShutdownReasons::WORKER,
        };
        let Some(worker) = self.worker.take() else {
            return reasons;
        };
        while !worker.is_finished() {
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
        if worker.join().is_err() {
            self.shared.record_sink_error(SinkOperation::Worker);
            return reasons | LoggerShutdownReasons::WORKER;
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
        shutdown_gate: RwLock::new(Some(shutdown_tx)),
        drain_state: AtomicU32::new(0),
        #[cfg(test)]
        counters: Counters::new(),
    });
    let (records_tx, records_rx) = crossbeam_channel::bounded(QUEUE_RECORDS);
    let (completion_tx, completion_rx) = crossbeam_channel::bounded(1);
    let worker_shared = Arc::clone(&shared);
    let worker = thread::Builder::new()
        .name("telemetry-writer".into())
        .spawn(move || {
            WRITER_CONTEXT.set(true);
            let shared: &Shared = &worker_shared;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                write_records(writer, records_rx, &shutdown_rx, shared);
            }));
            if result.is_err() {
                worker_shared.record_sink_error(SinkOperation::Worker);
            }
            drop(result);
            let _ = completion_tx.try_send(());
        })?;
    Ok((
        Output {
            sender: records_tx,
            shared: Arc::clone(&shared),
            worker_thread: worker.thread().clone(),
        },
        LoggerGuard {
            shared,
            completion_rx,
            worker: Some(worker),
        },
    ))
}

enum NextRecord {
    Record(Record),
    Flush,
    Closed,
}

fn wait_for_record(
    records: &crossbeam_channel::Receiver<Record>,
    shutdown: &crossbeam_channel::Receiver<()>,
    flush_after: Option<Duration>,
) -> NextRecord {
    if let Ok(record) = records.try_recv() {
        return NextRecord::Record(record);
    }
    let drain_after_shutdown = || {
        records
            .try_recv()
            .map_or(NextRecord::Closed, NextRecord::Record)
    };
    if let Some(timeout) = flush_after {
        crossbeam_channel::select_biased! {
            recv(records) -> received => received.map_or(NextRecord::Closed, NextRecord::Record),
            recv(shutdown) -> _ => drain_after_shutdown(),
            default(timeout) => NextRecord::Flush,
        }
    } else {
        crossbeam_channel::select_biased! {
            recv(records) -> received => received.map_or(NextRecord::Closed, NextRecord::Record),
            recv(shutdown) -> _ => drain_after_shutdown(),
        }
    }
}

fn flush_sink(writer: &mut impl Write, shared: &Shared) {
    if writer.flush().is_err() {
        shared.record_sink_error(SinkOperation::Flush);
    }
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
        let first = if let Some(record) = carried.take() {
            record
        } else {
            let record = match wait_for_record(&records, shutdown, dirty.then_some(FLUSH_INTERVAL))
            {
                NextRecord::Record(record) => record,
                NextRecord::Flush => {
                    flush_sink(&mut writer, shared);
                    dirty = false;
                    continue;
                }
                NextRecord::Closed => break,
            };
            if last_write.is_some_and(|at| at.elapsed() < BATCH_WINDOW) {
                gather(&records, BATCH_WINDOW);
            }
            record
        };
        batch.clear();
        batch.push(&first);
        while let Ok(record) = records.try_recv() {
            if !batch.fits(record.len()) {
                carried = Some(record);
                break;
            }
            batch.push(&record);
        }
        batch.write(&mut writer, shared);
        last_write = Some(Instant::now());
        dirty = true;
    }
    flush_sink(&mut writer, shared);
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
        let remaining = self.ends[failed..]
            .array_windows()
            .map(|&[start, end]| &self.bytes[start..end]);
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
        let key = reason.metric_key();
        metrics::with_recorder(|recorder| recorder.register_counter(key, &METRIC_METADATA))
            .absolute(dropped);
    }
    for (operation, &errors) in &snapshot.sink_errors {
        let key = operation.metric_key();
        metrics::with_recorder(|recorder| recorder.register_counter(key, &METRIC_METADATA))
            .absolute(errors);
    }
}

#[cfg(test)]
mod tests;
