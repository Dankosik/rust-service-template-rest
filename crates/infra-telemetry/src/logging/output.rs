//! Whole-record admission and synchronous output lifetime, independent of Tokio.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

const CAPACITY: usize = 1024;

/// A bounded description of why output stopped; never contains record contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoggingFailure {
    Write(io::ErrorKind),
    Flush(io::ErrorKind),
    Panicked,
    Terminated,
}

/// Actual writer lifetime, readable without using stdout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoggingWriterState {
    Running,
    Flushed,
    Failed(LoggingFailure),
}

/// Cumulative loss and sink failure counts. A timed-out live writer retains custody
/// of its queue; those records are not counted as lost merely because waiting ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoggingSnapshot {
    pub dropped_full: u64,
    pub dropped_stopped: u64,
    pub write_errors: u64,
    pub flush_errors: u64,
    pub state: LoggingWriterState,
}

#[derive(Debug)]
struct Shared {
    admission: Mutex<Option<mpsc::SyncSender<Vec<u8>>>>,
    full: AtomicU64,
    stopped: AtomicU64,
    write_errors: AtomicU64,
    flush_errors: AtomicU64,
    state: Mutex<LoggingWriterState>,
}

fn increment(counter: &AtomicU64, count: u64) {
    let _ = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(count))
    });
}

impl Shared {
    fn close(&self) {
        let sender = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(sender);
    }

    fn set_state(&self, state: LoggingWriterState) {
        let mut current = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(*current, LoggingWriterState::Failed(_)) {
            *current = state;
        }
    }
}

/// Cloneable, sink-independent reader, including losses before metrics installation.
#[derive(Clone, Debug)]
pub struct LoggingStatus(Arc<Shared>);

impl LoggingStatus {
    #[must_use]
    pub fn snapshot(&self) -> LoggingSnapshot {
        let state = *self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        LoggingSnapshot {
            dropped_full: self.0.full.load(Ordering::Relaxed),
            dropped_stopped: self.0.stopped.load(Ordering::Relaxed),
            write_errors: self.0.write_errors.load(Ordering::Relaxed),
            flush_errors: self.0.flush_errors.load(Ordering::Relaxed),
            state,
        }
    }
}

/// Result of the owner's bounded wait. Flushed means OS-writer acceptance,
/// not durable storage or collector delivery. Prior overload drops do not change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoggingShutdown {
    Flushed,
    Failed(LoggingFailure),
    TimedOut,
}

impl LoggingShutdown {
    #[must_use]
    pub const fn is_flushed(self) -> bool {
        matches!(self, Self::Flushed)
    }
}

/// Own this outside `Runtime::block_on` through the last intended log record.
/// Dropping it closes admission and detaches without waiting for sink I/O.
#[must_use = "retain logging custody through the final intended record"]
#[derive(Debug)]
pub struct LoggingGuard {
    shared: Arc<Shared>,
    completion: mpsc::Receiver<LoggingShutdown>,
    result: Option<LoggingShutdown>,
    _thread: JoinHandle<()>,
}

impl LoggingGuard {
    #[must_use]
    pub fn status(&self) -> LoggingStatus {
        LoggingStatus(Arc::clone(&self.shared))
    }

    /// Close admission and wait only until `deadline`, on the outer synchronous
    /// entry thread. Never call this on a Tokio worker. Timeout cannot cancel an
    /// OS write, and Drop does not wait again or join thread-local teardown.
    pub fn shutdown(&mut self, deadline: Instant) -> LoggingShutdown {
        self.shared.close();
        if let Some(result) = self.result {
            return result;
        }
        let result = match self.completion.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Disconnected) => {
                LoggingShutdown::Failed(LoggingFailure::Terminated)
            }
            Err(mpsc::TryRecvError::Empty) => {
                match self
                    .completion
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(result) => result,
                    Err(mpsc::RecvTimeoutError::Timeout) => return LoggingShutdown::TimedOut,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        LoggingShutdown::Failed(LoggingFailure::Terminated)
                    }
                }
            }
        };
        self.result = Some(result);
        result
    }
}

