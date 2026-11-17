use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use bytes::Bytes;
use futures_util::FutureExt as _;
use http_body_util::BodyExt as _;
use opentelemetry::context::FutureExt as _;
use operation_context::{Deadline, OperationContext};
use service_failure::{AT_CAPACITY_DETAIL, AT_CAPACITY_RETRY_AFTER};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::{CancellationToken, DropGuard};
use tonic::server::NamedService as _;
use tracing::Instrument as _;

use crate::call::{ServerCustody, attach_server, server_stopped};
use crate::health::HealthServer;
use crate::observe::{self, CallType, Methods};
use crate::protocol::grpc_timeout;
use crate::status::internal_status;
use crate::{Error, Failure, Services};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub request_timeout: Duration,
    pub max_in_flight: Option<NonZeroU32>,
    pub max_admission_wait: Duration,
    pub max_connections: Option<NonZeroU32>,
    pub max_connection_age: Option<Duration>,
}

#[derive(Clone, Debug)]
pub struct ResponseContext(OperationContext);

impl ResponseContext {
    #[must_use]
    pub const fn operation(&self) -> &OperationContext {
        &self.0
    }
}

struct BusinessState {
    methods: Arc<Methods>,
    limits: Limits,
    opening_permits: Option<Arc<Semaphore>>,
    call_permits: Option<Arc<Semaphore>>,
    // template:begin authn:grpc-router-bearer-field
    auth: Option<crate::authn::BearerAuth>,
    // template:end authn:grpc-router-bearer-field
    // template:begin service-auth:grpc-router-caller-field
    caller_auth: crate::service_auth::CallerAdmission,
    // template:end service-auth:grpc-router-caller-field
}

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
    #[allow(
        unused_variables,
        reason = "bearer authentication alone reads it; service tokens replace it"
    )]
    let service_tokens = false;
    // template:begin service-auth:grpc-router-caller-rules
    let caller_auth = services.take_caller_admission()?;
    #[allow(
        unused_variables,
        reason = "only the authn region reads it, to skip bearer authentication"
    )]
    let service_tokens = caller_auth.verifies();
    // template:end service-auth:grpc-router-caller-rules
    // template:begin authn:grpc-router-authenticate
    let scopes = std::mem::take(&mut services.scopes);
    let auth = if service_tokens {
        if !scopes.is_empty() {
            return Err(Error::ScopesWithServiceTokens);
        }
        None
    } else {
        match verifier {
            Some(verifier) => Some(crate::authn::BearerAuth::new(verifier, scopes)),
            None => return Err(Error::UnauthenticatedBusinessCalls),
        }
    };
    // template:end authn:grpc-router-authenticate
    if limits.max_in_flight.is_some() {
        observe::register_shed();
    }
    let methods = Arc::new(Methods::new(services.method_paths()));
    let state = Arc::new(BusinessState {
        methods: Arc::clone(&methods),
        limits,
        opening_permits: semaphore(limits.max_in_flight),
        call_permits: semaphore(limits.max_in_flight),
        // template:begin authn:grpc-router-bearer-init
        auth,
        // template:end authn:grpc-router-bearer-init
        // template:begin service-auth:grpc-router-caller-init
        caller_auth,
        // template:end service-auth:grpc-router-caller-init
    });
    let business = services
        .routes
        .routes()
        .into_axum_router()
        .layer(axum::middleware::from_fn_with_state(state, business_call));
    Ok(with_health(business, readiness, services.names, methods))
}

#[must_use]
pub fn server_options(limits: Limits) -> infra_http::ServerOptions {
    infra_http::ServerOptions {
        header_read_timeout: Duration::from_secs(5),
        max_header_bytes: 16 * 1024,
        max_connections: limits.max_connections,
        max_connection_age: limits.max_connection_age,
    }
}

fn semaphore(limit: Option<NonZeroU32>) -> Option<Arc<Semaphore>> {
    limit.map(|count| Arc::new(Semaphore::new(count.get() as usize)))
}

fn with_health(
    router: axum::Router,
    readiness: ::health::ReadinessReader,
    names: BTreeSet<&'static str>,
    methods: Arc<Methods>,
) -> axum::Router {
    let health = HealthServer::new(crate::health::Adapter::new(readiness, names));
    #[expect(
        clippy::disallowed_methods,
        reason = "gRPC health registration belongs to the separate tonic listener"
    )]
    let health =
        axum::Router::new().route_service(&format!("/{}/{{*rest}}", HealthServer::NAME), health);
    router.merge(health.layer(axum::middleware::from_fn_with_state(methods, health_call)))
}

enum OpeningOutcome {
    Admitted {
        response: Response,
        permit: Option<OwnedSemaphorePermit>,
    },
    Rejected(Response),
}

