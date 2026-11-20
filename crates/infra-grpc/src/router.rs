use std::collections::BTreeSet;
use std::convert::Infallible;
use std::num::NonZeroU32;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use bytes::Bytes;
use futures_util::FutureExt as _;
use http_body_util::BodyExt as _;
use infra_http_transport::{ConnectionLimits, Http2Options};
use opentelemetry::context::FutureExt as _;
use operation_context::{Deadline, OperationContext};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tonic::server::NamedService as _;
use tower::Layer as _;
use tracing::Instrument as _;

use crate::call::{ServerCustody, check_open, stopped_status, wrap_response};
use crate::health::HealthServer;
use crate::observe;
use crate::protocol::grpc_timeout;
use crate::services::{
    BuiltServices, CallType, ListenerLayer, ListenerService, Methods, REFLECTION_PATHS,
};
use crate::status::{FixedFailure, internal_status};
use crate::{Error, ResponseContext, Services, TlsRequiredError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub request_timeout: Duration,
    pub max_in_flight: Option<NonZeroU32>,
    pub max_admission_wait: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerOptions {
    pub limits: Limits,
    pub trust_trace_context: bool,
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

pub(crate) enum BusinessAuth {
    None,
    // template:begin authn:grpc-router-bearer-field
    Bearer {
        verifier: infra_bearerauthn::Verifier,
        scopes: crate::authn::Scopes,
    },
    // template:end authn:grpc-router-bearer-field
    // template:begin service-auth:grpc-router-caller-field
    ServiceTokens {
        verifier: infra_serviceauth::Verifier,
        caller_rules: crate::service_auth::CallerRules,
    },
    // template:end service-auth:grpc-router-caller-field
}

struct AdmissionPermits {
    opening: Arc<Semaphore>,
    call: Arc<Semaphore>,
    max_wait: Duration,
}

struct BusinessState {
    authentication: BusinessAuth,
    methods: Arc<Methods>,
    request_timeout: Duration,
    reflection_enabled: bool,
    admission: Option<AdmissionPermits>,
}

#[derive(Clone, Debug)]
pub struct Application {
    router: axum::Router,
    // template:begin service-auth:grpc-app-tls-requirement
    requires_tls: bool,
    // template:end service-auth:grpc-app-tls-requirement
}

impl Application {
    pub fn serve(
        self,
        listener: infra_http_transport::BoundListener,
        options: infra_http_transport::Http2Options,
        tls: Option<infra_http_transport::TlsOptions>,
    ) -> Result<infra_http_transport::Server, TlsRequiredError> {
        // template:begin service-auth:grpc-app-tls-check
        if self.requires_tls && tls.is_none() {
            return Err(TlsRequiredError);
        }
        // template:end service-auth:grpc-app-tls-check
        Ok(listener.serve_h2(self.router, options, tls))
    }
}

impl<B> tower::Service<http::Request<B>> for Application
where
    axum::Router: tower::Service<http::Request<B>, Response = Response, Error = Infallible>,
{
    type Response = Response;
    type Error = Infallible;
    type Future = <axum::Router as tower::Service<http::Request<B>>>::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        tower::Service::<http::Request<B>>::poll_ready(&mut self.router, cx)
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        tower::Service::call(&mut self.router, request)
    }
}

pub(crate) fn build(
    services: Services,
    readiness: ::health::ReadinessReader,
    authentication: Authentication,
    options: ServerOptions,
) -> Result<Application, Error> {
    let BuiltServices {
        routes,
        listener_layers,
        names,
        methods,
        reflection,
        authentication,
    } = services.into_built(authentication)?;
    let limits = options.limits;
    if limits.max_in_flight.is_some() {
        observe::register_shed();
    }
    observe::describe();
    let admission = limits.max_in_flight.map(|count| {
        let count = count.get() as usize;
        AdmissionPermits {
            opening: Arc::new(Semaphore::new(count)),
            call: Arc::new(Semaphore::new(count)),
            max_wait: limits.max_admission_wait,
        }
    });
    Ok(Application {
        // template:begin service-auth:grpc-app-tls-init
        requires_tls: matches!(authentication, BusinessAuth::ServiceTokens { .. }),
        // template:end service-auth:grpc-app-tls-init
        router: {
            let state = Arc::new(BusinessState {
                authentication,
                methods: Arc::clone(&methods),
                request_timeout: limits.request_timeout,
                reflection_enabled: reflection,
                admission,
            });
            let business = routes
                .into_axum_router()
                .layer(axum::middleware::from_fn_with_state(state, business_call));
            native_router(
                business,
                listener_layers,
                readiness,
                names,
                methods,
                limits.request_timeout,
                options.trust_trace_context,
            )
        },
    })
}

#[must_use]
pub fn server_options(connections: ConnectionLimits) -> Http2Options {
    Http2Options {
        preface_timeout: Duration::from_secs(5),
        max_header_list_size: 16 * 1024,
        connections,
    }
}

fn native_router(
    router: axum::Router,
    listener_layers: Vec<ListenerLayer>,
    readiness: ::health::ReadinessReader,
    names: BTreeSet<&'static str>,
    methods: Arc<Methods>,
    request_timeout: Duration,
    trust_trace_context: bool,
) -> axum::Router {
    let health = HealthServer::new(crate::health::Adapter::new(readiness, names));
    #[expect(
        clippy::disallowed_methods,
        reason = "gRPC health registration belongs to the separate tonic listener"
    )]
    let health =
        axum::Router::new().route_service(&format!("/{}/{{*rest}}", HealthServer::NAME), health);
    let router = router.merge(health.layer(axum::middleware::from_fn_with_state(
        (methods, request_timeout),
        health_call,
    )));
    let router = if listener_layers.is_empty() {
        router
    } else {
        let layered = listener_layers
            .into_iter()
            .fold(ListenerService::new(router), |service, layer| {
                layer.layer(service)
            });
        #[expect(
            clippy::disallowed_methods,
            reason = "the native gRPC listener delegates every route and its tonic fallback through the stacked listener layers"
        )]
        axum::Router::new().fallback_service(layered)
    };
    router.layer(axum::middleware::from_fn_with_state(
        trust_trace_context,
        native_grpc,
    ))
}

