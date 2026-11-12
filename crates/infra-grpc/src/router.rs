use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use bytes::Bytes;
use http_body_util::BodyExt as _;
use service_failure::{AT_CAPACITY_DETAIL, AT_CAPACITY_RETRY_AFTER};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tonic::server::NamedService as _;

use crate::call::{Handoff, Side};
use crate::health::HealthServer;
use crate::protocol::grpc_timeout;
use crate::{Error, Failure, Services};

/// Operator limits of the listener and its business calls, from the `grpc`
/// configuration section.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Upper bound for a business call's time to response headers,
    /// authentication included. A caller's shorter `grpc-timeout` wins.
    pub request_timeout: Duration,
    /// Independent bounds on pre-header openings and admitted business calls
    /// through terminal status, shared by router clones. A call over a bound
    /// waits for a permit, up to [`Limits::max_admission_wait`] and within its
    /// opening budget, and is refused only when none frees. `None` disables
    /// both bounds.
    pub max_in_flight: Option<NonZeroU32>,
    /// Longest a call over `max_in_flight` waits for a permit. Zero refuses it
    /// at once. The call's own remaining budget less the refusal reserve also
    /// bounds the wait.
    pub max_admission_wait: Duration,
    /// Accepted connections at once; `None` accepts without a bound.
    pub max_connections: Option<NonZeroU32>,
    /// Age after which a connection gets GOAWAY; `None` sets no age.
    pub max_connection_age: Option<Duration>,
}

/// Serves registered services and standard health behind the gRPC middleware
/// chain. Health is public. A business call passes, outside in: observation,
/// the opening budget, admission of its opening, the authentication and
/// caller admission this build compiles in, and the business concurrency
/// limit.
///
/// # Errors
///
/// Returns [`Error::InvalidFileDescriptorSet`] when reflection was added and
/// cannot serve the described sets and [`Error::DuplicateService`] when
/// reflection collides with a registered service. Returns
/// [`Error::ConflictingServiceDescription`] when the descriptions of a
/// built-in service disagree on its methods.
// template:begin service-auth:grpc-router-caller-rule-errors-doc
/// Returns [`Error::MissingCallerRule`] when a caller table is incomplete.
// template:end service-auth:grpc-router-caller-rule-errors-doc
// template:begin authn:grpc-router-unauthenticated-doc
/// Returns [`Error::UnauthenticatedBusinessCalls`] when `verifier` is `None`
/// and no service-token verifier replaces it, and
/// [`Error::ScopesWithServiceTokens`] when a scope requirement is declared
/// while service tokens replace bearer authentication.
// template:end authn:grpc-router-unauthenticated-doc
pub fn router(
    mut services: Services,
    readiness: ::health::ReadinessReader,
    // template:begin authn:grpc-router-verifier
    verifier: Option<infra_bearerauthn::Verifier>,
    // template:end authn:grpc-router-verifier
    limits: Limits,
) -> Result<axum::Router, Error> {
    if services.reflection {
        services.attach_reflection()?;
    }
    services.learn(tonic_health::pb::FILE_DESCRIPTOR_SET)?;
    // The placeholder for a build without the service-auth region, which
    // replaces it below; `allow`, not `expect`, because only some profile
    // combinations leave it unread.
    #[allow(
        unused_variables,
        reason = "bearer authentication alone reads it; service tokens replace it"
    )]
    let service_tokens = false;
    // template:begin service-auth:grpc-router-caller-rules
    let admission = Arc::new(services.caller_admission()?);
    #[allow(
        unused_variables,
        reason = "only the authn region reads it, to skip bearer authentication"
    )]
    let service_tokens = admission.verifies();
    // template:end service-auth:grpc-router-caller-rules
    let series = crate::observe::Series::server(services.method_paths());
    let mut business = services.routes.routes().into_axum_router();
    if let Some(limit) = limits.max_in_flight {
        crate::observe::register_shed();
        let permits = Arc::new(Semaphore::new(limit.get() as usize));
        business = business.layer(axum::middleware::from_fn_with_state(
            AdmissionLimit {
                permits,
                max_wait: limits.max_admission_wait,
            },
            admit_business_call,
        ));
    }
    // template:begin service-auth:grpc-router-admit-callers
    // Inside bearer authentication. Without a service-token verifier,
    // admission only removes `Authorization` before the handler.
    let business = business.layer(axum::middleware::from_fn(move |request, next| {
        let admission = Arc::clone(&admission);
        async move { crate::service_auth::admit_caller(admission, request, next).await }
    }));
    // template:end service-auth:grpc-router-admit-callers
    // template:begin authn:grpc-router-authenticate
    let scopes = Arc::new(std::mem::take(&mut services.scopes));
    let business = if service_tokens {
        // A service token names a caller and carries no scope, so a declared
        // requirement could only be dropped.
        if !scopes.is_empty() {
            return Err(Error::ScopesWithServiceTokens);
        }
        business
    } else {
        match verifier {
            Some(verifier) => business.layer(axum::middleware::from_fn(move |request, next| {
                let verifier = verifier.clone();
                let scopes = Arc::clone(&scopes);
                async move { crate::authn::authenticate(verifier, scopes, request, next).await }
            })),
            None => return Err(Error::UnauthenticatedBusinessCalls),
        }
    };
    // template:end authn:grpc-router-authenticate
    let business = if let Some(limit) = limits.max_in_flight {
        let openings = Arc::new(Semaphore::new(limit.get() as usize));
        business.layer(axum::middleware::from_fn_with_state(
            AdmissionLimit {
                permits: openings,
                max_wait: limits.max_admission_wait,
            },
            admit_opening,
        ))
    } else {
        business
    };
    let business = business.layer(axum::middleware::from_fn_with_state(
        Some(limits.request_timeout),
        opening,
    ));
    Ok(with_health(business, readiness, &services.names).layer(
        axum::middleware::from_fn_with_state(series, crate::observe::observe),
    ))
}

