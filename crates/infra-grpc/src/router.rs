use std::collections::BTreeSet;
use std::convert::Infallible;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use bytes::Bytes;
use http_body_util::BodyExt as _;
use tokio::sync::Semaphore;
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
    // template:begin authn:grpc-services-scopes-field
    /// Scopes a verified principal needs for a method, keyed by request path.
    scopes: std::collections::HashMap<String, Box<[String]>>,
    // template:end authn:grpc-services-scopes-field
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

    // template:begin authn:grpc-services-require-scopes
    /// Requires every scope in `scopes` for calls to `path`, the method's
    /// request path such as `/example.v1.EchoService/Unary`. A principal that
    /// lacks one gets `PERMISSION_DENIED` before the handler. A method with
    /// no declared requirement admits any authenticated caller, as an OpenAPI
    /// operation without scopes does. The method name itself is not checked:
    /// generated servers do not list their methods.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnregisteredMethodPath`] unless `path` is
    /// `/{service}/{method}` of a service already added, and
    /// [`Error::DuplicateScopeRequirement`] when `path` already has one.
    pub fn require_scopes(&mut self, path: &str, scopes: &[&str]) -> Result<(), Error> {
        let method_of_registered_service = path
            .strip_prefix('/')
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(service, method)| {
                self.names.contains(service) && !method.is_empty() && !method.contains('/')
            });
        if !method_of_registered_service {
            return Err(Error::UnregisteredMethodPath);
        }
        if self.scopes.contains_key(path) {
            return Err(Error::DuplicateScopeRequirement);
        }
        let scopes = scopes.iter().map(|&scope| scope.to_owned()).collect();
        self.scopes.insert(path.to_owned(), scopes);
        Ok(())
    }
    // template:end authn:grpc-services-require-scopes
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
    let business =
        services
            .routes
            .routes()
            .into_axum_router()
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(Semaphore::new(BUSINESS_CONCURRENCY)),
                admit,
            ));
    // template:begin authn:grpc-router-authenticate
    let scopes = Arc::new(services.scopes);
    let business = business.layer(axum::middleware::from_fn(move |request, next| {
        let verifier = verifier.clone();
        let scopes = Arc::clone(&scopes);
        async move { authenticate(verifier, scopes, request, next).await }
    }));
    // template:end authn:grpc-router-authenticate
    with_health(business, readiness, &services.names).layer(axum::middleware::from_fn_with_state(
        Arc::new(crate::observe::Series::server()),
        crate::observe::observe,
    ))
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

const GRPC_TIMEOUT: http::HeaderName = http::HeaderName::from_static("grpc-timeout");

/// Parses `grpc-timeout`. A malformed value, including more than eight digits,
/// is absent. Tonic's own parser is private.
#[must_use]
pub fn grpc_timeout(headers: &http::HeaderMap) -> Option<Duration> {
    headers
        .get(GRPC_TIMEOUT)
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
        .map_response(crate::observe::mark_dispatched)
        .service(HealthServer::new(crate::health::Adapter::new(
            readiness, names,
        )));
    router.route_service(&format!("/{}/{{*rest}}", HealthServer::NAME), health)
}

/// Sheds a business call at the concurrency limit without queueing, and bounds
/// its time to response headers by the deadline. The permit is held until the
/// response headers, as `tower::limit` holds it.
async fn admit(State(limit): State<Arc<Semaphore>>, request: Request, next: Next) -> Response {
    let Ok(_permit) = limit.try_acquire_owned() else {
        crate::observe::record_shed();
        let status = tonic::Status::resource_exhausted(service_failure::AT_CAPACITY_DETAIL);
        return reject(request, status).await;
    };
    let budget = grpc_timeout(request.headers())
        .unwrap_or(CALL_DEADLINE_CAP)
        .min(CALL_DEADLINE_CAP);
    match tokio::time::timeout(budget, next.run(request)).await {
        Ok(response) => crate::observe::mark_dispatched(response),
        Err(_elapsed) => tonic::Status::deadline_exceeded("request deadline exceeded").into_http(),
    }
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
async fn reject(request: Request, status: tonic::Status) -> Response {
    let mut body = request.into_body();
    let _ = tokio::time::timeout(REJECT_DRAIN, async {
        let mut read = 0;
        while let Some(Ok(frame)) = body.frame().await {
            read += frame.data_ref().map_or(0, Bytes::len);
            if read > REJECT_DRAIN_BYTES {
                break;
            }
        }
    })
    .await;
    status.into_http()
}

// template:begin authn:grpc-authenticate
const INSUFFICIENT_SCOPE_DETAIL: &str = "the verified principal lacks the required scope";

async fn authenticate(
    verifier: infra_bearerauthn::Verifier,
    scopes: Arc<std::collections::HashMap<String, Box<[String]>>>,
    mut request: Request,
    next: Next,
) -> Response {
    let authorization = request
        .headers()
        .get_all(http::header::AUTHORIZATION)
        .iter()
        .map(http::HeaderValue::as_bytes);
    let principal = match verifier
        .authenticate(authorization, infra_bearerauthn::Transport::Grpc)
        .await
    {
        Ok(principal) => principal,
        Err(failure) => return reject(request, authentication_status(failure)).await,
    };
    // `principal.scopes()` is sorted and deduplicated.
    let granted = scopes.get(request.uri().path()).is_none_or(|required| {
        required
            .iter()
            .all(|scope| principal.scopes().binary_search(scope).is_ok())
    });
    if !granted {
        let status = tonic::Status::permission_denied(INSUFFICIENT_SCOPE_DETAIL);
        return reject(request, status).await;
    }
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