async fn business_call(
    State(state): State<Arc<BusinessState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let listener_drain = infra_http::take_listener_drain_token(request.extensions_mut());
    sanitize_extensions(request.extensions_mut());

    let call = observe::Call::start(&state.methods, &request);
    let fallback = observe::propagated_parent(call.span(), request.headers());
    let origin = tokio::time::Instant::now();
    let caller_timeout = grpc_timeout(request.headers());
    let opening_budget = caller_timeout.map_or(state.limits.request_timeout, |caller| {
        caller.min(state.limits.request_timeout)
    });
    let opening_deadline = Deadline::new(origin, opening_budget);
    let response_deadline = caller_timeout.map(|budget| Deadline::new(origin, budget));

    let is_streaming = matches!(
        call.call_type(),
        Some(CallType::ServerStream | CallType::BidiStream)
    );
    let response_token = match listener_drain.filter(|_| is_streaming) {
        Some(drain) => drain.child_token(),
        None => CancellationToken::new(),
    };
    let opening_context =
        OperationContext::new(Some(opening_deadline), response_token.child_token());
    let response_context = ResponseContext(OperationContext::new(
        response_deadline,
        response_token.clone(),
    ));
    let response_guard = response_token.drop_guard();
    request.extensions_mut().insert(opening_context.clone());
    request.extensions_mut().insert(response_context);

    let outcome = run_in_span(
        async {
            let outcome = tokio::select! {
                biased;
                reason = opening_context.wait_stopped() => {
                    OpeningOutcome::Rejected(server_stopped(reason).into_http())
                }
                outcome = open_business(&state, &opening_context, request, next) => outcome,
            };
            if let Some(reason) = opening_context.stopped() {
                return OpeningOutcome::Rejected(server_stopped(reason).into_http());
            }
            outcome
        },
        call.span(),
        fallback.as_ref(),
        || OpeningOutcome::Rejected(internal_status().into_http()),
    )
    .await;

    finish_call(
        outcome,
        call,
        fallback,
        response_guard,
        response_deadline,
        is_streaming,
    )
}

async fn open_business(
    state: &BusinessState,
    opening_context: &OperationContext,
    request: Request,
    next: Next,
) -> OpeningOutcome {
    let mut remaining_wait = state.limits.max_admission_wait;
    let (_opening_permit, request) = match acquire_permit(
        state.opening_permits.as_ref(),
        &mut remaining_wait,
        opening_context,
        request,
    )
    .await
    {
        Ok(acquired) => acquired,
        Err(response) => return OpeningOutcome::Rejected(response),
    };
    // template:begin authn:grpc-router-run-authn
    let request = if let Some(auth) = &state.auth {
        match crate::authn::authenticate(auth, opening_context, request).await {
            Ok(request) => request,
            Err(response) => return OpeningOutcome::Rejected(response),
        }
    } else {
        request
    };
    // template:end authn:grpc-router-run-authn
    // template:begin service-auth:grpc-router-run-service-auth
    let request =
        match crate::service_auth::admit_caller(&state.caller_auth, opening_context, request).await
        {
            Ok(request) => request,
            Err(response) => return OpeningOutcome::Rejected(response),
        };
    // template:end service-auth:grpc-router-run-service-auth
    let (call_permit, request) = match acquire_permit(
        state.call_permits.as_ref(),
        &mut remaining_wait,
        opening_context,
        request,
    )
    .await
    {
        Ok(acquired) => acquired,
        Err(response) => return OpeningOutcome::Rejected(response),
    };
    let response = next.run(request).await;
    OpeningOutcome::Admitted {
        response,
        permit: call_permit,
    }
}

#[expect(
    clippy::result_large_err,
    reason = "the error is the refusal response the call returns"
)]
async fn acquire_permit(
    permits: Option<&Arc<Semaphore>>,
    remaining_wait: &mut Duration,
    opening_context: &OperationContext,
    request: Request,
) -> Result<(Option<OwnedSemaphorePermit>, Request), Response> {
    let Some(permits) = permits else {
        return Ok((None, request));
    };
    let wait = admission_wait(*remaining_wait, opening_context);
    let Ok((permit, waited)) =
        infra_http::acquire_within(permits, wait, observe::admission_waiting).await
    else {
        if let Some(reason) = opening_context.stopped() {
            return Err(server_stopped(reason).into_http());
        }
        return Err(reject_shed(opening_context, request).await);
    };
    *remaining_wait = remaining_wait.saturating_sub(waited.unwrap_or_default());
    Ok((Some(permit), request))
}

async fn reject_shed(opening_context: &OperationContext, request: Request) -> Response {
    let response = reject_with_budget(opening_context, request, shed_status()).await;
    if let Some(reason) = opening_context.stopped() {
        return server_stopped(reason).into_http();
    }
    observe::record_shed();
    response
}

fn shed_status() -> tonic::Status {
    Failure::new(service_failure::Code::ServiceUnavailable)
        .retry_after(AT_CAPACITY_RETRY_AFTER)
        .into_status_as(tonic::Code::ResourceExhausted, AT_CAPACITY_DETAIL)
}

fn admission_wait(max_wait: Duration, opening_context: &OperationContext) -> Duration {
    opening_context
        .remaining()
        .unwrap_or_default()
        .saturating_sub(REJECT_DRAIN)
        .min(max_wait)
}

