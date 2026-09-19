//! Async recipes: bounded work, explicit order, cancellation and operation-owned retry policy.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use backon::{ConstantBuilder, Retryable};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures_util::{StreamExt, TryStreamExt, stream};
use tokio::io::AsyncReadExt;
use tokio_util::io::{ReaderStream, StreamReader};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

#[tokio::test]
async fn fanout_is_bounded_without_spawning_one_task_per_item() {
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let work = stream::iter(0..12)
        .map(|id| async {
            let running = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(running, Ordering::SeqCst);
            tokio::task::yield_now().await;
            active.fetch_sub(1, Ordering::SeqCst);
            Ok::<_, std::convert::Infallible>(id * 2)
        })
        // buffered retains input order; buffer_unordered yields completion order.
        .buffered(3)
        .try_collect::<Vec<_>>();
    let values = tokio::time::timeout(Duration::from_secs(1), work).await.unwrap().unwrap();
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
    tracker.spawn(async move {
        let result = worker.run_until_cancelled(std::future::pending::<()>()).await;
        assert!(result.is_none());
    });
    tracker.close();
    child.cancel();
    tokio::time::timeout(Duration::from_secs(1), tracker.wait()).await.unwrap();
    assert!(!parent.is_cancelled());
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
        operation.retry(ConstantBuilder::default().with_delay(Duration::from_millis(1)).with_max_times(2))
            .when(|error| *error == FetchError::Transient),
    ).await.unwrap();
    assert_eq!(result, Ok(42));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let calls = AtomicUsize::new(0);
    let operation = || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Err::<(), _>(FetchError::Permanent)
    };
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        operation.retry(ConstantBuilder::default().with_max_times(2))
            .when(|error| *error == FetchError::Transient),
    ).await.unwrap();
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
        let value = cache.get_with("item".to_owned(), async {
            loads.fetch_add(1, Ordering::SeqCst);
            7
        }).await;
        assert_eq!(value, 7);
    }
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    cache.invalidate("item").await;
    assert_eq!(cache.get("item").await, None);
    // This example does not claim distributed consistency or exact LRU eviction.
}
