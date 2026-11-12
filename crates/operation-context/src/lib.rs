//! Fixed monotonic deadlines and cancellation lineage shared by operation owners.

use std::future::pending;
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Origin plus duration retains finite budgets too large for Instant addition.
#[derive(Clone, Copy, Debug)]
pub struct Deadline {
    origin: Instant,
    budget: Duration,
}

impl Deadline {
    #[must_use]
    pub const fn new(origin: Instant, budget: Duration) -> Self {
        Self { origin, budget }
    }

    /// A deadline that expires at `cutoff`, measured from now; a cutoff already past is expired.
    #[must_use]
    pub fn at(cutoff: Instant) -> Self {
        let now = Instant::now();
        Self::new(cutoff.min(now), cutoff.saturating_duration_since(now))
    }

    #[must_use]
    pub fn remaining_at(self, now: Instant) -> Duration {
        self.budget
            .saturating_add(self.origin.saturating_duration_since(now))
            .saturating_sub(now.saturating_duration_since(self.origin))
    }

    #[must_use]
    pub fn remaining(self) -> Duration {
        self.remaining_at(Instant::now())
    }

    #[must_use]
    pub fn expired(self) -> bool {
        self.remaining().is_zero()
    }

    /// Exact cutoff when representable; absence never means an unbounded budget.
    #[must_use]
    pub fn instant(self) -> Option<Instant> {
        self.origin.checked_add(self.budget)
    }

    /// Whichever of the two deadlines has less time remaining now.
    #[must_use]
    pub fn earlier(self, other: Self) -> Self {
        let now = Instant::now();
        if self.remaining_at(now) <= other.remaining_at(now) {
            self
        } else {
            other
        }
    }

    pub async fn wait(self) {
        // The check comes before any timer: timers have millisecond
        // resolution, so `sleep_until` alone leaves a zero budget pending on
        // its first poll.
        while !self.expired() {
            tokio::time::sleep(self.remaining().min(Duration::from_hours(24))).await;
        }
    }
}

/// Why an unfinished logical operation must stop; adapters own its error mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stopped {
    Deadline,
    Cancelled,
}

/// Clones share one scope. Children inherit cancellation without cancelling peers.
#[derive(Clone, Debug)]
pub struct OperationContext {
    deadline: Option<Deadline>,
    cancellation: CancellationToken,
}

impl OperationContext {
    /// A context with the given optional deadline and cancellation token; `None` means no deadline.
    #[must_use]
    pub const fn new(deadline: Option<Deadline>, cancellation: CancellationToken) -> Self {
        Self {
            deadline,
            cancellation,
        }
    }

    /// Explicitly unbounded parent; each dependency applies its own finite ceiling.
    #[must_use]
    pub fn unbounded() -> Self {
        Self::new(None, CancellationToken::new())
    }

    #[must_use]
    pub fn from_deadline(deadline: Deadline) -> Self {
        Self::new(Some(deadline), CancellationToken::new())
    }

    #[must_use]
    pub fn with_timeout(budget: Duration) -> Self {
        Self::from_deadline(Deadline::new(Instant::now(), budget))
    }

