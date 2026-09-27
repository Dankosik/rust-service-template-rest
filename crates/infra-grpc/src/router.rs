use std::collections::BTreeSet;
use std::convert::Infallible;
use std::num::NonZeroU32;
use std::time::Duration;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use tonic::server::NamedService as _;

use crate::Error;

/// Upper bound for the time to response headers of any business call, and the
/// floor for the process drain budget.
pub const CALL_DEADLINE_CAP: Duration = Duration::from_secs(8);

const BUSINESS_CONCURRENCY: usize = 256;

type HealthServer = tonic_health::pb::health_server::HealthServer<crate::health::Adapter>;

/// Registered generated services. Health is attached later, outside the
/// business concurrency limit and authentication.
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
    /// Returns [`Error::DuplicateService`] when that name is already registered.
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
            return Err(Error::DuplicateService(S::NAME));
        }
        self.routes.add_service(service);
        Ok(())
    }
}

/// Serves registered services and standard health behind the gRPC middleware
/// chain. Health is public; business calls are authenticated, limited, and
/// bounded by the deadline.
pub fn router(
    services: Services,
    readiness: ::health::ReadinessReader,
    // template:begin authn:grpc-router-verifier
    verifier: infra_bearerauthn::Verifier,
    // template:end authn:grpc-router-verifier
) -> axum::Router {
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
    // template:begin authn:grpc-router-authenticate
    let business = business.layer(axum::middleware::from_fn(move |request, next| {
        let verifier = verifier.clone();
        async move { authenticate(verifier, request, next).await }
    }));
    // template:end authn:grpc-router-authenticate
    with_health(business, readiness, &services.names)
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

/// Parses `grpc-timeout`. A malformed value, including more than eight digits,
/// is absent. Tonic's own parser is private.
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

/// Adds health the way tonic `Routes` adds a service: one `/{NAME}/{*rest}` route.
#[allow(
    clippy::disallowed_methods,
    reason = "gRPC health is a tonic service on the gRPC listener, outside the business limit, not an OpenAPI route"
)]
fn with_health(
    router: axum::Router,
    readiness: ::health::ReadinessReader,
    names: &BTreeSet<&'static str>,
) -> axum::Router {
    let health = tower::ServiceBuilder::new()
        .layer(axum::middleware::from_fn(crate::observe::mark_dispatched))
        .service(HealthServer::new(crate::health::Adapter::new(
            readiness, names,
        )));
    router.route_service(&format!("/{}/{{*rest}}", HealthServer::NAME), health)
}

async fn capacity_error(error: axum::BoxError) -> Response {
    if error.is::<tower::load_shed::error::Overloaded>() {
        crate::observe::record_shed();
        return tonic::Status::resource_exhausted(service_failure::AT_CAPACITY_DETAIL).into_http();
    }
    tonic::Status::internal("request failed").into_http()
}

async fn enforce_deadline(request: Request, next: Next) -> Response {
    let budget = grpc_timeout(request.headers())
        .unwrap_or(CALL_DEADLINE_CAP)
        .min(CALL_DEADLINE_CAP);
    match tokio::time::timeout(budget, next.run(request)).await {
        Ok(response) => response,
        Err(_elapsed) => tonic::Status::deadline_exceeded("request deadline exceeded").into_http(),
    }
}

fn panic_response(_panic: Box<dyn std::any::Any + Send>) -> Response {
    tonic::Status::internal("request failed").into_http()
}

// template:begin authn:grpc-authenticate
async fn authenticate(
    verifier: infra_bearerauthn::Verifier,
    mut request: Request,
    next: Next,
) -> Response {
    let authorization = request
        .headers()
        .get_all(http::header::AUTHORIZATION)
        .iter()
        .map(http::HeaderValue::as_bytes);
    let principal = match verifier.authenticate(authorization, "grpc").await {
        Ok(principal) => principal,
        Err(failure) => return authentication_status(failure).into_http(),
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
