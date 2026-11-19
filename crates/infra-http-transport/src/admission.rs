use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore, SemaphorePermit};
use tokio::time::Instant;

pub async fn acquire_within<G>(
    permits: &Arc<Semaphore>,
    patience: Duration,
    on_wait: impl FnOnce() -> G,
) -> Result<(OwnedSemaphorePermit, Option<Duration>), Duration> {
    if let Ok(permit) = Arc::clone(permits).try_acquire_owned() {
        return Ok((permit, None));
    }
    wait_within(patience, on_wait, || Arc::clone(permits).acquire_owned()).await
}

pub async fn acquire_borrowed_within(
    permits: &Semaphore,
    patience: Duration,
) -> Result<(SemaphorePermit<'_>, Option<Duration>), Duration> {
    if let Ok(permit) = permits.try_acquire() {
        return Ok((permit, None));
    }
    wait_within(patience, || (), || permits.acquire()).await
}

async fn wait_within<P, G, F: Future<Output = Result<P, AcquireError>>>(
    patience: Duration,
    on_wait: impl FnOnce() -> G,
    acquire: impl FnOnce() -> F,
) -> Result<(P, Option<Duration>), Duration> {
    if patience.is_zero() {
        return Err(Duration::ZERO);
    }
    let waiting = on_wait();
    let started = Instant::now();
    let acquire_result = tokio::time::timeout(patience, acquire()).await;
    let waited = started.elapsed();
    drop(waiting);
    match acquire_result {
        Ok(Ok(permit)) if waited < patience => Ok((permit, Some(waited))),
        _ => Err(waited),
    }
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Waker};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_permit_freed_after_the_wait_is_refused() {
        let permits = Arc::new(Semaphore::new(1));
        let holder = permits.try_acquire().unwrap();
        let patience = Duration::from_secs(1);
        let mut waiting = std::pin::pin!(acquire_within(&permits, patience, || ()));
        let cx = &mut Context::from_waker(Waker::noop());
        assert!(waiting.as_mut().poll(cx).is_pending());

        tokio::time::advance(patience + Duration::from_millis(1)).await;
        drop(holder);

        assert!(waiting.await.is_err());
        assert_eq!(permits.available_permits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_free_permit_is_taken_without_waiting_and_zero_patience_refuses_at_once() {
        let permits = Arc::new(Semaphore::new(1));
        let mut waits = 0;
        let (permit, waited) = acquire_within(&permits, Duration::ZERO, || waits += 1)
            .await
            .unwrap();
        assert_eq!(waited, None);

        let refused = acquire_within(&permits, Duration::ZERO, || waits += 1).await;
        assert_eq!(refused.err(), Some(Duration::ZERO));
        assert_eq!(waits, 0);
        drop(permit);
    }
}