impl Drop for LoggingGuard {
    fn drop(&mut self) {
        self.shared.close();
    }
}

/// Private `MakeWriter` seam: the formatting layers supply one complete event
/// per `write_all`, so this is not a general streaming `Write` adapter.
#[derive(Clone)]
pub(super) struct RecordWriter(Arc<Shared>);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RecordWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Write for RecordWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let record = bytes.to_vec();
        let rejection = match self.0.admission.lock() {
            Ok(gate) => match gate.as_ref() {
                Some(sender) => sender.try_send(record).err(),
                None => Some(mpsc::TrySendError::Disconnected(record)),
            },
            Err(poisoned) => {
                let sender = poisoned.into_inner().take();
                drop(sender);
                self.0
                    .set_state(LoggingWriterState::Failed(LoggingFailure::Terminated));
                Some(mpsc::TrySendError::Disconnected(record))
            }
        };
        // Rejected allocations are destroyed only after the admission lock is released.
        match rejection {
            Some(mpsc::TrySendError::Full(_)) => increment(&self.0.full, 1),
            Some(mpsc::TrySendError::Disconnected(_)) => increment(&self.0.stopped, 1),
            None => {}
        }
        // Sink rejection never activates a formatting layer's synchronous fallback.
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn stdout() -> io::Result<(RecordWriter, LoggingGuard)> {
    start(io::stdout())
}

fn start<W: Write + Send + 'static>(sink: W) -> io::Result<(RecordWriter, LoggingGuard)> {
    let (sender, receiver) = mpsc::sync_channel(CAPACITY);
    let (completion, result) = mpsc::channel();
    let shared = Arc::new(Shared {
        admission: Mutex::new(Some(sender)),
        full: AtomicU64::new(0),
        stopped: AtomicU64::new(0),
        write_errors: AtomicU64::new(0),
        flush_errors: AtomicU64::new(0),
        state: Mutex::new(LoggingWriterState::Running),
    });
    let worker = Worker {
        sink: Some(sink),
        receiver,
        shared: Arc::clone(&shared),
        completion,
        unconfirmed: false,
        completed: false,
    };
    let thread = thread::Builder::new()
        .name("service-log-output".to_owned())
        .spawn(move || worker.run())?;
    Ok((
        RecordWriter(Arc::clone(&shared)),
        LoggingGuard {
            shared,
            completion: result,
            result: None,
            _thread: thread,
        },
    ))
}

struct Worker<W> {
    sink: Option<W>,
    receiver: mpsc::Receiver<Vec<u8>>,
    shared: Arc<Shared>,
    completion: mpsc::Sender<LoggingShutdown>,
    unconfirmed: bool,
    completed: bool,
}

impl<W: Write> Worker<W> {
    fn run(mut self) {
        let outcome = self.drain();
        if let Err(failure) = outcome {
            self.shared.set_state(LoggingWriterState::Failed(failure));
            self.discard();
        }
        // A receipt is published only after sink teardown too.
        drop(self.sink.take());
        if outcome.is_ok() {
            self.shared.set_state(LoggingWriterState::Flushed);
        }
        let result = match *self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            LoggingWriterState::Flushed => LoggingShutdown::Flushed,
            LoggingWriterState::Failed(failure) => LoggingShutdown::Failed(failure),
            LoggingWriterState::Running => LoggingShutdown::Failed(LoggingFailure::Terminated),
        };
        self.completed = true;
        let _ = self.completion.send(result);
    }

    fn drain(&mut self) -> Result<(), LoggingFailure> {
        while let Ok(record) = self.receiver.recv() {
            self.unconfirmed = true;
            if let Err(error) = self
                .sink
                .as_mut()
                .ok_or(LoggingFailure::Terminated)?
                .write_all(&record)
            {
                increment(&self.shared.write_errors, 1);
                return Err(LoggingFailure::Write(error.kind()));
            }
            self.flush()?;
            self.unconfirmed = false;
        }
        self.flush()
    }

    fn flush(&mut self) -> Result<(), LoggingFailure> {
        self.sink
            .as_mut()
            .ok_or(LoggingFailure::Terminated)?
            .flush()
            .map_err(|error| {
                increment(&self.shared.flush_errors, 1);
                LoggingFailure::Flush(error.kind())
            })
    }
}

