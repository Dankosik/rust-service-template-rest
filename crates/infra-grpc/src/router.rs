use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
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

use crate::call::{ServerCustody, attach_server, deadline_status, server_stopped};
use crate::health::HealthServer;
use crate::observe;
use crate::protocol::grpc_timeout;
use crate::services::{CallType, Methods};
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

#[derive(Debug)]
pub enum Authentication {
    None,
    // template:begin authn:grpc-authentication-bearer
    Bearer(infra_bearerauthn::Verifier),
    // template:end authn:grpc-authentication-bearer
    // template:begin service-auth:grpc-authentication-service-tokens
    ServiceTokens(infra_serviceauth::Verifier),
    // template:end service-auth:grpc-authentication-service-tokens
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
    authentication: Authentication,
    methods: Arc<Methods>,
    limits: Limits,
    opening_permits: Option<Arc<Semaphore>>,
    call_permits: Option<Arc<Semaphore>>,
    // template:begin authn:grpc-router-bearer-field
    scopes: crate::authn::Scopes,
    // template:end authn:grpc-router-bearer-field
    // template:begin service-auth:grpc-router-caller-field
    caller_rules: crate::service_auth::CallerRules,
    // template:end service-auth:grpc-router-caller-field
}

pub fn router(
    mut services: Services,
    readiness: ::health::ReadinessReader,
    authentication: Authentication,
    limits: Limits,
) -> Result<axum::Router, Error> {
    if services.reflection {
        services.attach_reflection()?;
    }
    services.learn(tonic_health::pb::FILE_DESCRIPTOR_SET)?;
    // template:begin service-auth:grpc-router-caller-rules
    if matches!(authentication, Authentication::ServiceTokens(_))
        || !services.caller_rules.is_empty()
    {
        services.validate_caller_rules()?;
    }
    // template:end service-auth:grpc-router-caller-rules
    // template:begin authn:grpc-router-authenticate
    if matches!(authentication, Authentication::None) {
        return Err(Error::UnauthenticatedBusinessCalls);
    }
    if !services.scopes.is_empty() && !matches!(authentication, Authentication::Bearer(_)) {
        return Err(Error::ScopesWithServiceTokens);
    }
    // template:end authn:grpc-router-authenticate
    if limits.max_in_flight.is_some() {
        observe::register_shed();
    }
    observe::describe();
    let methods = Arc::new(services.methods());
    let state = Arc::new(BusinessState {
        authentication,
        methods: Arc::clone(&methods),
        limits,
        opening_permits: semaphore(limits.max_in_flight),
        call_permits: semaphore(limits.max_in_flight),
        // template:begin authn:grpc-router-bearer-init
        scopes: services.scopes,
        // template:end authn:grpc-router-bearer-init
        // template:begin service-auth:grpc-router-caller-init
        caller_rules: services.caller_rules,
        // template:end service-auth:grpc-router-caller-init
    });
    let business = services
        .routes
        .routes()
        .into_axum_router()
        .layer(axum::middleware::from_fn_with_state(state, business_call));
    Ok(native_router(
        business,
        readiness,
        services.names,
        methods,
        limits.request_timeout,
    ))
}

#[must_use]
pub fn server_options(limits: Limits) -> infra_http_transport::ServerOptions {
    infra_http_transport::ServerOptions {
        header_read_timeout: Duration::from_secs(5),
        max_header_bytes: 16 * 1024,
        max_connections: limits.max_connections,
        max_connection_age: limits.max_connection_age,
    }
}

fn semaphore(limit: Option<NonZeroU32>) -> Option<Arc<Semaphore>> {
    limit.map(|count| Arc::new(Semaphore::new(count.get() as usize)))
}

fn native_router(
    router: axum::Router,
    readiness: ::health::ReadinessReader,
    names: BTreeSet<&'static str>,
    methods: Arc<Methods>,
    request_timeout: Duration,
) -> axum::Router {
    let health = HealthServer::new(crate::health::Adapter::new(readiness, names));
    #[expect(
        clippy::disallowed_methods,
        reason = "gRPC health registration belongs to the separate tonic listener"
    )]
    let health =
        axum::Router::new().route_service(&format!("/{}/{{*rest}}", HealthServer::NAME), health);
    router
        .merge(health.layer(axum::middleware::from_fn_with_state(
            (methods, request_timeout),
            health_call,
        )))
        .layer(axum::middleware::from_fn(native_grpc))
}

