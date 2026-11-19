use std::collections::BTreeMap;
use std::mem::DropGuard;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::oneshot;
use tokio::time::Instant;

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

impl RegularQueue {
    fn push(&mut self, id: u64, grant: oneshot::Sender<()>) {
        let active = self.0.get_or_insert_with(|| ActiveRegularQueue {
            since: Instant::now(),
            waiters: BTreeMap::new(),
        });
        active.waiters.insert(id, grant);
    }

    fn is_standing(&self) -> bool {
        self.0.as_ref().is_some_and(|active| {
            Instant::now().saturating_duration_since(active.since) > STANDING_AFTER
        })
    }

    fn pop(&mut self, newest_first: bool) -> Option<oneshot::Sender<()>> {
        let active = self.0.as_mut()?;
        let (_, grant) = if newest_first {
            active.waiters.pop_last()
        } else {
            active.waiters.pop_first()
        }?;
        if active.waiters.is_empty() {
            self.0 = None;
        }
        Some(grant)
    }

    fn remove(&mut self, id: u64) -> bool {
        let Some(active) = self.0.as_mut() else {
            return false;
        };
        let removed = active.waiters.remove(&id).is_some();
        if active.waiters.is_empty() {
            self.0 = None;
        }
        removed
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Queue {
    Probe,
    Regular,
}

#[derive(Debug)]
#[must_use = "the slot is released when this guard is dropped"]
pub(crate) struct Slot(Arc<Slots>);

impl Slots {
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                free: capacity,
                next_waiter: 0,
                probes: BTreeMap::new(),
                regular: RegularQueue::default(),
            }),
        })
    }

    pub(crate) fn try_take(self: &Arc<Self>) -> Option<Slot> {
        let mut state = self.state.lock();
        state.free = state.free.checked_sub(1)?;
        Some(Slot(Arc::clone(self)))
    }

    pub(super) async fn take(self: &Arc<Self>, queue: Queue) -> Slot {
        let (id, granted) = {
            let mut state = self.state.lock();
            if let Some(free) = state.free.checked_sub(1) {
                state.free = free;
                return Slot(Arc::clone(self));
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
        let mut wait = DropGuard::new(granted, |_receiver| self.cancel_wait(queue, id));
        let _ = (&mut *wait).await;
        let _receiver = DropGuard::dismiss(wait);
        Slot(Arc::clone(self))
    }

    fn release(&self) {
        let mut state = self.state.lock();
        while let Some((_, grant)) = state.probes.pop_first() {
            if grant.send(()).is_ok() {
                return;
            }
        }
        let newest_first = state.regular.is_standing();
        while let Some(grant) = state.regular.pop(newest_first) {
            if grant.send(()).is_ok() {
                return;
            }
        }
        state.free += 1;
    }

    fn cancel_wait(&self, queue: Queue, id: u64) {
        let queued = {
            let mut state = self.state.lock();
            match queue {
                Queue::Probe => state.probes.remove(&id).is_some(),
                Queue::Regular => state.regular.remove(id),
            }
        };
        if !queued {
            self.release();
        }
    }
}

impl Drop for Slot {
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

    #[tokio::test(start_paused = true)]
    async fn a_free_slot_is_taken_without_waiting() {
        let slots = Slots::new(2);
        let first = slots.try_take();
        let second = poll(pin!(slots.take(Queue::Regular)));
        assert!(first.is_some());
        assert!(second.is_ready());
        assert!(slots.try_take().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_young_queue_is_served_in_arrival_order() {
        let slots = Slots::new(1);
        let held = slots.try_take();
        let mut older = pin!(slots.take(Queue::Regular));
        let mut newer = pin!(slots.take(Queue::Regular));
        assert!(poll(older.as_mut()).is_pending());
        assert!(poll(newer.as_mut()).is_pending());

        drop(held);

        assert!(poll(newer.as_mut()).is_pending());
        assert!(poll(older.as_mut()).is_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn a_queue_that_emptied_starts_young_again() {
        let slots = Slots::new(1);
        let held = slots.try_take();
        {
            let mut gone = pin!(slots.take(Queue::Regular));
            assert!(poll(gone.as_mut()).is_pending());
            tokio::time::advance(STANDING_AFTER * 2).await;
        }
        let mut older = pin!(slots.take(Queue::Regular));
        let mut newer = pin!(slots.take(Queue::Regular));
        assert!(poll(older.as_mut()).is_pending());
        assert!(poll(newer.as_mut()).is_pending());

        drop(held);

        assert!(poll(older.as_mut()).is_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn an_abandoned_wait_leaves_the_slot_free() {
        let slots = Slots::new(1);
        let held = slots.try_take();
        {
            let mut abandoned = pin!(slots.take(Queue::Regular));
            assert!(poll(abandoned.as_mut()).is_pending());
        }

        drop(held);

        assert!(slots.try_take().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_slot_granted_to_a_wait_dropped_unpolled_is_passed_on() {
        let slots = Slots::new(1);
        let held = slots.try_take();
        let mut next = pin!(slots.take(Queue::Regular));
        {
            let mut granted_then_dropped = pin!(slots.take(Queue::Regular));
            assert!(poll(granted_then_dropped.as_mut()).is_pending());
            assert!(poll(next.as_mut()).is_pending());
            drop(held);
        }

        assert!(poll(next.as_mut()).is_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn probe_waiters_precede_standing_newest_first_waiters_and_never_strand_slots() {
        let slots = Slots::new(1);
        let held = slots.try_take();
        let mut oldest = pin!(slots.take(Queue::Regular));
        assert!(poll(oldest.as_mut()).is_pending());
        tokio::time::advance(STANDING_AFTER + Duration::from_millis(1)).await;
        let mut probe = pin!(slots.take(Queue::Probe));
        assert!(poll(probe.as_mut()).is_pending());
        let mut newer = pin!(slots.take(Queue::Regular));
        let mut newest = pin!(slots.take(Queue::Regular));
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
            let mut abandoned_probe = pin!(slots.take(Queue::Probe));
            assert!(poll(abandoned_probe.as_mut()).is_pending());
        }
        {
            let mut granted_then_dropped_probe = pin!(slots.take(Queue::Probe));
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
        assert!(slots.try_take().is_some());
    }
}