impl<W> Worker<W> {
    fn discard(&mut self) {
        self.shared.close();
        let mut lost = u64::from(std::mem::take(&mut self.unconfirmed));
        while self.receiver.try_recv().is_ok() {
            lost = lost.saturating_add(1);
        }
        increment(&self.shared.stopped, lost);
    }
}

impl<W> Drop for Worker<W> {
    fn drop(&mut self) {
        if !self.completed {
            let failure = if thread::panicking() {
                LoggingFailure::Panicked
            } else {
                LoggingFailure::Terminated
            };
            self.shared.set_state(LoggingWriterState::Failed(failure));
            self.discard();
            // Drop sink before notifying, even during unwinding.
            drop(self.sink.take());
            let _ = self.completion.send(LoggingShutdown::Failed(failure));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "controlled sink fixtures fail with precise setup context"
    )]
    use super::*;
    use std::time::Duration;
    use tracing_subscriber::layer::SubscriberExt;

    const WAIT: Duration = Duration::from_secs(3);

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    enum Failure {
        None,
        Write,
        Flush,
        Panic,
    }

    struct Controlled {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        buffer: Buffer,
        first: bool,
        failure: Failure,
    }

    impl Write for Controlled {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.first {
                self.first = false;
                self.entered.send(()).unwrap();
                self.release
                    .recv_timeout(WAIT)
                    .expect("test must release sink");
            }
            match self.failure {
                Failure::Write => Err(io::ErrorKind::BrokenPipe.into()),
                Failure::Panic => panic!("controlled output panic"),
                Failure::None | Failure::Flush => self.buffer.write(bytes),
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            if matches!(self.failure, Failure::Flush) {
                Err(io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(())
            }
        }
    }

