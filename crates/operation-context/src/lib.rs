use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const MAX_SLEEP_SLICE: Duration = Duration::from_hours(24);

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

    #[must_use]
    pub fn after(budget: Duration) -> Self {
        Self::new(Instant::now(), budget)
    }

    #[must_use]
    pub fn at(cutoff: Instant) -> Self {
        let now = Instant::now();
        Self::new(cutoff.min(now), cutoff.saturating_duration_since(now))
    }

    #[must_use]
    pub fn remaining_at(self, now: Instant) -> Duration {
        if now < self.origin {
            self.budget.saturating_add(self.origin - now)
        } else {
            self.budget.saturating_sub(now - self.origin)
        }
    }

    #[must_use]
    pub fn remaining(self) -> Duration {
        self.remaining_at(Instant::now())
    }

    #[must_use]
    pub fn expired(self) -> bool {
        self.remaining().is_zero()
    }

    #[must_use]
    pub fn instant(self) -> Option<Instant> {
        self.origin.checked_add(self.budget)
    }

    #[must_use]
    pub fn earlier(self, other: Self) -> Self {
        let now = Instant::now();
        std::cmp::min_by_key(self, other, |deadline| deadline.remaining_at(now))
    }

    pub async fn wait(self) {
        loop {
            let remaining = self.remaining();
            if remaining.is_zero() {
                return;
            }
            tokio::time::sleep(remaining.min(MAX_SLEEP_SLICE)).await;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stopped {
    Deadline,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundedStop {
    Parent(Stopped),
    Timeout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeadlineSource {
    Parent,
    Local,
}

#[derive(Clone, Debug)]
pub struct BoundedContext {
    deadline: Deadline,
    source: DeadlineSource,
    cancellation: CancellationToken,
}

impl BoundedContext {
    #[must_use]
    pub const fn deadline(&self) -> Deadline {
        self.deadline
    }

    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.deadline.remaining()
    }

    #[must_use]
    pub fn stopped(&self) -> Option<BoundedStop> {
        if self.deadline.expired() {
            Some(self.deadline_stop())
        } else if self.cancellation.is_cancelled() {
            Some(BoundedStop::Parent(Stopped::Cancelled))
        } else {
            None
        }
    }

    pub fn check(&self) -> Result<(), BoundedStop> {
        self.stopped().map_or(Ok(()), Err)
    }

    pub async fn wait_stopped(&self) -> BoundedStop {
        tokio::select! {
            biased;
            () = self.deadline.wait() => self.deadline_stop(),
            () = self.cancellation.cancelled() => BoundedStop::Parent(Stopped::Cancelled),
        }
    }

    const fn deadline_stop(&self) -> BoundedStop {
        match self.source {
            DeadlineSource::Parent => BoundedStop::Parent(Stopped::Deadline),
            DeadlineSource::Local => BoundedStop::Timeout,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OperationContext {
    deadline: Option<Deadline>,
    cancellation: CancellationToken,
}

impl OperationContext {
    #[must_use]
    pub const fn new(deadline: Option<Deadline>, cancellation: CancellationToken) -> Self {
        Self {
            deadline,
            cancellation,
        }
    }

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
        Self::from_deadline(Deadline::after(budget))
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

    #[must_use]
    pub fn child(&self, ceiling: Duration) -> Self {
        self.child_until(Deadline::after(ceiling))
    }

    #[must_use]
    pub fn bounded(&self, ceiling: Duration) -> BoundedContext {
        let now = Instant::now();
        let (deadline, source) = match self.deadline {
            Some(parent) if parent.remaining_at(now) <= ceiling => (parent, DeadlineSource::Parent),
            _ => (Deadline::new(now, ceiling), DeadlineSource::Local),
        };
        BoundedContext {
            deadline,
            source,
            cancellation: self.cancellation.child_token(),
        }
    }

    #[must_use]
    pub fn effective_deadline(&self, ceiling: Deadline) -> Deadline {
        self.deadline
            .map_or(ceiling, |parent| parent.earlier(ceiling))
    }

    #[must_use]
    pub fn child_until(&self, ceiling: Deadline) -> Self {
        Self::new(
            Some(self.effective_deadline(ceiling)),
            self.cancellation.child_token(),
        )
    }

    #[must_use]
    pub fn child_context(&self) -> Self {
        Self::new(self.deadline, self.cancellation.child_token())
    }

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

    pub fn check(&self) -> Result<(), Stopped> {
        self.stopped().map_or(Ok(()), Err)
    }

    pub async fn wait_stopped(&self) -> Stopped {
        let Some(deadline) = self.deadline else {
            self.cancellation.cancelled().await;
            return Stopped::Cancelled;
        };
        tokio::select! {
            biased;
            () = deadline.wait() => Stopped::Deadline,
            () = self.cancellation.cancelled() => Stopped::Cancelled,
        }
    }

    pub async fn run<F: Future>(&self, future: F) -> Result<F::Output, Stopped> {
        tokio::select! {
            biased;
            reason = self.wait_stopped() => Err(reason),
            output = future => {
                self.check()?;
                Ok(output)
            }
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

        let sooner = Deadline::after(Duration::from_secs(5));
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
        let deadline = Deadline::after(Duration::MAX);
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

    #[tokio::test(start_paused = true)]
    async fn run_stops_on_deadline_or_cancellation_and_rechecks_on_completion() {
        let active = OperationContext::with_timeout(Duration::from_secs(1));
        assert_eq!(active.run(async { 42 }).await, Ok(42));

        let expired = OperationContext::with_timeout(Duration::ZERO);
        let mut ran = false;
        let result = expired
            .run(async {
                ran = true;
            })
            .await;
        assert_eq!(result, Err(Stopped::Deadline));
        assert!(!ran);

        let cancels_during_work = OperationContext::with_timeout(Duration::from_secs(5));
        let result = cancels_during_work
            .run(async {
                cancels_during_work.cancel();
                7
            })
            .await;
        assert_eq!(result, Err(Stopped::Cancelled));

        let pending_timeout = OperationContext::with_timeout(Duration::from_secs(1));
        let mut run = pin!(pending_timeout.run(std::future::pending::<()>()));
        assert!(
            poll_fn(|cx| Poll::Ready(run.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(run.await, Err(Stopped::Deadline));
    }

    #[tokio::test(start_paused = true)]
    async fn bounded_separates_parent_stop_from_local_timeout() {
        let short_parent = OperationContext::with_timeout(Duration::from_secs(1));
        let parent_limited = short_parent.bounded(Duration::from_secs(5));
        assert_eq!(parent_limited.remaining(), Duration::from_secs(1));
        assert_eq!(
            parent_limited.deadline().instant(),
            short_parent.deadline().and_then(Deadline::instant)
        );
        assert_eq!(parent_limited.check(), Ok(()));

        let equal_parent = OperationContext::with_timeout(Duration::from_secs(2));
        let tied = equal_parent.bounded(Duration::from_secs(2));

        let long_parent = OperationContext::with_timeout(Duration::from_secs(10));
        let locally_limited = long_parent.bounded(Duration::from_secs(2));
        assert_eq!(locally_limited.remaining(), Duration::from_secs(2));

        let unbounded_limited = OperationContext::unbounded().bounded(Duration::from_secs(2));

        let cancelling_parent = OperationContext::with_timeout(Duration::from_secs(10));
        let cancelled = cancelling_parent.bounded(Duration::from_secs(5));
        cancelling_parent.cancel();
        assert_eq!(
            cancelled.check(),
            Err(BoundedStop::Parent(Stopped::Cancelled))
        );
        assert_eq!(
            cancelled.wait_stopped().await,
            BoundedStop::Parent(Stopped::Cancelled)
        );

        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(
            parent_limited.stopped(),
            Some(BoundedStop::Parent(Stopped::Deadline))
        );
        assert_eq!(
            parent_limited.wait_stopped().await,
            BoundedStop::Parent(Stopped::Deadline)
        );
        assert_eq!(locally_limited.check(), Ok(()));

        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(tied.check(), Err(BoundedStop::Parent(Stopped::Deadline)));
        assert_eq!(locally_limited.check(), Err(BoundedStop::Timeout));
        assert_eq!(locally_limited.wait_stopped().await, BoundedStop::Timeout);
        assert_eq!(long_parent.check(), Ok(()));
        assert_eq!(unbounded_limited.check(), Err(BoundedStop::Timeout));
    }
}
