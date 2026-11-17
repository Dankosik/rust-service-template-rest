use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use service_failure::{AT_CAPACITY_DETAIL, AT_CAPACITY_RETRY_AFTER};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{REJECT_DRAIN, opening_context, reject};
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

#[derive(Clone, Copy)]
struct OpeningWait(Duration);

pub(super) async fn admit_opening(
    State(limit): State<AdmissionLimit>,
    mut request: Request,
    next: Next,
) -> Response {
    let patience = patience(&request, limit.max_wait);
    let Some((_permit, waited)) = acquire(&limit.permits, patience).await else {
        return reject_at_capacity(request).await;
    };
    if !waited.is_zero() {
        request.extensions_mut().insert(OpeningWait(waited));
    }
    next.run(request).await
}

pub(super) async fn admit_business_call(
    State(limit): State<AdmissionLimit>,
    request: Request,
    next: Next,
) -> Response {
    let waited = request
        .extensions()
        .get::<OpeningWait>()
        .map_or(Duration::ZERO, |wait| wait.0);
    let patience = patience(&request, limit.max_wait.saturating_sub(waited));
    let Some((permit, _)) = acquire(&limit.permits, patience).await else {
        return reject_at_capacity(request).await;
    };
    let mut response = next.run(request).await;
    response.extensions_mut().insert(Handoff::new(permit));
    response
}

fn patience(request: &Request, max_wait: Duration) -> Duration {
    opening_context(request)
        .and_then(operation_context::OperationContext::remaining)
        .unwrap_or_default()
        .saturating_sub(REJECT_DRAIN)
        .min(max_wait)
}

async fn acquire(
    permits: &Arc<Semaphore>,
    patience: Duration,
) -> Option<(OwnedSemaphorePermit, Duration)> {
    infra_http::acquire_within(permits, patience, crate::observe::admission_waiting)
        .await
        .ok()
        .map(|(permit, waited)| (permit, waited.unwrap_or_default()))
}

#[derive(Clone)]
pub(crate) struct Shed;

async fn reject_at_capacity(request: Request) -> Response {
    let status = Failure::new(service_failure::Code::ServiceUnavailable)
        .retry_after(AT_CAPACITY_RETRY_AFTER)
        .into_status_as(tonic::Code::ResourceExhausted, AT_CAPACITY_DETAIL);
    let mut response = reject(request, status).await;
    response.extensions_mut().insert(Shed);
    response
}
