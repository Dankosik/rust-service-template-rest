use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::Instant;

/// A queue that has not emptied for this long is standing: its oldest
/// waiters have probably been given up on by their callers, so a freed slot
/// goes to the newest one. A younger queue is a burst and is served in
/// arrival order. The rule and the value are the adaptive LIFO of "Fail at
/// Scale" (ACM Queue, 2015).
const STANDING_AFTER: Duration = Duration::from_millis(100);

/// A fixed number of slots with a wait queue. Callers bound the wait
/// themselves; an abandoned wait never strands a slot.
#[derive(Debug)]
pub(crate) struct Slots {
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    free: usize,
    next_waiter: u64,
    waiters: BTreeMap<u64, oneshot::Sender<()>>,
    occupied_since: Option<Instant>,
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
                waiters: BTreeMap::new(),
                occupied_since: None,
            }),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn try_take(self: &Arc<Self>) -> Option<Slot> {
        let mut state = self.state();
        state.free = state.free.checked_sub(1)?;
        Some(Slot(Arc::clone(self)))
    }

    pub(crate) async fn take(self: &Arc<Self>) -> Slot {
        let (id, granted) = {
            let mut state = self.state();
            if let Some(free) = state.free.checked_sub(1) {
                state.free = free;
                return Slot(Arc::clone(self));
            }
            let id = state.next_waiter;
            state.next_waiter += 1;
            let (grant, granted) = oneshot::channel();
            if state.waiters.is_empty() {
                state.occupied_since = Some(Instant::now());
            }
            state.waiters.insert(id, grant);
            (id, granted)
        };
        let mut wait = Wait {
            slots: self,
            id,
            granted: Some(granted),
        };
        if let Some(granted) = wait.granted.as_mut() {
            // The sender stays in the queue until it grants or this wait removes it.
            let _ = granted.await;
        }
        wait.granted = None;
        Slot(Arc::clone(self))
    }

    fn release(&self) {
        let mut state = self.state();
        let now = Instant::now();
        loop {
            let newest_first = state
                .occupied_since
                .is_some_and(|since| now.saturating_duration_since(since) > STANDING_AFTER);
            let next = if newest_first {
                state.waiters.pop_last()
            } else {
                state.waiters.pop_first()
            };
            if state.waiters.is_empty() {
                state.occupied_since = None;
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
    id: u64,
    granted: Option<oneshot::Receiver<()>>,
}

impl Drop for Wait<'_> {
    fn drop(&mut self) {
        if self.granted.is_none() {
            return;
        }
        let queued = {
            let mut state = self.slots.state();
            let queued = state.waiters.remove(&self.id).is_some();
            if state.waiters.is_empty() {
                state.occupied_since = None;
            }
            queued
        };
        if !queued {
            // Granted between the last poll and this drop: pass the slot on.
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
        let second = poll(pin!(slots.take()));
        assert!(first.is_some());
        assert!(second.is_ready());
        assert!(slots.try_take().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_young_queue_is_served_in_arrival_order() {
        let slots = Slots::new(1);
        let held = slots.try_take();
        let mut older = pin!(slots.take());
        let mut newer = pin!(slots.take());
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
        let mut older = pin!(slots.take());
        assert!(poll(older.as_mut()).is_pending());
        tokio::time::advance(STANDING_AFTER + Duration::from_millis(1)).await;
        let mut newer = pin!(slots.take());
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
            let mut gone = pin!(slots.take());
            assert!(poll(gone.as_mut()).is_pending());
            tokio::time::advance(STANDING_AFTER * 2).await;
        }
        let mut older = pin!(slots.take());
        let mut newer = pin!(slots.take());
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
            let mut abandoned = pin!(slots.take());
            assert!(poll(abandoned.as_mut()).is_pending());
        }

        drop(held);

        assert!(slots.try_take().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_slot_granted_to_a_wait_dropped_unpolled_is_passed_on() {
        let slots = Slots::new(1);
        let held = slots.try_take();
        let mut next = pin!(slots.take());
        {
            let mut granted_then_dropped = pin!(slots.take());
            assert!(poll(granted_then_dropped.as_mut()).is_pending());
            assert!(poll(next.as_mut()).is_pending());
            drop(held);
        }

        assert!(poll(next.as_mut()).is_ready());
    }
}