async fn native_grpc(request: Request, next: Next) -> Response {
    let response = if request.method() != http::Method::POST {
        (
            http::StatusCode::METHOD_NOT_ALLOWED,
            [(http::header::ALLOW, "POST")],
        )
            .into_response()
    } else if !crate::protocol::native_content_type(request.headers()) {
        http::StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response()
    } else {
        return next.run(request).await;
    };
    drain_rejected(request, REJECT_DRAIN).await;
    response
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
    let connection = request
        .extensions_mut()
        .remove::<infra_http_transport::ConnectionInfo>();
    sanitize_extensions(request.extensions_mut());

    let path = request.uri().path();
    let method = state.methods.resolve(path);
    let call = observe::Call::start(method, path);
    let fallback = observe::propagated_parent(call.span(), request.headers());
    let (opening_deadline, response_deadline) =
        call_deadlines(request.headers(), state.limits.request_timeout);

    let is_streaming = matches!(
        method.call_type,
        Some(CallType::ServerStream | CallType::BidiStream)
    );
    let response_token = match connection.filter(|_| is_streaming) {
        Some(info) => info.child_drain_token(),
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

    finish_call(outcome, call, fallback, response_guard, response_deadline)
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
    let admission = match &state.authentication {
        // template:begin authn:grpc-router-run-authn
        Authentication::Bearer(verifier) => {
            crate::authn::authenticate(verifier, &state.scopes, opening_context, request).await
        }
        // template:end authn:grpc-router-run-authn
        // template:begin service-auth:grpc-router-run-service-auth
        Authentication::ServiceTokens(verifier) => {
            crate::service_auth::admit_caller(
                verifier,
                &state.caller_rules,
                opening_context,
                request,
            )
            .await
        }
        // template:end service-auth:grpc-router-run-service-auth
        Authentication::None => {
            // template:begin service-auth:grpc-router-strip-unverified-authorization
            let mut request = request;
            request.headers_mut().remove(http::header::AUTHORIZATION);
            // template:end service-auth:grpc-router-strip-unverified-authorization
            Ok(request)
        }
    };
    let request = match admission {
        Ok(request) => request,
        Err(response) => return OpeningOutcome::Rejected(response),
    };
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
    let response = next.run(request.map(without_trailer_credentials)).await;
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
        infra_http_transport::acquire_within(permits, wait, observe::admission_waiting).await
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
    State((methods, request_timeout)): State<(Arc<Methods>, Duration)>,
    mut request: Request,
    next: Next,
) -> Response {
    request
        .extensions_mut()
        .remove::<infra_http_transport::ConnectionInfo>();
    sanitize_extensions(request.extensions_mut());

    let path = request.uri().path();
    let call = observe::Call::start(methods.resolve(path), path);
    let fallback = observe::propagated_parent(call.span(), request.headers());
    let (opening_deadline, response_deadline) = call_deadlines(request.headers(), request_timeout);
    let response = run_in_span(
        async {
            tokio::select! {
                biased;
                () = opening_deadline.wait() => deadline_status().into_http(),
                response = next.run(request) => {
                    if opening_deadline.expired() {
                        deadline_status().into_http()
                    } else {
                        response
                    }
                }
            }
        },
        call.span(),
        fallback.as_ref(),
        || internal_status().into_http(),
    )
    .await;

    let custody = ServerCustody {
        deadline: response_deadline,
        ..ServerCustody::default()
    };
    attach_server(response, call, fallback, custody).map(axum::body::Body::new)
}

fn call_deadlines(
    headers: &http::HeaderMap,
    request_timeout: Duration,
) -> (Deadline, Option<Deadline>) {
    let origin = tokio::time::Instant::now();
    let caller = grpc_timeout(headers);
    let opening = caller.map_or(request_timeout, |budget| budget.min(request_timeout));
    (
        Deadline::new(origin, opening),
        caller.map(|budget| Deadline::new(origin, budget)),
    )
}

fn without_trailer_credentials(body: axum::body::Body) -> axum::body::Body {
    axum::body::Body::new(body.map_frame(|mut frame| {
        if let Some(trailers) = frame.trailers_mut() {
            trailers.remove(http::header::AUTHORIZATION);
        }
        frame
    }))
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
) -> Response {
    match outcome {
        OpeningOutcome::Admitted { response, permit } => {
            let custody = ServerCustody {
                permit,
                cancel_on_drop: Some(response_guard),
                deadline: response_deadline,
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
    drain_rejected(request, drain).await;
    status.into_http()
}

async fn drain_rejected(request: Request, drain: Duration) {
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
}

#[cfg(test)]
mod ingress_tests;

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