async fn native_grpc(
    State(trust_trace_context): State<bool>,
    mut request: Request,
    next: Next,
) -> Response {
    let response = if request.method() != http::Method::POST {
        (
            http::StatusCode::METHOD_NOT_ALLOWED,
            [(http::header::ALLOW, "POST")],
        )
            .into_response()
    } else if !crate::protocol::has_native_content_type(request.headers()) {
        http::StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response()
    } else {
        infra_http_transport::admit_trace_context(request.headers_mut(), trust_trace_context);
        return next.run(request).await;
    };
    drain_rejected(request, REJECT_DRAIN_TIMEOUT).await;
    response
}

struct Admitted {
    response: Response,
    permit: Option<OwnedSemaphorePermit>,
}

struct StartedCall {
    call_type: Option<CallType>,
    call: observe::Call,
    fallback: Option<opentelemetry::Context>,
    opening_deadline: Deadline,
    response_deadline: Option<Deadline>,
}

fn start_call(request: &mut Request, methods: &Methods, request_timeout: Duration) -> StartedCall {
    sanitize_extensions(request.extensions_mut());
    let path = request.uri().path();
    let method = methods.resolve(path);
    let call = observe::Call::start(method, path);
    let fallback = observe::attach_parent_or_fallback(call.span(), request.headers());
    let (opening_deadline, response_deadline) = call_deadlines(request.headers(), request_timeout);
    StartedCall {
        call_type: method.call_type,
        call,
        fallback,
        opening_deadline,
        response_deadline,
    }
}

struct BusinessLifecycle {
    opening_context: OperationContext,
    response_context: ResponseContext,
    custody: ServerCustody,
}

