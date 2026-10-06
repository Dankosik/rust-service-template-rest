//! Async recipes: bounded work, explicit order, cancellation and operation-owned retry policy.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use backon::{ConstantBuilder, Retryable};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures_util::{StreamExt, TryStreamExt, stream};
use tokio::io::AsyncReadExt;
use tokio::sync::{Semaphore, TryAcquireError, oneshot};
use tokio_util::io::{ReaderStream, StreamReader};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

#[tokio::test]
async fn fanout_is_bounded_without_spawning_one_task_per_item() {
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let active = &active;
    let peak = &peak;
    let work = stream::iter(0..12)
        .map(|id| async move {
            let running = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(running, Ordering::SeqCst);
            tokio::task::yield_now().await;
            active.fetch_sub(1, Ordering::SeqCst);
            Ok::<_, std::convert::Infallible>(id * 2)
        })
        // buffered retains input order; buffer_unordered yields completion order.
        .buffered(3)
        .try_collect::<Vec<_>>();
    let values = tokio::time::timeout(Duration::from_secs(1), work)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(values, (0..12).map(|id| id * 2).collect::<Vec<_>>());
    assert!(peak.load(Ordering::SeqCst) <= 3);
    assert!(peak.load(Ordering::SeqCst) > 1);
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn reader_stream_adapters_preserve_bytes_across_chunk_boundaries() {
    let input = b"row one\nrow two\n";
    let chunks = ReaderStream::with_capacity(&input[..], 3);
    let mut reader = StreamReader::new(chunks);
    let mut output = Vec::new();
    reader.read_to_end(&mut output).await.unwrap();
    assert_eq!(output, input);
}

#[test]
fn byte_buffers_keep_framing_and_slices_explicit() {
    let body = Bytes::from_static(b"abcdef");
    assert_eq!(body.slice(1..4), &b"bcd"[..]);
    let mut encoded = BytesMut::new();
    encoded.put_u16(513);
    encoded.extend_from_slice(b"ok");
    let mut encoded = encoded.freeze();
    assert_eq!(encoded.get_u16(), 513);
    assert_eq!(encoded, &b"ok"[..]);
}

#[tokio::test]
async fn cancelling_a_child_stops_tracked_work_without_cancelling_its_parent() {
    let parent = CancellationToken::new();
    let child = parent.child_token();
    let tracker = TaskTracker::new();
    let worker = child.clone();
    let task = tracker.spawn(async move {
        let result = worker
            .run_until_cancelled(std::future::pending::<()>())
            .await;
        assert!(result.is_none());
    });
    tracker.close();
    child.cancel();
    tokio::time::timeout(Duration::from_secs(1), tracker.wait())
        .await
        .unwrap();
    task.await.unwrap();
    assert!(!parent.is_cancelled());
}

// A feature-local specimen, not a generic executor or a production API.
const CPU_MAX_BYTES: usize = 64 * 1024;
const CPU_RECORD_BYTES: usize = size_of::<u128>();
const CPU_MAX_ITEMS: usize = 4096;
const CPU_MAX_ROUNDS: u32 = 4096;
const CPU_TEST_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq)]
enum CpuAdmissionError {
    InvalidInput,
    Busy,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CpuFailure {
    Computation,
    Panicked,
    NotStarted,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CpuCompletions {
    succeeded: usize,
    failed: usize,
    panicked: usize,
    not_started: usize,
    abandoned_results: usize,
}

struct CpuRecipe {
    admission: Arc<Semaphore>,
    tasks: TaskTracker,
    completions: Arc<Mutex<CpuCompletions>>,
}

impl CpuRecipe {
    fn new() -> Self {
        Self {
            admission: Arc::new(Semaphore::new(1)),
            tasks: TaskTracker::new(),
            completions: Arc::new(Mutex::new(CpuCompletions::default())),
        }
    }

    // Mutable access serializes registration with close: TaskTracker::close
    // alone neither closes admission nor prevents new task registrations.
    fn submit(
        &mut self,
        source: &[u8],
        rounds: u32,
        rendezvous: Option<CpuRendezvous>,
    ) -> Result<oneshot::Receiver<Result<Vec<u8>, CpuFailure>>, CpuAdmissionError> {
        if source.len() > CPU_MAX_BYTES
            || !source.len().is_multiple_of(CPU_RECORD_BYTES)
            || source.len() / CPU_RECORD_BYTES > CPU_MAX_ITEMS
            || rounds > CPU_MAX_ROUNDS
        {
            return Err(CpuAdmissionError::InvalidInput);
        }
        let permit =
            Arc::clone(&self.admission)
                .try_acquire_owned()
                .map_err(|error| match error {
                    TryAcquireError::NoPermits => CpuAdmissionError::Busy,
                    TryAcquireError::Closed => CpuAdmissionError::Closed,
                })?;

        // Admission precedes payload copying, decoding and spawning. Copy only
        // this slice: a tiny Bytes view could retain a much larger allocation.
        let source = source.to_vec();
        let blocking = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if let Some(rendezvous) = rendezvous {
                rendezvous.arrive()?;
            }
            let mut result = Vec::with_capacity(source.len());
            for record in source.as_chunks::<CPU_RECORD_BYTES>().0 {
                let mut value = u128::from_le_bytes(*record);
                for round in 0..rounds {
                    value = std::hint::black_box(value).rotate_left(7).wrapping_mul(3)
                        ^ u128::from(round);
                }
                result.extend_from_slice(&value.to_le_bytes());
            }
            Ok(result)
        });
        let (sender, receiver) = oneshot::channel();
        let completions = Arc::clone(&self.completions);
        // The tracked observer owns the blocking JoinHandle, independently of
        // the result receiver. It observes panics even after a caller timeout.
        let _observer = self.tasks.spawn(async move {
            let result = match blocking.await {
                Ok(result) => result,
                Err(error) if error.is_panic() => Err(CpuFailure::Panicked),
                Err(_) => Err(CpuFailure::NotStarted),
            };
            let outcome = result.as_ref().map(|_| ()).map_err(|error| *error);
            let abandoned = sender.send(result).is_err();
            let mut counts = completions.lock().unwrap();
            match outcome {
                Ok(()) => counts.succeeded += 1,
                Err(CpuFailure::Computation) => counts.failed += 1,
                Err(CpuFailure::Panicked) => counts.panicked += 1,
                Err(CpuFailure::NotStarted) => counts.not_started += 1,
            }
            counts.abandoned_results += usize::from(abandoned);
        });
        Ok(receiver)
    }