    fn controlled(
        failure: Failure,
    ) -> (
        RecordWriter,
        LoggingGuard,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        Buffer,
    ) {
        let (entered, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let buffer = Buffer::default();
        let (writer, guard) = start(Controlled {
            entered,
            release: released,
            buffer: buffer.clone(),
            first: true,
            failure,
        })
        .unwrap();
        (writer, guard, observed, release, buffer)
    }

    // The actual sink is held outside the admission path: filling its real queue
    // must drop only newest records and leave unrelated current-thread tasks runnable.
    #[tokio::test(flavor = "current_thread")]
    async fn stalled_sink_bounds_admission_and_preserves_admitted_records() {
        let (mut writer, mut guard, entered, release, buffer) = controlled(Failure::None);
        writer.write_all(b"first\n").unwrap();
        // Coordinate using a separate synchronous waiter, never block this runtime on I/O.
        tokio::task::spawn_blocking(move || entered.recv_timeout(WAIT).unwrap())
            .await
            .unwrap();
        let mut expected = b"first\n".to_vec();
        for index in 0..CAPACITY {
            let line = format!("{index}\n");
            writer.write_all(line.as_bytes()).unwrap();
            expected.extend_from_slice(line.as_bytes());
        }
        writer.write_all(b"discard newest\n").unwrap();
        tokio::task::yield_now().await;
        assert_eq!(guard.status().snapshot().dropped_full, 1);
        let status = guard.status();
        // shutdown belongs to the outer synchronous owner, not this runtime worker.
        let (mut guard, result) = tokio::task::spawn_blocking(move || {
            let result = guard.shutdown(Instant::now());
            (guard, result)
        })
        .await
        .unwrap();
        assert_eq!(result, LoggingShutdown::TimedOut);
        writer.write_all(b"closed\n").unwrap();
        assert_eq!(status.snapshot().dropped_stopped, 1);
        release.send(()).unwrap();
        let result = tokio::task::spawn_blocking(move || guard.shutdown(Instant::now() + WAIT))
            .await
            .unwrap();
        assert_eq!(result, LoggingShutdown::Flushed);
        assert_eq!(*buffer.0.lock().unwrap(), expected);
        assert_eq!(status.snapshot().state, LoggingWriterState::Flushed);
    }

    #[test]
    fn stopped_writer_counts_current_and_admitted_backlog_exactly_once() {
        for (failure, expected, writes, flushes) in [
            (
                Failure::Write,
                LoggingFailure::Write(io::ErrorKind::BrokenPipe),
                1,
                0,
            ),
            (
                Failure::Flush,
                LoggingFailure::Flush(io::ErrorKind::BrokenPipe),
                0,
                1,
            ),
            (Failure::Panic, LoggingFailure::Panicked, 0, 0),
        ] {
            let (mut writer, mut guard, entered, release, _) = controlled(failure);
            writer.write_all(b"unconfirmed\n").unwrap();
            entered.recv_timeout(WAIT).unwrap();
            for _ in 0..7 {
                writer.write_all(b"admitted\n").unwrap();
            }
            release.send(()).unwrap();
            assert_eq!(
                guard.shutdown(Instant::now() + WAIT),
                LoggingShutdown::Failed(expected)
            );
            let snapshot = guard.status().snapshot();
            assert_eq!(snapshot.dropped_stopped, 8);
            assert_eq!(
                (snapshot.write_errors, snapshot.flush_errors),
                (writes, flushes)
            );
            writer.write_all(b"after failure\n").unwrap();
            assert_eq!(
                guard.shutdown(Instant::now()),
                LoggingShutdown::Failed(expected)
            );
            assert_eq!(guard.status().snapshot().dropped_stopped, 9);
        }
    }

    #[test]
    fn failure_close_accounts_for_racing_producers() {
        let (mut writer, mut guard, entered, release, _) = controlled(Failure::Write);
        writer.write_all(b"unconfirmed\n").unwrap();
        entered.recv_timeout(WAIT).unwrap();
        let ready = Arc::new(std::sync::Barrier::new(5));
        let producers: Vec<_> = (0..4)
            .map(|_| {
                let ready = Arc::clone(&ready);
                let mut writer = writer.clone();
                thread::spawn(move || {
                    ready.wait();
                    for _ in 0..200 {
                        writer.write_all(b"racing\n").unwrap();
                    }
                })
            })
            .collect();
        ready.wait();
        release.send(()).unwrap();
        assert_eq!(
            guard.shutdown(Instant::now() + WAIT),
            LoggingShutdown::Failed(LoggingFailure::Write(io::ErrorKind::BrokenPipe))
        );
        for producer in producers {
            producer.join().unwrap();
        }
        let snapshot = guard.status().snapshot();
        assert_eq!(snapshot.dropped_full, 0);
        assert_eq!(snapshot.dropped_stopped, 801);
    }

    #[test]
    fn final_flush_failure_does_not_claim_confirmed_records_were_lost() {
        struct FinalFlush {
            flushes: usize,
        }
        impl Write for FinalFlush {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flushes += 1;
                if self.flushes == 2 {
                    Err(io::ErrorKind::Other.into())
                } else {
                    Ok(())
                }
            }
        }
        let (mut writer, mut guard) = start(FinalFlush { flushes: 0 }).unwrap();
        writer.write_all(b"confirmed\n").unwrap();
        assert_eq!(
            guard.shutdown(Instant::now() + WAIT),
            LoggingShutdown::Failed(LoggingFailure::Flush(io::ErrorKind::Other))
        );
        assert_eq!(guard.status().snapshot().dropped_stopped, 0);
    }

