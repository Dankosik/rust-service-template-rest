use std::collections::{BTreeSet, HashMap, HashSet};
use std::convert::Infallible;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use bytes::Bytes;
use http_body_util::BodyExt as _;
use prost::Message as _;
use service_failure::{AT_CAPACITY_DETAIL, Code};
use tokio::sync::Semaphore;
use tonic::server::NamedService as _;

use crate::{Error, Failure};

/// Operator limits of the listener and its business calls, from the `grpc`
/// configuration section.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Upper bound for a business call's time to response headers,
    /// authentication included. A caller's shorter `grpc-timeout` wins.
    pub request_timeout: Duration,
    /// Independent bounds on pre-header openings and authenticated calls through
    /// terminal status, shared by router clones. `None` disables both bounds.
    pub max_in_flight: Option<NonZeroU32>,
    /// Accepted connections at once; `None` accepts without a bound.
    pub max_connections: Option<NonZeroU32>,
    /// Age after which a connection gets GOAWAY; `None` sets no age.
    pub max_connection_age: Option<Duration>,
}

type HealthServer = tonic_health::pb::health_server::HealthServer<crate::health::Adapter>;

const REFLECTION_SERVICE: &str = tonic_reflection::pb::v1::server_reflection_server::SERVICE_NAME;

/// Registered generated services and the contracts that describe them.
/// Health is attached later, outside the business concurrency limit and
/// authentication.
#[derive(Debug, Default)]
pub struct Services {
    routes: tonic::service::RoutesBuilder,
    names: BTreeSet<&'static str>,
    /// Method names of every described service, by full service name.
    described: HashMap<String, Vec<String>>,
    /// The sets given to [`Services::describe`], which reflection serves.
    descriptor_sets: Vec<&'static [u8]>,
    reflection: bool,
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

    /// Reads the services and methods of an encoded `FileDescriptorSet`,
    /// such as `grpc_contracts::FILE_DESCRIPTOR_SET`. A generated server does
    /// not list its methods; the set is what lets a scope requirement name a
    /// real method and a metric carry a method label no caller can invent.
    /// Describe a contract before adding its servers.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFileDescriptorSet`] when the bytes do not
    /// decode.
    pub fn describe(&mut self, encoded_file_descriptor_set: &'static [u8]) -> Result<(), Error> {
        self.learn(encoded_file_descriptor_set)?;
        self.descriptor_sets.push(encoded_file_descriptor_set);
        Ok(())
    }

    fn learn(&mut self, encoded_file_descriptor_set: &[u8]) -> Result<(), Error> {
        let set = prost_types::FileDescriptorSet::decode(encoded_file_descriptor_set)
            .map_err(|_| Error::InvalidFileDescriptorSet)?;
        for file in &set.file {
            for service in &file.service {
                let name = match file.package() {
                    "" => service.name().to_owned(),
                    package => format!("{package}.{}", service.name()),
                };
                let methods = service
                    .method
                    .iter()
                    .map(|method| method.name().to_owned())
                    .collect();
                self.described.insert(name, methods);
            }
        }
        Ok(())
    }