    fn close(&mut self) {
        self.admission.close();
        self.tasks.close();
    }

    async fn wait(&self) -> CpuCompletions {
        self.tasks.wait().await;
        *self.completions.lock().unwrap()
    }
}

// Only the executable tests use this bounded rendezvous/fault control. A real
// feature copies its finite computation and lifecycle, not this instrumentation.
enum CpuTestFinish {
    Compute,
    Fail,
    Panic,
}

struct CpuRendezvous {
    started: oneshot::Sender<()>,
    release: mpsc::Receiver<CpuTestFinish>,
}

impl CpuRendezvous {
    fn new() -> (Self, oneshot::Receiver<()>, mpsc::SyncSender<CpuTestFinish>) {
        let (started, observed) = oneshot::channel();
        let (release, held) = mpsc::sync_channel(1);
        (
            Self {
                started,
                release: held,
            },
            observed,
            release,
        )
    }

    fn arrive(self) -> Result<(), CpuFailure> {
        let _ = self.started.send(());
        match self.release.recv_timeout(CPU_TEST_DEADLINE).unwrap() {
            CpuTestFinish::Compute => Ok(()),
            CpuTestFinish::Fail => Err(CpuFailure::Computation),
            CpuTestFinish::Panic => panic!("bounded CPU recipe panic fixture"),
        }
    }
}

#[tokio::test]
async fn cpu_recipe_enforces_finite_input_and_returns_a_separately_owned_result() {
    let mut recipe = CpuRecipe::new();
    for (source, rounds) in [
        (vec![0; CPU_MAX_BYTES + CPU_RECORD_BYTES], 1),
        (vec![0; CPU_RECORD_BYTES - 1], 1),
        (vec![0; CPU_RECORD_BYTES], CPU_MAX_ROUNDS + 1),
    ] {
        assert!(matches!(
            recipe.submit(&source, rounds, None),
            Err(CpuAdmissionError::InvalidInput)
        ));
    }
    let source = [1_u128.to_le_bytes(), 0_u128.to_le_bytes()].concat();
    let result = recipe.submit(&source, 2, None).unwrap();
    drop(source);
    let result = tokio::time::timeout(CPU_TEST_DEADLINE, result)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    // Independent two-round arithmetic: 1 -> 384 -> 147457; 0 -> 0 -> 1.
    assert_eq!(
        result,
        [147_457_u128.to_le_bytes(), 1_u128.to_le_bytes()].concat()
    );
    let maximum = recipe
        .submit(&vec![0; CPU_MAX_BYTES], CPU_MAX_ROUNDS, None)
        .unwrap();
    recipe.close();
    let maximum = tokio::time::timeout(CPU_TEST_DEADLINE, maximum)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let counts = tokio::time::timeout(CPU_TEST_DEADLINE, recipe.wait())
        .await
        .unwrap();
    assert_eq!(counts.succeeded, 2);
    assert_eq!(maximum.len(), CPU_MAX_BYTES);
    // Completed results remain caller-owned after actual capacity is released.
    drop(recipe);
    assert_eq!(result.len(), 2 * CPU_RECORD_BYTES);
}

