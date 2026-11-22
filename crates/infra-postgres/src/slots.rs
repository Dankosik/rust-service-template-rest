use std::collections::BTreeMap;
use std::mem::DropGuard;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::oneshot;
use tokio::time::{Instant, error::Elapsed, timeout_at};

/// A queue that has not emptied for this long is standing: its oldest
/// waiters have probably been given up on by their callers, so a freed slot
/// goes to the newest one. A younger queue is a burst and is served in
/// arrival order. The rule and the value are the adaptive LIFO of "Fail at
/// Scale" (ACM Queue, 2015).
const STANDING_AFTER: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(crate) struct Slots {
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    free: usize,
    next_waiter: u64,
    probes: BTreeMap<u64, oneshot::Sender<()>>,
    regular: RegularQueue,
}

#[derive(Debug, Default)]
struct RegularQueue(Option<ActiveRegularQueue>);

#[derive(Debug)]
struct ActiveRegularQueue {
    since: Instant,
    waiters: BTreeMap<u64, oneshot::Sender<()>>,
}

struct DequeuedWaiter {
    grant: oneshot::Sender<()>,
    emptied_queue: Option<ActiveRegularQueue>,
}

impl RegularQueue {
    fn push(&mut self, id: u64, grant: oneshot::Sender<()>) {
        let active = self.0.get_or_insert_with(|| ActiveRegularQueue {
            since: Instant::now(),
            waiters: BTreeMap::new(),
        });
        active.waiters.insert(id, grant);
    }

    fn pop(&mut self) -> Option<DequeuedWaiter> {
        let active = self.0.as_mut()?;
        let newest_first = active.waiters.len() > 1 && active.since.elapsed() > STANDING_AFTER;
        let (_, grant) = if newest_first {
            active.waiters.pop_last()
        } else {
            active.waiters.pop_first()
        }?;
        Some(self.dequeued(grant))
    }

    fn remove(&mut self, id: u64) -> Option<DequeuedWaiter> {
        let grant = self.0.as_mut()?.waiters.remove(&id)?;
        Some(self.dequeued(grant))
    }

    fn dequeued(&mut self, grant: oneshot::Sender<()>) -> DequeuedWaiter {
        DequeuedWaiter {
            grant,
            emptied_queue: self.0.take_if(|active| active.waiters.is_empty()),
        }
    }
}

impl DequeuedWaiter {
    fn probe(grant: oneshot::Sender<()>) -> Self {
        Self {
            grant,
            emptied_queue: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Queue {
    Probe,
    Regular,
}

#[derive(Debug)]
#[must_use = "the slot is released when this guard is dropped"]
pub(crate) struct Slot<'a>(&'a Slots);

impl Slots {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(State {
                free: capacity,
                next_waiter: 0,
                probes: BTreeMap::new(),
                regular: RegularQueue::default(),
            }),
        }
    }

