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
use infra_grpc_core::protocol::grpc_timeout;
use infra_http_transport::{ConnectionLimits, Http2Options};
use opentelemetry::context::FutureExt as _;
use operation_context::{Deadline, OperationContext};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::{CancellationToken, DropGuard};
use tonic::server::NamedService as _;
use tower::Layer as _;
use tracing::Instrument as _;

use crate::call::{
    ServerCustody, ServerOutcome, ServerStop, check_open, stopped_status, wrap_response,
};
use crate::health::HealthServer;
use crate::observe;
use crate::services::{BuiltServices, ListenerLayer, ListenerService, Method, MethodKind, Methods};
use crate::status::{ClassifiedStatus, FixedFailure};
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

struct AdmissionPermits {
    opening: Semaphore,
    call: Arc<Semaphore>,
    max_wait: Duration,
}

struct BusinessState {
    authentication: Authentication,
    methods: Arc<Methods>,
    request_timeout: Duration,
    admission: Option<AdmissionPermits>,
}

pub(crate) enum Rejection {
    Status(ClassifiedStatus),
    // template:begin authn:grpc-rejection-insufficient-scope
    InsufficientScope(ClassifiedStatus),
    // template:end authn:grpc-rejection-insufficient-scope
}

impl From<ClassifiedStatus> for Rejection {
    fn from(status: ClassifiedStatus) -> Self {
        Self::Status(status)
    }
}

#[derive(Clone, Debug)]
pub struct Application {
    routes: tonic::service::Routes,
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
        Ok(listener.serve_h2(self.routes.into_axum_router(), options, tls))
    }
}

impl<B> tower::Service<http::Request<B>> for Application
where
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<tower::BoxError>,
{
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = <tonic::service::Routes as tower::Service<http::Request<B>>>::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        tower::Service::<http::Request<B>>::poll_ready(&mut self.routes, cx)
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        tower::Service::call(&mut self.routes, request)
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
        service_names,
        methods,
        authentication,
    } = services.into_built(authentication)?;
    let limits = options.limits;
    observe::describe();
    if limits.max_in_flight.is_some() {
        observe::register_shed();
    }
    let admission = limits.max_in_flight.map(|count| {
        let count = count.get() as usize;
        AdmissionPermits {
            opening: Semaphore::new(count),
            call: Arc::new(Semaphore::new(count)),
            max_wait: limits.max_admission_wait,
        }
    });
    // template:begin service-auth:grpc-app-tls-init
    let requires_tls = matches!(authentication, Authentication::ServiceTokens(_));
    // template:end service-auth:grpc-app-tls-init
    let state = Arc::new(BusinessState {
        authentication,
        methods: Arc::clone(&methods),
        request_timeout: limits.request_timeout,
        admission,
    });
    let business = routes
        .into_axum_router()
        .layer(axum::middleware::from_fn_with_state(state, business_call));
    let router = native_router(
        business,
        listener_layers,
        readiness,
        service_names,
        methods,
        limits.request_timeout,
        options.trust_trace_context,
    );
    Ok(Application {
        routes: tonic::service::Routes::from(router),
        // template:begin service-auth:grpc-app-tls-field
        requires_tls,
        // template:end service-auth:grpc-app-tls-field
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
    service_names: BTreeSet<&'static str>,
    methods: Arc<Methods>,
    request_timeout: Duration,
    trust_trace_context: bool,
) -> axum::Router {
    let health = HealthServer::new(crate::health::Adapter::new(readiness, service_names));
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
        let base = ListenerService::new(tower::util::MapResponse::new(
            router,
            |response: Response| response.map(tonic::body::Body::new),
        ));
        let layered = listener_layers
            .into_iter()
            .fold(base, |service, layer| layer.layer(service));
        #[expect(
            clippy::disallowed_methods,
            reason = "the native gRPC listener delegates every route and its tonic fallback through the stacked listener layers"
        )]
        axum::Router::new()
            .fallback_service(tower::util::MapRequest::new(layered, |request: Request| {
                request.map(tonic::body::Body::new)
            }))
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
    if let Some(rejection) = non_grpc_rejection(&request) {
        drain_rejected(request, REJECT_DRAIN_TIMEOUT).await;
        return rejection;
    }
    infra_http_transport::admit_trace_context(request.headers_mut(), trust_trace_context);
    next.run(request).await
}

fn non_grpc_rejection(request: &Request) -> Option<Response> {
    if request.method() != http::Method::POST {
        return Some(
            (
                http::StatusCode::METHOD_NOT_ALLOWED,
                [(http::header::ALLOW, "POST")],
            )
                .into_response(),
        );
    }
    if !crate::protocol::has_native_content_type(request.headers()) {
        return Some(http::StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response());
    }
    None
}

struct Admitted {
    response: Response,
    permit: Option<OwnedSemaphorePermit>,
}

struct StartedCall<'a> {
    method: &'a Method,
    call: observe::Call,
    fallback_context: Option<opentelemetry::Context>,
    opening_deadline: Deadline,
    response_deadline: Option<Deadline>,
}