    #[test]
    fn drop_closes_without_waiting_for_sink_and_completion_follows_sink_teardown() {
        struct Teardown(mpsc::Sender<()>);
        impl Write for Teardown {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl Drop for Teardown {
            fn drop(&mut self) {
                self.0.send(()).unwrap();
            }
        }

        let (mut writer, mut guard, entered, release, _) = controlled(Failure::None);
        writer.write_all(b"held\n").unwrap();
        entered.recv_timeout(WAIT).unwrap();
        // Retain the receipt so this test observes actual completion after Drop.
        let completion = std::mem::replace(&mut guard.completion, mpsc::channel().1);
        let status = guard.status();
        let (dropped, observed) = mpsc::channel();
        let owner = thread::spawn(move || {
            drop(guard);
            dropped.send(()).unwrap();
        });
        let dropped_before_release = observed.recv_timeout(WAIT);
        release.send(()).unwrap();
        owner.join().unwrap();
        assert!(dropped_before_release.is_ok(), "guard Drop waited for sink");
        assert_eq!(
            completion.recv_timeout(WAIT).unwrap(),
            LoggingShutdown::Flushed
        );
        writer.write_all(b"closed\n").unwrap();
        assert_eq!(status.snapshot().dropped_stopped, 1);

        let (sent, received) = mpsc::channel();
        let (_, mut guard) = start(Teardown(sent)).unwrap();
        assert_eq!(
            guard.shutdown(Instant::now() + WAIT),
            LoggingShutdown::Flushed
        );
        received.try_recv().expect("receipt follows sink teardown");
    }

    #[test]
    fn json_and_text_layers_deliver_whole_records_from_concurrent_producers() {
        for json in [true, false] {
            let buffer = Buffer::default();
            let (writer, mut guard) = start(buffer.clone()).unwrap();
            let layer: Box<
                dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync,
            > = if json {
                Box::new(super::super::json::JsonLayer::new(writer))
            } else {
                Box::new(
                    tracing_subscriber::fmt::layer()
                        .with_target(false)
                        .with_ansi(false)
                        .without_time()
                        .with_writer(writer),
                )
            };
            let dispatch =
                tracing::Dispatch::new(tracing_subscriber::Registry::default().with(layer));
            let producers: Vec<_> = (0..4)
                .map(|producer| {
                    let dispatch = dispatch.clone();
                    thread::spawn(move || {
                        let _default = tracing::dispatcher::set_default(&dispatch);
                        for sequence in 0..20 {
                            tracing::info!(producer, sequence, "complete_record");
                        }
                    })
                })
                .collect();
            for producer in producers {
                producer.join().unwrap();
            }
            assert_eq!(
                guard.shutdown(Instant::now() + WAIT),
                LoggingShutdown::Flushed
            );
            let records = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
            assert_eq!(records.lines().count(), 80);
            let mut next = [0_u64; 4];
            for record in records.lines() {
                if json {
                    let value: serde_json::Value = serde_json::from_str(record).unwrap();
                    let producer = usize::try_from(value["producer"].as_u64().unwrap()).unwrap();
                    assert_eq!(value["sequence"], next[producer]);
                    assert_eq!(value["message"], "complete_record");
                    next[producer] += 1;
                } else {
                    assert!(
                        record.contains("complete_record")
                            && record.contains("producer=")
                            && record.contains("sequence="),
                        "{record}"
                    );
                }
            }
        }
    }

    #[test]
    fn metrics_scrapes_include_preinstallation_losses_without_double_counting() {
        let (mut writer, mut guard, entered, release, _) = controlled(Failure::Write);
        writer.write_all(b"current\n").unwrap();
        entered.recv_timeout(WAIT).unwrap();
        for _ in 0..CAPACITY {
            writer.write_all(b"pending\n").unwrap();
        }
        writer.write_all(b"full\n").unwrap();
        let metrics = crate::Metrics::install(&[])
            .unwrap()
            .with_logging(guard.status());
        let before = metrics.render();
        assert!(
            before.contains("service_log_records_dropped_total{reason=\"full\"} 1"),
            "{before}"
        );
        assert!(before.contains("service_log_writer_running 1"), "{before}");
        release.send(()).unwrap();
        assert!(matches!(
            guard.shutdown(Instant::now() + WAIT),
            LoggingShutdown::Failed(_)
        ));
        for _ in 0..2 {
            let after = metrics.render();
            assert!(
                after.contains("service_log_records_dropped_total{reason=\"stopped\"} 1025"),
                "{after}"
            );
            assert!(
                after.contains("service_log_sink_errors_total{operation=\"write\"} 1"),
                "{after}"
            );
            assert!(after.contains("service_log_writer_running 0"), "{after}");
        }
    }
}
