# Cancellation safety, pinned timers, and unlock-before-drop

Use this reference when a `loop { tokio::select! }` mixes timers, channels, or shutdown signals, or when state updates inside a lock must trigger cleanup or notifications.

## Pinned timers and two-phase channel sends in `loop { select! }`

Creating `tokio::time::sleep` inside `loop { select! }` resets the timer on every received event, starving the timeout. Awaiting `tx.send(msg)` directly inside `select!` drops `msg` if another arm wins. Pin the timer outside the loop and reserve a `Permit` before committing the message synchronously:

```rust
use std::time::Duration;

async fn forward_with_idle_timeout(
    mut rx: tokio::sync::mpsc::Receiver<String>,
    tx: tokio::sync::mpsc::Sender<String>,
    idle_limit: Duration,
) {
    let deadline = tokio::time::sleep(idle_limit);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            () = &mut deadline => break,
            permit = tx.reserve() => {
                let Ok(permit) = permit else { break };
                let Some(msg) = rx.recv().await else { break };
                permit.send(msg);
                deadline.as_mut().reset(tokio::time::Instant::now() + idle_limit);
            }
        }
    }
}
```

Wait for `tx.reserve()` only after an item is ready when backpressure must not hold a permit across `rx.recv().await`; the key invariant is separating the cancel-safe wait (`reserve().await`) from the infallible move (`permit.send(msg)`). For periodic ticks, use `tokio::time::interval` with an explicit `MissedTickBehavior`.

## Release locks before dropping collections or notifying waiters

Collect evicted buffers or wakeup channels inside the critical section, drop the mutex guard, and drop or notify outside the lock (in this repository, inspect [`AfterUnlock`](../../../../crates/infra-postgres/src/slots.rs) and [`PrimeLease`](../../../../crates/infra-clickhouse/src/metadata.rs)):

```rust
use std::collections::VecDeque;
use std::sync::Mutex;

struct QueueState {
    items: VecDeque<Vec<u8>>,
}

struct AfterUnlock {
    evicted: Vec<Vec<u8>>,
}

fn clear_queue(shared: &Mutex<QueueState>) -> usize {
    let after = {
        let mut guard = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        AfterUnlock {
            evicted: guard.items.drain(..).collect(),
        }
    };
    after.evicted.len()
}
```

**When to stay plain:** use a short `parking_lot::Mutex` rather than an Actor task + channel when shared state has no autonomous timers, batching, or non-`Sync` I/O. Only add a background task when an independent loop owns real lifecycle work.

Sources: [Oxide RFD 400: Cancel Safety in Async Rust](https://rfd.shared.oxide.computer/rfd/400), [Alice Ryhl, *Actors with Tokio*](https://ryhl.io/blog/actors-with-tokio/), and [Mara Bos, *Rust Atomics and Locks* (Ch. 1–4)](https://marabos.nl/atomics/).