fn business_lifecycle(
    path: &str,
    call_type: Option<CallType>,
    opening_deadline: Deadline,
    response_deadline: Option<Deadline>,
    connection: Option<infra_http_transport::ConnectionInfo>,
    reflection_enabled: bool,
) -> BusinessLifecycle {
    let streams_response = matches!(
        call_type,
        Some(CallType::ServerStream | CallType::BidiStream)
    );
    let response_token = match connection.filter(|_| streams_response) {
        Some(info) => info.child_drain_token(),
        None => CancellationToken::new(),
    };
    let opening_context =
        OperationContext::new(Some(opening_deadline), response_token.child_token());
    let response_context = ResponseContext(OperationContext::new(
        response_deadline,
        response_token.clone(),
    ));
    let custody = ServerCustody {
        deadline: response_deadline,
        end_on_drain: (reflection_enabled && REFLECTION_PATHS.contains(&path))
            .then(|| response_token.clone()),
        cancel_on_drop: Some(response_token.drop_guard()),
        ..ServerCustody::default()
    };
    BusinessLifecycle {
        opening_context,
        response_context,
        custody,
    }
}

async fn business_call(
    State(state): State<Arc<BusinessState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let connection = request
        .extensions_mut()
        .remove::<infra_http_transport::ConnectionInfo>();
    // template:begin service-auth:grpc-router-verified-transport
    if matches!(state.authentication, BusinessAuth::ServiceTokens { .. })
        && connection
            .as_ref()
            .is_none_or(|info| info.scheme() != "https")
    {
        return tonic::Status::unauthenticated("service tokens require TLS").into_http();
    }
    // template:end service-auth:grpc-router-verified-transport

    let started = start_call(&mut request, &state.methods, state.request_timeout);
    let lifecycle = business_lifecycle(
        request.uri().path(),
        started.call_type,
        started.opening_deadline,
        started.response_deadline,
        connection,
        state.reflection_enabled,
    );
    request
        .extensions_mut()
        .insert(lifecycle.opening_context.clone());
    request.extensions_mut().insert(lifecycle.response_context);

    #[allow(
        clippy::result_large_err,
        reason = "the panic fallback returns the same refusal response"
    )]
    let outcome = run_in_span(
        async {
            lifecycle
                .opening_context
                .run(open_business(
                    &state,
                    &lifecycle.opening_context,
                    started.call_type,
                    request,
                    next,
                ))
                .await
                .map_err(|reason| stopped_status(reason).into_http())
                .and_then(std::convert::identity)
        },
        started.call.span(),
        started.fallback.as_ref(),
        || Err(internal_status().into_http()),
    )
    .await;

    finish_call(outcome, started.call, started.fallback, lifecycle.custody)
}

#[expect(
    clippy::result_large_err,
    reason = "the error is the refusal response the call returns"
)]
async fn open_business(
    state: &BusinessState,
    opening_context: &OperationContext,
    call_type: Option<CallType>,
    request: Request,
    next: Next,
) -> Result<Admitted, Response> {
    let mut remaining_wait = state
        .admission
        .as_ref()
        .map_or(Duration::ZERO, |admission| admission.max_wait);
    let (_opening_permit, request) = acquire_permit(
        state.admission.as_ref().map(|admission| &admission.opening),
        &mut remaining_wait,
        opening_context,
        request,
    )
    .await?;
    let authenticated: Result<_, Response> = match &state.authentication {
        // template:begin authn:grpc-router-run-authn
        BusinessAuth::Bearer { verifier, scopes } => {
            crate::authn::authenticate(verifier, scopes, opening_context, request).await
        }
        // template:end authn:grpc-router-run-authn
        // template:begin service-auth:grpc-router-run-service-auth
        BusinessAuth::ServiceTokens {
            verifier,
            caller_rules,
        } => {
            crate::service_auth::admit_caller(verifier, caller_rules, opening_context, request)
                .await
        }
        // template:end service-auth:grpc-router-run-service-auth
        BusinessAuth::None => {
            // template:begin service-auth:grpc-router-strip-unverified-authorization
            let mut request = request;
            request.headers_mut().remove(http::header::AUTHORIZATION);
            // template:end service-auth:grpc-router-strip-unverified-authorization
            Ok(request)
        }
    };
    let request = authenticated?;
    if call_type.is_none() {
        let status = tonic::Status::unimplemented("");
        return Err(reject_with_budget(opening_context, request, status).await);
    }
    let (call_permit, request) = acquire_permit(
        state.admission.as_ref().map(|admission| &admission.call),
        &mut remaining_wait,
        opening_context,
        request,
    )
    .await?;
    let response = next.run(request.map(without_trailer_credentials)).await;
    Ok(Admitted {
        response,
        permit: call_permit,
    })
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
        check_open(opening_context)?;
        return Err(reject_shed(opening_context, request).await);
    };
    *remaining_wait = remaining_wait.saturating_sub(waited.unwrap_or_default());
    Ok((Some(permit), request))
}