    pub(super) async fn take(&self, queue: Queue, deadline: Instant) -> Result<Slot<'_>, Elapsed> {
        let (id, granted) = {
            let mut state = self.state.lock();
            if state.free > 0 {
                state.free -= 1;
                return Ok(Slot(self));
            }
            let id = state.next_waiter;
            state.next_waiter += 1;
            let (grant, granted) = oneshot::channel();
            match queue {
                Queue::Probe => {
                    state.probes.insert(id, grant);
                }
                Queue::Regular => state.regular.push(id, grant),
            }
            (id, granted)
        };
        let mut wait = DropGuard::new(granted, |receiver| self.cancel_wait(queue, id, receiver));
        let _ = timeout_at(deadline, &mut *wait).await?;
        let _receiver = DropGuard::dismiss(wait);
        Ok(Slot(self))
    }

    fn release(&self) {
        let handoff = {
            let mut state = self.state.lock();
            let handoff = state
                .probes
                .pop_first()
                .map(|(_, grant)| DequeuedWaiter::probe(grant))
                .or_else(|| state.regular.pop());
            if handoff.is_none() {
                state.free += 1;
            }
            handoff
        };
        if let Some(DequeuedWaiter {
            grant,
            emptied_queue,
        }) = handoff
        {
            let _ = grant.send(());
            drop(emptied_queue);
        }
    }

    fn cancel_wait(&self, queue: Queue, id: u64, receiver: oneshot::Receiver<()>) {
        let removed = {
            let mut state = self.state.lock();
            match queue {
                Queue::Probe => state.probes.remove(&id).map(DequeuedWaiter::probe),
                Queue::Regular => state.regular.remove(id),
            }
        };
        drop(receiver);
        match removed {
            Some(waiter) => drop(waiter),
            None => self.release(),
        }
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[cfg(test)]
mod tests {
    use std::pin::{Pin, pin};
    use std::task::{Context, Poll, Waker};

    use super::*;

    fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    async fn take(slots: &Slots, queue: Queue) -> Slot<'_> {
        slots
            .take(queue, Instant::now() + Duration::from_secs(1))
            .await
            .unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn a_free_slot_is_taken_without_waiting() {
        let slots = Slots::new(2);
        let first = poll(pin!(take(&slots, Queue::Regular)));
        let second = poll(pin!(take(&slots, Queue::Regular)));
        assert!(first.is_ready());
        assert!(second.is_ready());
        assert!(poll(pin!(take(&slots, Queue::Regular))).is_pending());
    }

    #[tokio::test(start_paused = true)]
    async fn a_young_queue_is_served_in_arrival_order() {
        let slots = Slots::new(1);
        let held = take(&slots, Queue::Regular).await;
        let mut older = pin!(take(&slots, Queue::Regular));
        let mut newer = pin!(take(&slots, Queue::Regular));
        assert!(poll(older.as_mut()).is_pending());
        assert!(poll(newer.as_mut()).is_pending());

        drop(held);

        assert!(poll(newer.as_mut()).is_pending());
        assert!(poll(older.as_mut()).is_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn a_queue_that_emptied_starts_young_again() {
        let slots = Slots::new(1);
        let held = take(&slots, Queue::Regular).await;
        {
            let mut gone = pin!(take(&slots, Queue::Regular));
            assert!(poll(gone.as_mut()).is_pending());
            tokio::time::advance(STANDING_AFTER * 2).await;
        }
        let mut older = pin!(take(&slots, Queue::Regular));
        let mut newer = pin!(take(&slots, Queue::Regular));
        assert!(poll(older.as_mut()).is_pending());
        assert!(poll(newer.as_mut()).is_pending());

        drop(held);

        assert!(poll(older.as_mut()).is_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn an_abandoned_wait_leaves_the_slot_free() {
        let slots = Slots::new(1);
        let held = take(&slots, Queue::Regular).await;
        {
            let mut abandoned = pin!(take(&slots, Queue::Regular));
            assert!(poll(abandoned.as_mut()).is_pending());
        }

        drop(held);

        assert!(poll(pin!(take(&slots, Queue::Regular))).is_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn a_slot_granted_to_a_wait_dropped_unpolled_is_passed_on() {
        let slots = Slots::new(1);
        let held = take(&slots, Queue::Regular).await;
        let mut next = pin!(take(&slots, Queue::Regular));
        {
            let mut granted_then_dropped = pin!(take(&slots, Queue::Regular));
            assert!(poll(granted_then_dropped.as_mut()).is_pending());
            assert!(poll(next.as_mut()).is_pending());
            drop(held);
        }

        assert!(poll(next.as_mut()).is_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn probe_waiters_precede_standing_newest_first_waiters_and_never_strand_slots() {
        let slots = Slots::new(1);
        let held = take(&slots, Queue::Regular).await;
        let mut oldest = pin!(take(&slots, Queue::Regular));
        assert!(poll(oldest.as_mut()).is_pending());
        tokio::time::advance(STANDING_AFTER + Duration::from_millis(1)).await;
        let mut probe = pin!(take(&slots, Queue::Probe));
        assert!(poll(probe.as_mut()).is_pending());
        let mut newer = pin!(take(&slots, Queue::Regular));
        let mut newest = pin!(take(&slots, Queue::Regular));
        assert!(poll(newer.as_mut()).is_pending());
        assert!(poll(newest.as_mut()).is_pending());

        drop(held);

        assert!(poll(oldest.as_mut()).is_pending());
        assert!(poll(newer.as_mut()).is_pending());
        assert!(poll(newest.as_mut()).is_pending());
        let Poll::Ready(probe_slot) = poll(probe.as_mut()) else {
            panic!("the probe waiter was not served first");
        };

        {
            let mut abandoned_probe = pin!(take(&slots, Queue::Probe));
            assert!(poll(abandoned_probe.as_mut()).is_pending());
        }
        {
            let mut granted_then_dropped_probe = pin!(take(&slots, Queue::Probe));
            assert!(poll(granted_then_dropped_probe.as_mut()).is_pending());
            drop(probe_slot);
        }

        assert!(poll(oldest.as_mut()).is_pending());
        assert!(poll(newer.as_mut()).is_pending());
        let Poll::Ready(newest_slot) = poll(newest.as_mut()) else {
            panic!("cancelled probe wait stranded the slot or reset standing order");
        };
        drop(newest_slot);
        let Poll::Ready(newer_slot) = poll(newer.as_mut()) else {
            panic!("the next newest waiter was not served");
        };
        drop(newer_slot);
        let Poll::Ready(oldest_slot) = poll(oldest.as_mut()) else {
            panic!("the oldest waiter was not served");
        };
        drop(oldest_slot);
        assert!(poll(pin!(take(&slots, Queue::Regular))).is_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn a_single_standing_waiter_receives_the_slot_and_resets_queue_age() {
        let slots = Slots::new(1);
        let held = take(&slots, Queue::Regular).await;
        let mut lone = pin!(take(&slots, Queue::Regular));
        assert!(poll(lone.as_mut()).is_pending());
        tokio::time::advance(STANDING_AFTER * 2).await;

        drop(held);
        let Poll::Ready(lone_slot) = poll(lone.as_mut()) else {
            panic!("the single standing waiter was not served");
        };

        let mut older = pin!(take(&slots, Queue::Regular));
        let mut newer = pin!(take(&slots, Queue::Regular));
        assert!(poll(older.as_mut()).is_pending());
        assert!(poll(newer.as_mut()).is_pending());
        drop(lone_slot);
        assert!(poll(newer.as_mut()).is_pending());
        assert!(poll(older.as_mut()).is_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_queued_wait_does_not_wake_and_release_wakes_outside_the_lock() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::task::Wake;

        struct LockCheckingWake {
            slots: Arc<Slots>,
            wakes: AtomicUsize,
            unlocked_on_wake: AtomicBool,
        }

        impl Wake for LockCheckingWake {
            fn wake(self: Arc<Self>) {
                self.wake_by_ref();
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.wakes.fetch_add(1, Ordering::Relaxed);
                if self.slots.state.try_lock().is_none() {
                    self.unlocked_on_wake.store(false, Ordering::Relaxed);
                }
            }
        }

        let slots = Arc::new(Slots::new(1));
        let held = take(&slots, Queue::Regular).await;
        for queue in [Queue::Regular, Queue::Probe] {
            let wake = Arc::new(LockCheckingWake {
                slots: Arc::clone(&slots),
                wakes: AtomicUsize::new(0),
                unlocked_on_wake: AtomicBool::new(true),
            });
            let waker = Waker::from(Arc::clone(&wake));
            {
                let mut wait = pin!(take(&slots, queue));
                assert!(
                    wait.as_mut()
                        .poll(&mut Context::from_waker(&waker))
                        .is_pending()
                );
            }
            assert_eq!(wake.wakes.load(Ordering::Relaxed), 0);
        }

        let mut held = Some(held);
        for queue in [Queue::Regular, Queue::Probe] {
            let wake = Arc::new(LockCheckingWake {
                slots: Arc::clone(&slots),
                wakes: AtomicUsize::new(0),
                unlocked_on_wake: AtomicBool::new(true),
            });
            let waker = Waker::from(Arc::clone(&wake));
            let mut wait = pin!(take(&slots, queue));
            assert!(
                wait.as_mut()
                    .poll(&mut Context::from_waker(&waker))
                    .is_pending()
            );
            drop(held.take());
            assert_eq!(wake.wakes.load(Ordering::Relaxed), 1);
            assert!(wake.unlocked_on_wake.load(Ordering::Relaxed));
            let Poll::Ready(slot) = wait.as_mut().poll(&mut Context::from_waker(&waker)) else {
                panic!("released slot did not reach the waiting future");
            };
            held = Some(slot);
        }
        drop(held);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_releases_and_cancellations_conserve_every_slot() {
        use std::sync::Arc;

        const CAPACITY: usize = 2;
        const WORKERS: usize = 8;
        const ROUNDS: usize = 64;

        async fn exercise_round(slots: &Slots, step: usize) {
            let queue = if step.is_multiple_of(4) {
                Queue::Probe
            } else {
                Queue::Regular
            };
            let deadline = Instant::now() + Duration::from_secs(5);
            if step.is_multiple_of(3) {
                tokio::select! {
                    biased;
                    slot = slots.take(queue, deadline) => {
                        let _slot = slot.unwrap();
                        tokio::task::yield_now().await;
                    }
                    () = tokio::task::yield_now() => {}
                }
            } else if let Ok(_slot) = slots.take(queue, deadline).await {
                tokio::task::yield_now().await;
            }
        }

        let slots = Arc::new(Slots::new(CAPACITY));
        let mut tasks = tokio::task::JoinSet::new();
        for worker in 0..WORKERS {
            let slots = Arc::clone(&slots);
            tasks.spawn(async move {
                for round in 0..ROUNDS {
                    exercise_round(&slots, worker + round).await;
                }
            });
        }
        while let Some(joined) = tasks.join_next().await {
            joined.unwrap();
        }
        let state = slots.state.lock();
        assert_eq!(state.free, CAPACITY);
        assert!(state.probes.is_empty());
        assert!(state.regular.0.is_none());
    }
}
