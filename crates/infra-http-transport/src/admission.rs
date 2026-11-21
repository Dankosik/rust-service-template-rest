use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore, SemaphorePermit};
use tokio::time::Instant;

#[must_use]
#[derive(Debug)]
pub enum Admission<P> {
    Immediate(P),
    Waited { permit: P, waited: Duration },
    Shed { waited: Duration },
}

pub async fn acquire_within<G>(
    permits: &Arc<Semaphore>,
    patience: Duration,
    on_wait: impl FnOnce() -> G,
) -> Admission<OwnedSemaphorePermit> {
    if let Ok(permit) = Arc::clone(permits).try_acquire_owned() {
        return Admission::Immediate(permit);
    }
    wait_within(patience, on_wait, || Arc::clone(permits).acquire_owned()).await
}

pub async fn acquire_borrowed_within<G>(
    permits: &Semaphore,
    patience: Duration,
    on_wait: impl FnOnce() -> G,
) -> Admission<SemaphorePermit<'_>> {
    if let Ok(permit) = permits.try_acquire() {
        return Admission::Immediate(permit);
    }
    wait_within(patience, on_wait, || permits.acquire()).await
}

async fn wait_within<P, G, F: Future<Output = Result<P, AcquireError>>>(
    patience: Duration,
    on_wait: impl FnOnce() -> G,
    acquire: impl FnOnce() -> F,
) -> Admission<P> {
    if patience.is_zero() {
        return Admission::Shed {
            waited: Duration::ZERO,
        };
    }
    let waiting = on_wait();
    let started = Instant::now();
    let acquire_result = tokio::time::timeout(patience, acquire()).await;
    let waited = started.elapsed();
    drop(waiting);
    match acquire_result {
        Ok(Ok(permit)) if waited < patience => Admission::Waited { permit, waited },
        _ => Admission::Shed { waited },
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

        std::assert_matches!(waiting.await, Admission::Shed { .. });
        assert_eq!(permits.available_permits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_free_permit_is_taken_without_waiting_and_zero_patience_refuses_at_once() {
        let permits = Arc::new(Semaphore::new(1));
        let mut waits = 0;
        let Admission::Immediate(permit) =
            acquire_within(&permits, Duration::ZERO, || waits += 1).await
        else {
            panic!("free permit should be granted immediately");
        };

        std::assert_matches!(
            acquire_within(&permits, Duration::ZERO, || waits += 1).await,
            Admission::Shed {
                waited: Duration::ZERO,
            }
        );
        assert_eq!(waits, 0);
        drop(permit);
    }

    #[tokio::test(start_paused = true)]
    async fn a_permit_freed_before_patience_expires_reports_its_wait() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let permits = Semaphore::new(1);
        let waits = AtomicUsize::new(0);
        let active_waits = AtomicUsize::new(0);
        let observe = || {
            waits.fetch_add(1, Ordering::SeqCst);
            active_waits.fetch_add(1, Ordering::SeqCst);
            std::mem::DropGuard::new((), |()| {
                active_waits.fetch_sub(1, Ordering::SeqCst);
            })
        };

        let Admission::Immediate(holder) =
            acquire_borrowed_within(&permits, Duration::ZERO, observe).await
        else {
            panic!("free borrowed permit should be granted immediately");
        };
        std::assert_matches!(
            acquire_borrowed_within(&permits, Duration::ZERO, observe).await,
            Admission::Shed {
                waited: Duration::ZERO,
            }
        );
        assert_eq!(waits.load(Ordering::SeqCst), 0);

        let patience = Duration::from_secs(1);
        let wait = Duration::from_millis(250);
        let mut waiting = std::pin::pin!(acquire_borrowed_within(&permits, patience, observe));
        let cx = &mut Context::from_waker(Waker::noop());
        assert!(waiting.as_mut().poll(cx).is_pending());
        assert_eq!(waits.load(Ordering::SeqCst), 1);
        assert_eq!(active_waits.load(Ordering::SeqCst), 1);

        tokio::time::advance(wait).await;
        drop(holder);

        let Admission::Waited { permit, waited } = waiting.await else {
            panic!("permit freed within patience should be admitted after waiting");
        };
        assert_eq!(waited, wait);
        assert_eq!(active_waits.load(Ordering::SeqCst), 0);
        drop(permit);
        assert_eq!(permits.available_permits(), 1);
    }
}
