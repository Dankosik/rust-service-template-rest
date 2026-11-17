use std::collections::BTreeMap;
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
    regular_waiters: BTreeMap<u64, oneshot::Sender<()>>,
    regular_waiters_since: Option<Instant>,
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
                regular_waiters: BTreeMap::new(),
                regular_waiters_since: None,
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
                Queue::Regular => {
                    if state.regular_waiters.is_empty() {
                        state.regular_waiters_since = Some(Instant::now());
                    }
                    state.regular_waiters.insert(id, grant);
                }
            }
            (id, granted)
        };
        let mut wait = Wait {
            slots: self,
            queue,
            id,
            granted,
            pending: true,
        };
        let _ = (&mut wait.granted).await;
        wait.pending = false;
        Slot(Arc::clone(self))
    }

    fn release(&self) {
        let mut state = self.state.lock();
        while let Some((_, grant)) = state.probes.pop_first() {
            if grant.send(()).is_ok() {
                return;
            }
        }
        let now = Instant::now();
        loop {
            let newest_first = state
                .regular_waiters_since
                .is_some_and(|since| now.saturating_duration_since(since) > STANDING_AFTER);
            let next = if newest_first {
                state.regular_waiters.pop_last()
            } else {
                state.regular_waiters.pop_first()
            };
            if state.regular_waiters.is_empty() {
                state.regular_waiters_since = None;
            }
            let Some((_, grant)) = next else {
                state.free += 1;
                return;
            };
            if grant.send(()).is_ok() {
                return;
            }
        }
    }
}

struct Wait<'a> {
    slots: &'a Slots,
    queue: Queue,
    id: u64,
    granted: oneshot::Receiver<()>,
    pending: bool,
}

impl Drop for Wait<'_> {
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        let queued = {
            let mut state = self.slots.state.lock();
            match self.queue {
                Queue::Probe => state.probes.remove(&self.id).is_some(),
                Queue::Regular => {
                    let queued = state.regular_waiters.remove(&self.id).is_some();
                    if state.regular_waiters.is_empty() {
                        state.regular_waiters_since = None;
                    }
                    queued
                }
            }
        };
        if !queued {
            self.slots.release();
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
    async fn a_standing_queue_serves_the_newest_waiter_first() {
        let slots = Slots::new(1);
        let held = slots.try_take();
        let mut older = pin!(slots.take(Queue::Regular));
        assert!(poll(older.as_mut()).is_pending());
        tokio::time::advance(STANDING_AFTER + Duration::from_millis(1)).await;
        let mut newer = pin!(slots.take(Queue::Regular));
        assert!(poll(newer.as_mut()).is_pending());

        drop(held);

        assert!(poll(older.as_mut()).is_pending());
        let Poll::Ready(slot) = poll(newer.as_mut()) else {
            panic!("the newest waiter was not served");
        };
        drop(slot);
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
