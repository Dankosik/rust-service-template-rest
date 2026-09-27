use std::collections::BTreeSet;
use std::convert::Infallible;
use std::num::NonZeroU32;
use std::time::Duration;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use tower::ServiceExt as _;

use crate::Error;

/// Upper bound for one business call, also the floor for the process drain budget.
pub const UNARY_DEADLINE: Duration = Duration::from_secs(8);

/// Retry hint on a shed call; matches the HTTP listener's `Retry-After`.
const SHED_RETRY_AFTER: Duration = Duration::from_secs(1);

const BUSINESS_CONCURRENCY: usize = 256;
const HEALTH_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const HEALTH_CHECK_PATH: &str = "/grpc.health.v1.Health/Check";
const HEALTH_WATCH_PATH: &str = "/grpc.health.v1.Health/Watch";

/// Registered generated services. Health is attached later, outside the
/// business concurrency limit.
#[derive(Debug, Default)]
pub struct Services {
    routes: tonic::service::RoutesBuilder,
    names: BTreeSet<&'static str>,
}

impl Services {
    /// An empty registry. Health is still served.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `S::NAME` and adds the generated server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidRegistration`] when that name is already registered.
    pub fn add<S>(&mut self, service: S) -> Result<(), Error>
    where
        S: tower::Service<http::Request<tonic::body::Body>, Error = Infallible>
            + tonic::server::NamedService
            + Clone
            + Send
            + Sync
            + 'static,
        S::Response: axum::response::IntoResponse,
        S::Future: Send + 'static,
    {
        if !self.names.insert(S::NAME) {
            return Err(Error::InvalidRegistration);
        }
        self.routes.add_service(service);
        Ok(())
    }
}

/// Serves registered services, standard health, and the gRPC middleware chain.
pub fn router(
    services: Services,
    readiness: ::health::ReadinessReader,
    // template:begin authn:grpc-router-verifier
    verifier: infra_bearerauthn::Verifier,
    // template:end authn:grpc-router-verifier
) -> axum::Router {
    let names = services.names;
    let business = services.routes.routes().into_axum_router().layer(
        tower::ServiceBuilder::new()
            .layer(axum::error_handling::HandleErrorLayer::new(capacity_error))
            .load_shed()
            .layer(tower::limit::GlobalConcurrencyLimitLayer::new(
                BUSINESS_CONCURRENCY,
            ))
            .layer(axum::middleware::from_fn(enforce_deadline))
            .layer(axum::middleware::from_fn(crate::observe::mark_dispatched)),
    );
    let router = route_health(business, readiness, &names);
    // template:begin authn:grpc-router-authenticate
    let router = router.layer(axum::middleware::from_fn(move |request, next| {
        let verifier = verifier.clone();
        async move { authenticate(verifier, request, next).await }
    }));
    // template:end authn:grpc-router-authenticate
    router
        .layer(tower_http::catch_panic::CatchPanicLayer::custom(
            panic_response,
        ))
        .layer(axum::middleware::from_fn(crate::observe::observe))
}

/// Fixed listener options for the gRPC port.
#[must_use]
pub fn server_options() -> infra_http::ServerOptions {
    infra_http::ServerOptions {
        header_read_timeout: Duration::from_secs(5),
        max_header_bytes: 16 * 1024,
        max_connections: NonZeroU32::new(4096),
    }
}

/// Parses `grpc-timeout`. A malformed value, including more than eight digits, is absent.
#[must_use]
pub fn grpc_timeout(headers: &http::HeaderMap) -> Option<Duration> {
    headers
        .get("grpc-timeout")
        .and_then(|value| parse_grpc_timeout(value.as_bytes()))
}

fn parse_grpc_timeout(value: &[u8]) -> Option<Duration> {
    if value.is_empty() || value.len() > 9 {
        return None;
    }
    let (&unit, digits) = value.split_last()?;
    let amount = std::str::from_utf8(digits).ok()?.parse::<u64>().ok()?;
    match unit {
        b'H' => Some(Duration::from_secs(amount.checked_mul(60 * 60)?)),
        b'M' => Some(Duration::from_secs(amount.checked_mul(60)?)),
        b'S' => Some(Duration::from_secs(amount)),
        b'm' => Some(Duration::from_millis(amount)),
        b'u' => Some(Duration::from_micros(amount)),
        b'n' => Some(Duration::from_nanos(amount)),
        _ => None,
    }
}