#[tokio::test]
async fn cancelling_a_cpu_waiter_does_not_admit_replacement_or_complete_the_join() {
    let mut recipe = CpuRecipe::new();
    let (rendezvous, started, release) = CpuRendezvous::new();
    let waiter = recipe
        .submit(&1_u128.to_le_bytes(), 2, Some(rendezvous))
        .unwrap();
    tokio::time::timeout(CPU_TEST_DEADLINE, started)
        .await
        .unwrap()
        .unwrap();
    drop(waiter);
    assert!(matches!(
        recipe.submit(&1_u128.to_le_bytes(), 2, None),
        Err(CpuAdmissionError::Busy)
    ));
    recipe.close();
    assert!(matches!(
        recipe.submit(&1_u128.to_le_bytes(), 2, None),
        Err(CpuAdmissionError::Closed)
    ));
    // This is an incomplete wait, not cancellation of the blocking closure.
    assert!(
        tokio::time::timeout(Duration::from_millis(1), recipe.wait())
            .await
            .is_err()
    );
    release.try_send(CpuTestFinish::Compute).unwrap();
    let counts = tokio::time::timeout(CPU_TEST_DEADLINE, recipe.wait())
        .await
        .unwrap();
    assert_eq!(
        counts,
        CpuCompletions {
            succeeded: 1,
            abandoned_results: 1,
            ..CpuCompletions::default()
        }
    );
    assert_eq!(recipe.admission.available_permits(), 1);
}

#[tokio::test]
async fn cpu_completion_owner_retains_failure_and_panic_after_waiter_cancellation() {
    for (finish, expected) in [
        (
            CpuTestFinish::Fail,
            CpuCompletions {
                failed: 1,
                abandoned_results: 1,
                ..CpuCompletions::default()
            },
        ),
        (
            CpuTestFinish::Panic,
            CpuCompletions {
                panicked: 1,
                abandoned_results: 1,
                ..CpuCompletions::default()
            },
        ),
    ] {
        let mut recipe = CpuRecipe::new();
        let (rendezvous, started, release) = CpuRendezvous::new();
        let waiter = recipe
            .submit(&1_u128.to_le_bytes(), 2, Some(rendezvous))
            .unwrap();
        tokio::time::timeout(CPU_TEST_DEADLINE, started)
            .await
            .unwrap()
            .unwrap();
        drop(waiter);
        recipe.close();
        release.try_send(finish).unwrap();
        assert_eq!(
            tokio::time::timeout(CPU_TEST_DEADLINE, recipe.wait())
                .await
                .unwrap(),
            expected
        );
        assert_eq!(recipe.admission.available_permits(), 1);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FetchError {
    Transient,
    Permanent,
}

#[tokio::test]
async fn retry_is_bounded_and_only_repeats_eligible_failures() {
    let calls = AtomicUsize::new(0);
    let operation = || async {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(FetchError::Transient)
        } else {
            Ok(42)
        }
    };
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        operation
            .retry(
                ConstantBuilder::default()
                    .with_delay(Duration::from_millis(1))
                    .with_max_times(2),
            )
            .when(|error| *error == FetchError::Transient),
    )
    .await
    .unwrap();
    assert_eq!(result, Ok(42));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let calls = AtomicUsize::new(0);
    let operation = || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Err::<(), _>(FetchError::Permanent)
    };
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        operation
            .retry(ConstantBuilder::default().with_max_times(2))
            .when(|error| *error == FetchError::Transient),
    )
    .await
    .unwrap();
    assert_eq!(result, Err(FetchError::Permanent));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // These are read-like operations. Cancellation/retry never rolls back an
    // already accepted remote effect, and CommitUnknown must not enter this loop.
}

#[tokio::test]
async fn a_local_cache_has_explicit_capacity_expiration_and_invalidation() {
    let cache = moka::future::Cache::<String, u32>::builder()
        .max_capacity(32)
        .time_to_live(Duration::from_secs(60))
        .build();
    let loads = AtomicUsize::new(0);
    for _ in 0..2 {
        let value = cache
            .get_with("item".to_owned(), async {
                loads.fetch_add(1, Ordering::SeqCst);
                7
            })
            .await;
        assert_eq!(value, 7);
    }
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    cache.invalidate("item").await;
    assert_eq!(cache.get("item").await, None);
    // This example does not claim distributed consistency or exact LRU eviction.
}