async fn health_call(
    State(methods): State<Arc<Methods>>,
    mut request: Request,
    next: Next,
) -> Response {
    let _ = infra_http::take_listener_drain_token(request.extensions_mut());
    sanitize_extensions(request.extensions_mut());

    let call = observe::Call::start(&methods, &request);
    let fallback = observe::propagated_parent(call.span(), request.headers());
    let is_streaming = matches!(
        call.call_type(),
        Some(CallType::ServerStream | CallType::BidiStream)
    );
    let Some(budget) = grpc_timeout(request.headers()) else {
        let response = run_in_span(next.run(request), call.span(), fallback.as_ref(), || {
            internal_status().into_http()
        })
        .await;
        return attach_server(response, call, fallback, ServerCustody::default())
            .map(axum::body::Body::new);
    };

    let deadline = Deadline::new(tokio::time::Instant::now(), budget);
    let response_token = CancellationToken::new();
    let opening_context = OperationContext::new(Some(deadline), response_token.child_token());
    let response_context = ResponseContext(OperationContext::new(
        Some(deadline),
        response_token.clone(),
    ));
    let response_guard = response_token.drop_guard();
    request.extensions_mut().insert(opening_context.clone());
    request.extensions_mut().insert(response_context);

    let outcome = run_in_span(
        async {
            let response = tokio::select! {
                biased;
                reason = opening_context.wait_stopped() => {
                    return OpeningOutcome::Rejected(server_stopped(reason).into_http());
                }
                response = next.run(request) => response,
            };
            if let Some(reason) = opening_context.stopped() {
                return OpeningOutcome::Rejected(server_stopped(reason).into_http());
            }
            OpeningOutcome::Admitted {
                response,
                permit: None,
            }
        },
        call.span(),
        fallback.as_ref(),
        || OpeningOutcome::Rejected(internal_status().into_http()),
    )
    .await;

    finish_call(
        outcome,
        call,
        fallback,
        response_guard,
        Some(deadline),
        is_streaming,
    )
}

fn sanitize_extensions(extensions: &mut http::Extensions) {
    extensions.remove::<OperationContext>();
    extensions.remove::<ResponseContext>();
    // template:begin authn:grpc-router-strip-identity
    crate::authn::strip_identity(extensions);
    // template:end authn:grpc-router-strip-identity
    // template:begin service-auth:grpc-router-strip-caller
    crate::service_auth::strip_caller(extensions);
    // template:end service-auth:grpc-router-strip-caller
}

async fn run_in_span<T>(
    future: impl Future<Output = T>,
    span: &tracing::Span,
    fallback: Option<&opentelemetry::Context>,
    on_panic: fn() -> T,
) -> T {
    let instrumented = AssertUnwindSafe(future)
        .catch_unwind()
        .instrument(span.clone());
    let settled = match fallback {
        None => instrumented.await,
        Some(context) => instrumented.with_context(context.clone()).await,
    };
    settled.unwrap_or_else(|_panic| on_panic())
}

fn finish_call(
    outcome: OpeningOutcome,
    call: observe::Call,
    fallback: Option<opentelemetry::Context>,
    response_guard: DropGuard,
    response_deadline: Option<Deadline>,
    is_streaming: bool,
) -> Response {
    match outcome {
        OpeningOutcome::Admitted { response, permit } => {
            let custody = ServerCustody {
                permit,
                cancel_on_drop: Some(response_guard),
                deadline: response_deadline,
                watch_deadline: is_streaming && response_deadline.is_some(),
            };
            attach_server(response, call, fallback, custody).map(axum::body::Body::new)
        }
        OpeningOutcome::Rejected(response) => {
            drop(response_guard);
            attach_server(response, call, fallback, ServerCustody::default())
                .map(axum::body::Body::new)
        }
    }
}

const REJECT_DRAIN: Duration = Duration::from_millis(100);
const REJECT_DRAIN_BYTES: usize = 64 * 1024;

pub(crate) async fn reject_with_budget(
    opening_context: &OperationContext,
    request: Request,
    status: tonic::Status,
) -> Response {
    let drain = opening_context
        .remaining()
        .map_or(REJECT_DRAIN, |remaining| REJECT_DRAIN.min(remaining / 2));
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

    #[tokio::test]
    async fn rejected_call_reads_at_most_one_frame_past_64_kib() {
        use futures_util::StreamExt as _;
        use std::convert::Infallible;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let frames = Arc::new(AtomicUsize::new(0));
        let polled = Arc::clone(&frames);
        let body = futures_util::stream::iter(0..10).map(move |_| {
            polled.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(Bytes::from(vec![0_u8; 16 * 1024]))
        });
        let request = Request::new(axum::body::Body::from_stream(body));
        let context = OperationContext::unbounded();
        let response =
            reject_with_budget(&context, request, tonic::Status::unavailable("closed")).await;
        assert_eq!(response.headers()["grpc-status"], "14");
        assert_eq!(frames.load(Ordering::SeqCst), 5);
    }
}