fn route_health(
    router: axum::Router,
    readiness: ::health::ReadinessReader,
    names: &BTreeSet<&'static str>,
) -> axum::Router {
    let health = tonic_health::pb::health_server::HealthServer::new(crate::health::Adapter::new(
        readiness, names,
    ))
    .max_decoding_message_size(HEALTH_MESSAGE_BYTES)
    .max_encoding_message_size(HEALTH_MESSAGE_BYTES)
    .map_request(|request: http::Request<axum::body::Body>| request.map(tonic::body::Body::new));
    let health = tower::ServiceBuilder::new()
        .layer(axum::middleware::from_fn(crate::observe::mark_dispatched))
        .service(health);
    route_health_service(
        route_health_service(router, HEALTH_CHECK_PATH, health.clone()),
        HEALTH_WATCH_PATH,
        health,
    )
}

#[allow(
    clippy::disallowed_methods,
    reason = "gRPC health is a tonic service on the gRPC listener, outside the business limit, not an OpenAPI route"
)]
fn route_health_service<S>(router: axum::Router, path: &str, service: S) -> axum::Router
where
    S: tower::Service<Request, Error = Infallible> + Clone + Send + Sync + 'static,
    S::Response: axum::response::IntoResponse,
    S::Future: Send + 'static,
{
    router.route_service(path, service)
}

async fn capacity_error(error: axum::BoxError) -> Response {
    let status = if error.is::<tower::load_shed::error::Overloaded>() {
        crate::status::failure_status_with_retry(
            service_failure::Code::ServiceUnavailable,
            SHED_RETRY_AFTER,
        )
    } else {
        tonic::Status::internal(crate::status::REQUEST_FAILED)
    };
    status_response(status)
}

async fn enforce_deadline(request: Request, next: Next) -> Response {
    let budget = grpc_timeout(request.headers())
        .unwrap_or(UNARY_DEADLINE)
        .min(UNARY_DEADLINE);
    match tokio::time::timeout(budget, next.run(request)).await {
        Ok(response) => response,
        Err(_elapsed) => status_response(tonic::Status::deadline_exceeded(
            "request deadline exceeded",
        )),
    }
}

fn panic_response(_panic: Box<dyn std::any::Any + Send>) -> Response {
    status_response(tonic::Status::internal(crate::status::REQUEST_FAILED))
}

pub(crate) fn status_response(status: tonic::Status) -> Response {
    status.into_http()
}

// template:begin authn:grpc-authenticate
async fn authenticate(
    verifier: infra_bearerauthn::Verifier,
    mut request: Request,
    next: Next,
) -> Response {
    if request.uri().path() == HEALTH_CHECK_PATH {
        return next.run(request).await;
    }
    let headers = request
        .headers()
        .get_all(http::header::AUTHORIZATION)
        .iter()
        .map(http::HeaderValue::as_bytes);
    let bearer = match infra_bearerauthn::parse_bearer(headers) {
        Ok(bearer) => bearer,
        Err(failure) => return status_response(authentication_status(failure)),
    };
    let principal = match verifier.verify(&bearer).await {
        Ok(principal) => principal,
        Err(failure) => return status_response(authentication_status(failure)),
    };
    request.extensions_mut().insert(principal);
    request.headers_mut().remove(http::header::AUTHORIZATION);
    next.run(request).await
}

fn authentication_status(failure: infra_bearerauthn::Failure) -> tonic::Status {
    match failure {
        infra_bearerauthn::Failure::Unavailable => {
            tonic::Status::unavailable("authentication is unavailable")
        }
        infra_bearerauthn::Failure::Missing
        | infra_bearerauthn::Failure::Malformed
        | infra_bearerauthn::Failure::Invalid => {
            tonic::Status::unauthenticated("authentication failed")
        }
    }
}
// template:end authn:grpc-authenticate
