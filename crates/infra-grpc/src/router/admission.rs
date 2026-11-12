use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use service_failure::{AT_CAPACITY_DETAIL, AT_CAPACITY_RETRY_AFTER};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{REJECT_DRAIN, opening_context, reject_with_drain};
use crate::Failure;
use crate::call::Handoff;

#[derive(Clone)]
pub(super) struct AdmissionLimit {
    permits: Arc<Semaphore>,
    max_wait: Duration,
}

impl AdmissionLimit {
    pub(super) fn new(limit: NonZeroU32, max_wait: Duration) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(limit.get() as usize)),
            max_wait,
        }
    }
}

/// Admission before authentication. Only the opening future owns this permit;
/// response headers release it independently of call custody.
pub(super) async fn admit_opening(
    State(limit): State<AdmissionLimit>,
    request: Request,
    next: Next,
) -> Response {
    let patience = patience(&request, limit.max_wait);
    let Some(_permit) = acquire(limit.permits, patience).await else {
        return reject_at_capacity(request).await;
    };
    next.run(request).await
}

/// Authenticated business admission. The future owns the permit until headers,
/// then transfers it to the terminal response owner.
pub(super) async fn admit_business_call(
    State(limit): State<AdmissionLimit>,
    request: Request,
    next: Next,
) -> Response {
    let patience = patience(&request, limit.max_wait);
    let Some(permit) = acquire(limit.permits, patience).await else {
        return reject_at_capacity(request).await;
    };
    let mut response = next.run(request).await;
    response.extensions_mut().insert(Handoff::new(permit));
    response
}

/// Leave time within the opening budget to drain and return the refusal.
fn patience(request: &Request, max_wait: Duration) -> Duration {
    opening_context(request)
        .and_then(operation_context::OperationContext::remaining)
        .unwrap_or_default()
        .saturating_sub(REJECT_DRAIN)
        .min(max_wait)
}

/// A call that waits is served once capacity frees within its budget.
/// Refusing it earlier would refuse work the process could still do.
async fn acquire(permits: Arc<Semaphore>, patience: Duration) -> Option<OwnedSemaphorePermit> {
    if let Ok(permit) = Arc::clone(&permits).try_acquire_owned() {
        return Some(permit);
    }
    if patience.is_zero() {
        return None;
    }

    let waiting = crate::observe::AdmissionWaiting::start();
    let started = tokio::time::Instant::now();
    let permit = tokio::time::timeout(patience, permits.acquire_owned())
        .await
        .ok()
        .and_then(Result::ok)
        // Tokio's timeout can return a ready permit after the wait budget.
        .filter(|_| started.elapsed() < patience);
    drop(waiting);
    permit
}

#[derive(Clone)]
pub(crate) struct Shed;

/// The refusal: `RESOURCE_EXHAUSTED`, so a client that retries `UNAVAILABLE`
/// without a delay adds no load, carrying the same catalog identity as HTTP's
/// shed answer. `RetryInfo` gives a client that honors it the delay.
async fn reject_at_capacity(request: Request) -> Response {
    let status = Failure::new(service_failure::Code::ServiceUnavailable)
        .retry_after(AT_CAPACITY_RETRY_AFTER)
        .into_status_as(tonic::Code::ResourceExhausted, AT_CAPACITY_DETAIL);
    // Leave part of the remaining budget to return the status even if
    // the upload never finishes.
    let drain = opening_context(&request)
        .and_then(operation_context::OperationContext::remaining)
        .map_or(REJECT_DRAIN, |remaining| REJECT_DRAIN.min(remaining / 2));
    let mut response = reject_with_drain(request, status, drain).await;
    response.extensions_mut().insert(Shed);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_permit_freed_after_the_admission_wait_is_refused() {
        use std::task::{Context, Waker};

        let permits = Arc::new(Semaphore::new(1));
        let holder = permits.try_acquire().unwrap();
        let patience = Duration::from_secs(1);
        let mut waiting = std::pin::pin!(acquire(Arc::clone(&permits), patience));
        let cx = &mut Context::from_waker(Waker::noop());
        assert!(waiting.as_mut().poll(cx).is_pending());

        tokio::time::advance(patience + Duration::from_millis(1)).await;
        drop(holder);

        assert!(waiting.await.is_none());
        assert_eq!(permits.available_permits(), 1);
    }
}