async fn reject_shed(opening_context: &OperationContext, request: Request) -> Response {
    let response =
        reject_with_budget(opening_context, request, FixedFailure::AtCapacity.status()).await;
    if let Some(reason) = opening_context.stopped() {
        return stopped_status(reason).into_http();
    }
    observe::record_shed();
    response
}

fn admission_wait(max_wait: Duration, opening_context: &OperationContext) -> Duration {
    if max_wait.is_zero() {
        return Duration::ZERO;
    }
    opening_context
        .remaining()
        .unwrap_or_default()
        .saturating_sub(REJECT_DRAIN_TIMEOUT)
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
    let started = start_call(&mut request, &methods, request_timeout);
    let (response, response_deadline) = run_in_span(
        async {
            let expired = || (FixedFailure::Deadline.status().into_http(), None);
            tokio::select! {
                biased;
                () = started.opening_deadline.wait() => expired(),
                response = next.run(request) => {
                    if started.opening_deadline.expired() {
                        expired()
                    } else {
                        (response, started.response_deadline)
                    }
                }
            }
        },
        started.call.span(),
        started.fallback.as_ref(),
        || (internal_status().into_http(), started.response_deadline),
    )
    .await;

    let custody = ServerCustody {
        deadline: response_deadline,
        ..ServerCustody::default()
    };
    wrap_response(response, started.call, started.fallback, custody)
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
    on_panic: impl FnOnce() -> T,
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
    outcome: Result<Admitted, Response>,
    call: observe::Call,
    fallback: Option<opentelemetry::Context>,
    mut custody: ServerCustody,
) -> Response {
    match outcome {
        Ok(Admitted { response, permit }) => {
            custody.permit = permit;
            wrap_response(response, call, fallback, custody)
        }
        Err(response) => {
            // template:begin authn:grpc-router-scope-refusal-span
            if response
                .extensions()
                .get::<crate::authn::InsufficientScope>()
                .is_some()
            {
                call.record_insufficient_scope();
            }
            // template:end authn:grpc-router-scope-refusal-span
            drop(custody);
            wrap_response(response, call, fallback, ServerCustody::default())
        }
    }
}

const REJECT_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);
const REJECT_DRAIN_BYTES: usize = 64 * 1024;

pub(crate) async fn reject_with_budget(
    opening_context: &OperationContext,
    request: Request,
    status: tonic::Status,
) -> Response {
    let timeout = opening_context
        .remaining()
        .map_or(REJECT_DRAIN_TIMEOUT, |remaining| {
            REJECT_DRAIN_TIMEOUT.min(remaining / 2)
        });
    drain_rejected(request, timeout).await;
    status.into_http()
}

#[warn(clippy::arithmetic_side_effects)]
async fn drain_rejected(request: Request, timeout: Duration) {
    let mut body = request.into_body();
    let _ = tokio::time::timeout(timeout, async {
        let mut remaining = REJECT_DRAIN_BYTES;
        while let Some(Ok(frame)) = body.frame().await {
            tokio::task::consume_budget().await;
            remaining = remaining.saturating_sub(frame.data_ref().map_or(0, Bytes::len));
            if remaining == 0 {
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
    async fn rejected_call_stops_reading_when_frames_reach_64_kib() {
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
        assert_eq!(frames.load(Ordering::SeqCst), 4);
    }
}