    /// Records `S::NAME` and adds the generated server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UndescribedService`] when no described set holds
    /// that name, and [`Error::DuplicateService`] when it is already
    /// registered.
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
        if !self.described.contains_key(S::NAME) {
            return Err(Error::UndescribedService(S::NAME));
        }
        if !self.names.insert(S::NAME) {
            return Err(Error::DuplicateService(S::NAME));
        }
        self.routes.add_service(service);
        Ok(())
    }

    /// Adds standard server reflection over every described set, whenever
    /// it was described, so `grpcurl` and `buf curl` can call the service
    /// without its schema files. Both protocol versions are served,
    /// `grpc.reflection.v1` and the `v1alpha` it replaced, as grpc-go serves
    /// them: older tools ask only for `v1alpha`. Health is described too.
    /// Reflection is a business route: authenticated, limited and
    /// deadline-bound like every other registered service.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DuplicateService`] when reflection is already added.
    pub fn add_reflection(&mut self) -> Result<(), Error> {
        if std::mem::replace(&mut self.reflection, true) {
            return Err(Error::DuplicateService(REFLECTION_SERVICE));
        }
        Ok(())
    }

    /// Builds the reflection servers requested by [`Services::add_reflection`].
    fn attach_reflection(&mut self) -> Result<(), Error> {
        self.learn(tonic_reflection::pb::v1::FILE_DESCRIPTOR_SET)?;
        self.learn(tonic_reflection::pb::v1alpha::FILE_DESCRIPTOR_SET)?;
        // Each version lists the other, as grpc-go's registration does;
        // the builder would add only the one it builds.
        let builder = || {
            let builder = tonic_reflection::server::Builder::configure()
                .include_reflection_service(false)
                .register_encoded_file_descriptor_set(tonic_reflection::pb::v1::FILE_DESCRIPTOR_SET)
                .register_encoded_file_descriptor_set(
                    tonic_reflection::pb::v1alpha::FILE_DESCRIPTOR_SET,
                )
                .register_encoded_file_descriptor_set(tonic_health::pb::FILE_DESCRIPTOR_SET);
            self.descriptor_sets.iter().fold(builder, |builder, set| {
                builder.register_encoded_file_descriptor_set(set)
            })
        };
        let v1 = builder()
            .build_v1()
            .map_err(|_| Error::InvalidFileDescriptorSet)?;
        let v1alpha = builder()
            .build_v1alpha()
            .map_err(|_| Error::InvalidFileDescriptorSet)?;
        self.add(v1)?;
        self.add(v1alpha)
    }

    /// The request path of every described method of the registered
    /// services and of health: the only paths that become metric labels.
    fn method_paths(&self) -> HashSet<Box<str>> {
        self.names
            .iter()
            .copied()
            .chain([HealthServer::NAME])
            .flat_map(|service| {
                let methods = self.described.get(service).into_iter().flatten();
                methods.map(move |method| format!("/{service}/{method}").into_boxed_str())
            })
            .collect()
    }

    // template:begin authn:grpc-services-require-scopes
    /// Requires every scope in `scopes` for calls to `path`, the method's
    /// request path such as `/example.v1.EchoService/Unary`. A principal that
    /// lacks one gets `PERMISSION_DENIED` before the handler. A method with
    /// no declared requirement admits any authenticated caller, as an OpenAPI
    /// operation without scopes does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnregisteredMethodPath`] unless `path` is
    /// `/{service}/{method}` of a described method of a service already
    /// added, and [`Error::DuplicateScopeRequirement`] when `path` already
    /// has one.
    pub fn require_scopes(&mut self, path: &str, scopes: &[&str]) -> Result<(), Error> {
        let method_of_registered_service = path
            .strip_prefix('/')
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(service, method)| {
                self.names.contains(service)
                    && self
                        .described
                        .get(service)
                        .is_some_and(|methods| methods.iter().any(|known| known == method))
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
/// chain. Health is public; business calls are bounded by the deadline,
/// authenticated inside it, and then limited.
///
/// # Errors
///
/// Returns [`Error::InvalidFileDescriptorSet`] when reflection was added and
/// cannot serve the described sets.
pub fn router(
    mut services: Services,
    readiness: ::health::ReadinessReader,
    // template:begin authn:grpc-router-verifier
    verifier: infra_bearerauthn::Verifier,
    // template:end authn:grpc-router-verifier
    limits: Limits,
) -> Result<axum::Router, Error> {
    if services.reflection {
        services.attach_reflection()?;
    }
    services.learn(tonic_health::pb::FILE_DESCRIPTOR_SET)?;
    let series = crate::observe::Series::server(services.method_paths());
    let mut business = services.routes.routes().into_axum_router();
    if let Some(limit) = limits.max_in_flight {
        let permits = Arc::new(Semaphore::new(limit.get() as usize));
        business = business.layer(axum::middleware::from_fn_with_state(permits, shed));
    }
    // template:begin authn:grpc-router-authenticate
    let scopes = Arc::new(services.scopes);
    let business = business.layer(axum::middleware::from_fn(move |request, next| {
        let verifier = verifier.clone();
        let scopes = Arc::clone(&scopes);
        async move { authenticate(verifier, scopes, request, next).await }
    }));
    // template:end authn:grpc-router-authenticate
    let business = if let Some(limit) = limits.max_in_flight {
        let openings = Arc::new(Semaphore::new(limit.get() as usize));
        business.layer(axum::middleware::from_fn_with_state(
            openings,
            admit_opening,
        ))
    } else {
        business
    };
    let business = business.layer(axum::middleware::from_fn_with_state(
        limits.request_timeout,
        deadline,
    ));
    Ok(with_health(business, readiness, &services.names).layer(
        axum::middleware::from_fn_with_state(Arc::new(series), crate::observe::observe),
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
    let health = HealthServer::new(crate::health::Adapter::new(readiness, names));
    router.route_service(&format!("/{}/{{*rest}}", HealthServer::NAME), health)
}

/// Bounds a business call's time to response headers by the caller's
/// `grpc-timeout`, capped at the operator's. It is the outermost business
/// layer, so the time authentication takes is the caller's too.
async fn deadline(State(cap): State<Duration>, request: Request, next: Next) -> Response {
    let origin = tokio::time::Instant::now();
    let caller = grpc_timeout(request.headers());
    let opening = crate::call::Deadline::new(origin, caller.map_or(cap, |asked| asked.min(cap)));
    tokio::select! {
        biased;
        () = opening.wait() => tonic::Status::from(Failure::new(Code::GatewayTimeout)).into_http(),
        response = next.run(request) => {
            if opening.expired() {
                return tonic::Status::from(Failure::new(Code::GatewayTimeout)).into_http();
            }
            let mut response = response;
            if let Some(caller) = caller {
                response.extensions_mut().insert(crate::call::Deadline::new(origin, caller));
            }
            response
        }
    }
}

/// Admission before authentication without a queue. Only the opening future
/// owns this permit; response headers release it independently of call custody.
async fn admit_opening(
    State(permits): State<Arc<Semaphore>>,
    request: Request,
    next: Next,
) -> Response {
    let Ok(_permit) = permits.try_acquire_owned() else {
        crate::observe::record_shed();
        return reject(request, at_capacity()).await;
    };
    next.run(request).await
}

/// Authenticated business admission without a queue. The future owns the
/// permit until headers, then transfers it to the terminal response owner.
async fn shed(State(permits): State<Arc<Semaphore>>, request: Request, next: Next) -> Response {
    let Ok(permit) = permits.try_acquire_owned() else {
        crate::observe::record_shed();
        return reject(request, at_capacity()).await;
    };
    let mut response = next.run(request).await;
    response
        .extensions_mut()
        .insert(crate::call::Permit::new(permit));
    response
}

/// The shed answer. It is `RESOURCE_EXHAUSTED`, so a client that retries
/// `UNAVAILABLE` adds no load, under the identity HTTP's shed answers with.
fn at_capacity() -> tonic::Status {
    Failure::new(Code::ServiceUnavailable)
        .into_status_as(tonic::Code::ResourceExhausted, AT_CAPACITY_DETAIL)
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
        let status = Failure::new(Code::Forbidden)
            .into_status_as(tonic::Code::PermissionDenied, INSUFFICIENT_SCOPE_DETAIL);
        return reject(request, status).await;
    }
    request.extensions_mut().insert(principal);
    request.headers_mut().remove(http::header::AUTHORIZATION);
    next.run(request).await
}

fn authentication_status(failure: infra_bearerauthn::Failure) -> tonic::Status {
    Failure::new(match failure {
        infra_bearerauthn::Failure::Missing => Code::AuthenticationRequired,
        infra_bearerauthn::Failure::Malformed => Code::AuthenticationMalformed,
        infra_bearerauthn::Failure::Invalid => Code::AuthenticationInvalid,
        infra_bearerauthn::Failure::Unavailable => Code::AuthenticationUnavailable,
    })
    .into()
}
// template:end authn:grpc-authenticate