/// Listener options for the gRPC port: the operator's connection bounds and
/// the fixed head limits.
#[must_use]
pub fn server_options(limits: Limits) -> infra_http::ServerOptions {
    infra_http::ServerOptions {
        header_read_timeout: Duration::from_secs(5),
        max_header_bytes: 16 * 1024,
        max_connections: limits.max_connections,
        max_connection_age: limits.max_connection_age,
    }
}

/// Adds health the way tonic `Routes` adds a service: one `/{NAME}/{*rest}` route.
fn with_health(
    router: axum::Router,
    readiness: ::health::ReadinessReader,
    names: &BTreeSet<&'static str>,
) -> axum::Router {
    let health = HealthServer::new(crate::health::Adapter::new(readiness, names));
    #[expect(
        clippy::disallowed_methods,
        reason = "gRPC health registration belongs to the separate tonic listener"
    )]
    let health =
        axum::Router::new().route_service(&format!("/{}/{{*rest}}", HealthServer::NAME), health);
    router.merge(health.layer(axum::middleware::from_fn_with_state(
        None::<Duration>,
        opening,
    )))
}

/// Caller-owned lifetime for work continuing after the response headers: the
/// caller's own `grpc-timeout` and the call's cancellation, without the
/// server's opening cap. A handler reads it from the request extensions. It
/// is a separate type so it never collides with the opening
/// [`OperationContext`](operation_context::OperationContext) extension.
#[derive(Clone, Debug)]
pub struct ResponseContext(operation_context::OperationContext);

impl ResponseContext {
    #[must_use]
    pub const fn operation(&self) -> &operation_context::OperationContext {
        &self.0
    }

    pub(crate) fn into_operation(self) -> operation_context::OperationContext {
        self.0
    }
}

/// Bounds opening from before authentication while preserving a separate
/// caller lifetime. Once the opening context stops, that stop wins over any
/// result produced after it, so every later layer checks it again.
async fn opening(
    State(cap): State<Option<Duration>>,
    mut request: Request,
    next: Next,
) -> Response {
    use operation_context::{Deadline, OperationContext};
    let origin = tokio::time::Instant::now();
    let caller = grpc_timeout(request.headers());
    let budget = match (cap, caller) {
        (Some(cap), Some(caller)) => cap.min(caller),
        (Some(budget), None) | (None, Some(budget)) => budget,
        (None, None) => return next.run(request).await,
    };
    let cancellation = tokio_util::sync::CancellationToken::new();
    let opening_context =
        OperationContext::new(Some(Deadline::new(origin, budget)), cancellation.clone());
    let response_context = ResponseContext(OperationContext::new(
        caller.map(|budget| Deadline::new(origin, budget)),
        cancellation.clone(),
    ));
    let guard = cancellation.drop_guard();
    request.extensions_mut().insert(opening_context.clone());
    request.extensions_mut().insert(response_context.clone());
    let mut response = tokio::select! {
        biased;
        reason = opening_context.wait_stopped() => return Side::Server.stopped(reason).into_http(),
        response = next.run(request) => response,
    };
    if let Some(reason) = opening_context.stopped() {
        return Side::Server.stopped(reason).into_http();
    }
    response.extensions_mut().insert(response_context);
    response.extensions_mut().insert(Handoff::new(guard));
    response
}