fn start_call<'a>(
    request: &mut Request,
    methods: &'a Methods,
    request_timeout: Duration,
) -> StartedCall<'a> {
    sanitize_extensions(request.extensions_mut());
    let path = request.uri().path();
    let method = methods.resolve(path);
    let call = observe::Call::start(method, path);
    let fallback_context = observe::attach_parent_or_fallback(call.span(), request.headers());
    let (opening_deadline, response_deadline) = call_deadlines(request.headers(), request_timeout);
    StartedCall {
        method,
        call,
        fallback_context,
        opening_deadline,
        response_deadline,
    }
}

struct BusinessLifecycle {
    opening_context: OperationContext,
    response_context: ResponseContext,
    cancel_on_drop: DropGuard,
    stop: Option<ServerStop>,
}

fn business_lifecycle(
    method: MethodKind,
    opening_deadline: Deadline,
    response_deadline: Option<Deadline>,
    connection: Option<infra_http_transport::ConnectionInfo>,
) -> BusinessLifecycle {
    let response_token = match connection.filter(|_| method.streams_response()) {
        Some(info) => info.child_drain_token(),
        None => CancellationToken::new(),
    };
    let opening_context =
        OperationContext::new(Some(opening_deadline), response_token.child_token());
    let response_context = ResponseContext(OperationContext::new(
        response_deadline,
        response_token.clone(),
    ));
    let stop = if method.end_on_drain() {
        Some(ServerStop::Drain {
            deadline: response_deadline,
            drain: response_token.clone(),
        })
    } else {
        response_deadline.map(ServerStop::Deadline)
    };
    BusinessLifecycle {
        opening_context,
        response_context,
        cancel_on_drop: response_token.drop_guard(),
        stop,
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
    if matches!(state.authentication, Authentication::ServiceTokens(_))
        && connection
            .as_ref()
            .is_none_or(|info| !info.scheme().is_tls())
    {
        return tonic::Status::unauthenticated("service tokens require TLS").into_http();
    }
    // template:end service-auth:grpc-router-verified-transport

    let started = start_call(&mut request, &state.methods, state.request_timeout);
    let lifecycle = business_lifecycle(
        started.method.kind,
        started.opening_deadline,
        started.response_deadline,
        connection,
    );
    request
        .extensions_mut()
        .insert(lifecycle.opening_context.clone());
    request.extensions_mut().insert(lifecycle.response_context);

    let outcome = run_in_span(
        async {
            match lifecycle
                .opening_context
                .run(open_business(
                    &state,
                    &lifecycle.opening_context,
                    started.method,
                    request,
                    next,
                ))
                .await
            {
                Ok(outcome) => outcome,
                Err(reason) => Err(stopped_status(reason).into()),
            }
        },
        started.call.span(),
        started.fallback_context.as_ref(),
        || {
            Err(Rejection::from(ClassifiedStatus::fixed(
                FixedFailure::Internal,
            )))
        },
    )
    .await;

    finish_call(
        outcome,
        started.call,
        started.fallback_context,
        lifecycle.cancel_on_drop,
        lifecycle.stop,
    )
}

async fn open_business(
    state: &BusinessState,
    opening_context: &OperationContext,
    method: &Method,
    request: Request,
    next: Next,
) -> Result<Admitted, Rejection> {
    let mut remaining_wait = state
        .admission
        .as_ref()
        .map_or(Duration::ZERO, |admission| admission.max_wait);
    let (_opening_permit, request) = acquire_permit(
        state.admission.as_ref().map(|admission| &admission.opening),
        &mut remaining_wait,
        opening_context,
        request,
        |permits, wait| {
            infra_http_transport::acquire_borrowed_within(permits, wait, observe::admission_waiting)
        },
    )
    .await?;
    let request = match &state.authentication {
        // template:begin authn:grpc-router-run-authn
        Authentication::Bearer(verifier) => {
            crate::authn::authenticate(
                verifier,
                method.scope_requirement(),
                opening_context,
                request,
            )
            .await?
        }
        // template:end authn:grpc-router-run-authn
        // template:begin service-auth:grpc-router-run-service-auth
        Authentication::ServiceTokens(verifier) => {
            crate::service_auth::admit_caller(
                verifier,
                method.allowed_callers(),
                opening_context,
                request,
            )
            .await?
        }
        // template:end service-auth:grpc-router-run-service-auth
        Authentication::None => {
            // template:begin service-auth:grpc-router-strip-unverified-authorization
            let mut request = request;
            request.headers_mut().remove(http::header::AUTHORIZATION);
            // template:end service-auth:grpc-router-strip-unverified-authorization
            request
        }
    };
    if !method.kind.is_registered() {
        let status = ClassifiedStatus::plain(tonic::Status::unimplemented(""));
        return Err(reject_with_budget(opening_context, request, status)
            .await
            .into());
    }
    let (call_permit, request) = acquire_permit(
        state.admission.as_ref().map(|admission| &admission.call),
        &mut remaining_wait,
        opening_context,
        request,
        |permits, wait| {
            infra_http_transport::acquire_within(permits, wait, observe::admission_waiting)
        },
    )
    .await?;
    let response = next.run(request.map(without_trailer_credentials)).await;
    Ok(Admitted {
        response,
        permit: call_permit,
    })
}

async fn acquire_permit<S, P, F>(
    permits: Option<S>,
    remaining_wait: &mut Duration,
    opening_context: &OperationContext,
    request: Request,
    acquire: impl FnOnce(S, Duration) -> F,
) -> Result<(Option<P>, Request), ClassifiedStatus>
where
    F: Future<Output = infra_http_transport::Admission<P>>,
{
    let Some(permits) = permits else {
        return Ok((None, request));
    };
    let wait = admission_wait(*remaining_wait, opening_context);
    let permit = match acquire(permits, wait).await {
        infra_http_transport::Admission::Immediate(permit) => permit,
        infra_http_transport::Admission::Waited { permit, waited } => {
            *remaining_wait = remaining_wait.saturating_sub(waited);
            permit
        }
        infra_http_transport::Admission::Shed { .. } => {
            check_open(opening_context)?;
            return Err(reject_shed(opening_context, request).await);
        }
    };
    Ok((Some(permit), request))
}

async fn reject_shed(opening_context: &OperationContext, request: Request) -> ClassifiedStatus {
    let status = reject_with_budget(
        opening_context,
        request,
        ClassifiedStatus::fixed(FixedFailure::AtCapacity),
    )
    .await;
    if let Some(reason) = opening_context.stopped() {
        return stopped_status(reason);
    }
    observe::record_shed();
    status
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
    let fixed = |failure: FixedFailure, response_deadline| {
        (
            failure.into_http(),
            Some(ServerOutcome::fixed(failure)),
            response_deadline,
        )
    };
    let (response, known_outcome, response_deadline) = run_in_span(
        async {
            tokio::select! {
                biased;
                () = started.opening_deadline.wait() => fixed(FixedFailure::Deadline, None),
                response = next.run(request) => {
                    if started.opening_deadline.expired() {
                        fixed(FixedFailure::Deadline, None)
                    } else {
                        (response, None, started.response_deadline)
                    }
                }
            }
        },
        started.call.span(),
        started.fallback_context.as_ref(),
        || fixed(FixedFailure::Internal, started.response_deadline),
    )
    .await;

    wrap_response(
        response,
        started.call,
        started.fallback_context,
        ServerCustody::deadline(response_deadline),
        known_outcome,
    )
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
    fallback_context: Option<&opentelemetry::Context>,
    on_panic: impl FnOnce() -> T,
) -> T {
    let instrumented = AssertUnwindSafe(future)
        .catch_unwind()
        .instrument(span.clone());
    let settled = match fallback_context {
        None => instrumented.await,
        Some(context) => instrumented.with_context(context.clone()).await,
    };
    settled.unwrap_or_else(|_panic| on_panic())
}

fn finish_call(
    outcome: Result<Admitted, Rejection>,
    call: observe::Call,
    fallback_context: Option<opentelemetry::Context>,
    cancel_on_drop: DropGuard,
    stop: Option<ServerStop>,
) -> Response {
    match outcome {
        Ok(Admitted { response, permit }) => wrap_response(
            response,
            call,
            fallback_context,
            ServerCustody {
                permit,
                cancel_on_drop: Some(cancel_on_drop),
                stop,
            },
            None,
        ),
        // template:begin authn:grpc-router-scope-refusal-span
        Err(Rejection::InsufficientScope(status)) => {
            call.record_insufficient_scope();
            finish_rejected(status, call, cancel_on_drop)
        }
        // template:end authn:grpc-router-scope-refusal-span
        Err(Rejection::Status(status)) => finish_rejected(status, call, cancel_on_drop),
    }
}

fn finish_rejected(
    status: ClassifiedStatus,
    call: observe::Call,
    cancel_on_drop: DropGuard,
) -> Response {
    drop(cancel_on_drop);
    call.finish_with_failure(status.code(), status.catalog_code());
    status.into_http()
}

const REJECT_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);
const REJECT_DRAIN_BYTES: usize = 64 * 1024;

pub(crate) async fn reject_with_budget(
    opening_context: &OperationContext,
    request: Request,
    status: ClassifiedStatus,
) -> ClassifiedStatus {
    let timeout = opening_context
        .remaining()
        .map_or(REJECT_DRAIN_TIMEOUT, |remaining| {
            REJECT_DRAIN_TIMEOUT.min(remaining / 2)
        });
    drain_rejected(request, timeout).await;
    status
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
        let status = reject_with_budget(
            &context,
            request,
            ClassifiedStatus::plain(tonic::Status::unavailable("closed")),
        )
        .await;
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(frames.load(Ordering::SeqCst), 4);
    }
}