    #[must_use]
    pub const fn deadline(&self) -> Option<Deadline> {
        self.deadline
    }

    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline.map(Deadline::remaining)
    }

    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Derive once at operation admission; all later stages retain this child.
    #[must_use]
    pub fn child(&self, ceiling: Duration) -> Self {
        let local = Deadline::new(Instant::now(), ceiling);
        Self::new(
            Some(self.deadline.map_or(local, |parent| parent.earlier(local))),
            self.cancellation.child_token(),
        )
    }

    /// A separate cancellation scope under the same existing lifetime.
    #[must_use]
    pub fn child_context(&self) -> Self {
        Self::new(self.deadline, self.cancellation.child_token())
    }

    /// Why this scope must stop, if it must. An expired deadline is reported before cancellation.
    #[must_use]
    pub fn stopped(&self) -> Option<Stopped> {
        if self.deadline.is_some_and(Deadline::expired) {
            Some(Stopped::Deadline)
        } else if self.cancellation.is_cancelled() {
            Some(Stopped::Cancelled)
        } else {
            None
        }
    }

    /// # Errors
    /// Returns the existing deadline/cancellation reason when this scope stopped.
    pub fn check(&self) -> Result<(), Stopped> {
        self.stopped().map_or(Ok(()), Err)
    }

    /// Wait without spawning work. Deadline takes precedence when both are ready.
    pub async fn wait_stopped(&self) -> Stopped {
        let deadline = async {
            match self.deadline {
                Some(deadline) => deadline.wait().await,
                None => pending().await,
            }
        };
        tokio::select! {
            biased;
            () = deadline => Stopped::Deadline,
            () = self.cancellation.cancelled() => Stopped::Cancelled,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future as _, poll_fn};
    use std::pin::pin;
    use std::task::Poll;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn child_spends_the_original_parent_budget() {
        let parent = OperationContext::with_timeout(Duration::from_secs(10));
        tokio::time::advance(Duration::from_secs(7)).await;
        let child = parent.child(Duration::from_secs(5));
        assert_eq!(child.remaining(), Some(Duration::from_secs(3)));
        let shorter = parent.child(Duration::from_secs(1));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(shorter.check(), Err(Stopped::Deadline));
        assert_eq!(child.remaining(), Some(Duration::from_secs(2)));
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(child.wait_stopped().await, Stopped::Deadline);
    }

    #[tokio::test]
    async fn a_spent_budget_is_reported_on_the_first_poll() {
        let context = OperationContext::with_timeout(Duration::ZERO);
        let mut stopped = pin!(context.wait_stopped());
        let first = poll_fn(|cx| Poll::Ready(stopped.as_mut().poll(cx))).await;
        assert_eq!(first, Poll::Ready(Stopped::Deadline));
    }

    #[tokio::test(start_paused = true)]
    async fn future_origin_retains_time_before_origin() {
        let origin = Instant::now() + Duration::from_secs(10);
        let deadline = Deadline::new(origin, Duration::from_secs(5));
        assert_eq!(deadline.remaining(), Duration::from_secs(15));
        assert!(!deadline.expired());
        assert_eq!(deadline.instant(), Some(origin + Duration::from_secs(5)));

        let zero = Deadline::new(origin, Duration::ZERO);
        assert_eq!(zero.remaining(), Duration::from_secs(10));
        assert!(!zero.expired());

        let sooner = Deadline::new(Instant::now(), Duration::from_secs(5));
        assert_eq!(
            deadline.earlier(sooner).instant(),
            Some(Instant::now() + Duration::from_secs(5))
        );

        let parent = OperationContext::from_deadline(Deadline::new(origin, Duration::from_secs(1)));
        let child = parent.child(Duration::from_secs(5));
        assert_eq!(child.remaining(), Some(Duration::from_secs(5)));
        assert_eq!(
            child.deadline().and_then(Deadline::instant),
            Some(Instant::now() + Duration::from_secs(5))
        );
    }

    #[tokio::test]
    async fn cancellation_flows_down_without_stopping_parent_or_sibling() {
        let parent = OperationContext::unbounded();
        let child = parent.child_context();
        let sibling = parent.child_context();
        child.cancel();
        assert_eq!(child.wait_stopped().await, Stopped::Cancelled);
        assert_eq!(parent.check(), Ok(()));
        assert_eq!(sibling.check(), Ok(()));
        parent.cancel();
        assert_eq!(sibling.wait_stopped().await, Stopped::Cancelled);
    }

    #[tokio::test(start_paused = true)]
    async fn expired_deadline_is_reported_before_cancellation() {
        let context = OperationContext::with_timeout(Duration::from_secs(1));
        context.cancel();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(context.check(), Err(Stopped::Deadline));
        assert_eq!(context.wait_stopped().await, Stopped::Deadline);
    }

    #[tokio::test(start_paused = true)]
    async fn huge_finite_deadline_can_wait_and_clamp_without_overflow() {
        let deadline = Deadline::new(Instant::now(), Duration::MAX);
        let parent = OperationContext::from_deadline(deadline);
        assert!(!deadline.expired());
        assert_eq!(
            parent.child(Duration::from_secs(1)).remaining(),
            Some(Duration::from_secs(1))
        );
        let mut wait = pin!(parent.wait_stopped());
        assert!(
            poll_fn(|cx| Poll::Ready(wait.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        parent.cancel();
        assert_eq!(wait.await, Stopped::Cancelled);
    }
}