/// The opening context the [`opening`] layer puts on every business call
/// before any layer inside it runs; `None` only if that layer is missing.
pub(crate) fn opening_context(request: &Request) -> Option<&operation_context::OperationContext> {
    request
        .extensions()
        .get::<operation_context::OperationContext>()
}

/// A concurrency bound and how long a call over it may wait.
#[derive(Clone)]
struct AdmissionLimit {
    permits: Arc<Semaphore>,
    max_wait: Duration,
}

/// Admission before authentication. Only the opening future owns this permit;
/// response headers release it independently of call custody.
async fn admit_opening(
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
async fn admit_business_call(
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

/// A permit, waited for up to `patience`. A call that waits is served once
/// capacity frees; refusing it earlier would refuse work the process could
/// still do.
async fn acquire(permits: Arc<Semaphore>, patience: Duration) -> Option<OwnedSemaphorePermit> {
    if let Ok(permit) = Arc::clone(&permits).try_acquire_owned() {
        return Some(permit);
    }
    if !patience.is_zero() {
        let waiting = crate::observe::AdmissionWaiting::start();
        let started = tokio::time::Instant::now();
        let permit = tokio::time::timeout(patience, permits.acquire_owned())
            .await
            .ok()
            .and_then(Result::ok)
            // Tokio's timeout can return a ready permit after the wait budget.
            .filter(|_| started.elapsed() < patience);
        drop(waiting);
        if permit.is_some() {
            return permit;
        }
    }
    None
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

/// Longest wait for the rest of a rejected call's request body.
const REJECT_DRAIN: Duration = Duration::from_millis(100);
/// Most request bytes read from a rejected call before giving up.
const REJECT_DRAIN_BYTES: usize = 64 * 1024;

/// Answers a call rejected before its handler, after reading the rest of its
/// request body within a small bound. A caller sends its request DATA after
/// the headers, so an immediate answer closes the stream while that DATA is
/// in flight; h2 answers the late DATA with a stream reset, and after 1024
/// such resets on one connection closes it, with every other call on it.
#[allow(
    dead_code,
    reason = "only the optional bearer and service authentication profiles use the full drain"
)]
pub(crate) async fn reject(request: Request, status: tonic::Status) -> Response {
    reject_with_drain(request, status, REJECT_DRAIN).await
}

async fn reject_with_drain(request: Request, status: tonic::Status, drain: Duration) -> Response {
    let mut body = request.into_body();
    let _ = tokio::time::timeout(drain, async {
        let mut read = 0;
        while let Some(Ok(frame)) = body.frame().await {
            tokio::task::consume_budget().await;
            read += frame.data_ref().map_or(0, Bytes::len);
            if read > REJECT_DRAIN_BYTES {
                break;
            }
        }
    })
    .await;
    status.into_http()
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

    #[tokio::test]
    async fn a_rejected_call_reads_at_most_one_frame_past_64_kib() {
        use futures_util::StreamExt as _;
        use std::convert::Infallible;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let frames = Arc::new(AtomicUsize::new(0));
        let polled = Arc::clone(&frames);
        // Ten 16 KiB frames: the fifth takes the read past 64 KiB.
        let body = futures_util::stream::iter(0..10).map(move |_| {
            polled.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(Bytes::from(vec![0_u8; 16 * 1024]))
        });
        let request = Request::new(axum::body::Body::from_stream(body));
        let response = reject(request, tonic::Status::unavailable("closed")).await;
        assert_eq!(response.headers()["grpc-status"], "14");
        assert_eq!(frames.load(Ordering::SeqCst), 5);
    }
}
