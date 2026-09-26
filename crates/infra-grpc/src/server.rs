use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http::header::AUTHORIZATION;
use http::{Request, Response};
use hyper::rt::Executor;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tonic::body::Body;
use tonic::service::Routes;
use tower::{Service, ServiceExt as _};

use crate::body::GuardedBody;
use crate::call::CallState;
use crate::health;
use crate::registration::Registry;
use crate::tls::ServerTlsMaterial;
use crate::validation::Validation;
use crate::{Error, Services};

const BUSINESS_LIMIT: usize = 256;
const HEALTH_LIMIT: usize = 4096;
const CONNECTION_LIMIT: usize = 4096;
const MAX_METADATA_BYTES: usize = 16 * 1024;
const UNARY_DEADLINE: Duration = Duration::from_secs(8);
const INITIAL_CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// Explicit listener security.  Plaintext remains a deliberate configuration
/// choice; enabling bearer authentication does not silently upgrade it.
#[derive(Clone, Debug)]
pub enum ServerSecurity {
    Plaintext,
    Tls(ServerTlsMaterial),
}

/// Immutable transport inputs supplied by service composition.
#[derive(Clone, Debug)]
pub struct ServerOptions {
    pub security: ServerSecurity,
    pub effective_drain_budget: Duration,
}

/// Stateless construction entry point.
#[derive(Debug, Default)]
pub struct Server;

/// Validated policy and routes before a socket is bound.
pub struct PreparedServer {
    routes: Routes,
    registry: Registry,
    // template:begin authn:grpc-server-prepared-verifier-field
    verifier: infra_bearerauthn::Verifier,
    // template:end authn:grpc-server-prepared-verifier-field
    options: ServerOptions,
    startup: Arc<HealthState>,
    business_open: Arc<AtomicBool>,
    cancel: CancellationToken,
    tracker: TaskTracker,
}

impl std::fmt::Debug for PreparedServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedServer")
            .field("registered_services", &self.registry.services().count())
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

/// A bound listener whose startup admission is still closed.
#[derive(Debug)]
pub struct BoundServer {
    prepared: PreparedServer,
    listener: TcpListener,
    local_addr: SocketAddr,
}

/// Running listener and all of its owned connection/H2/call tasks.
pub struct RunningServer {
    local_addr: SocketAddr,
    startup: Arc<HealthState>,
    business_open: Arc<AtomicBool>,
    cancel: CancellationToken,
    business: Arc<Semaphore>,
    tracker: TaskTracker,
    accept_task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for RunningServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunningServer")
            .field("local_addr", &self.local_addr)
            .field(
                "admission_open",
                &self.business_open.load(Ordering::Acquire),
            )
            .field("tracked_tasks", &self.tracker.len())
            .finish_non_exhaustive()
    }
}

impl Server {
    /// Validates policy, generated descriptors, authentication mode, and TLS
    /// material.  It has no listener, network I/O, or readiness side effect.
    pub fn prepare(
        services: Services,
        readiness: ::health::ReadinessReader,
        // template:begin authn:grpc-server-prepare-verifier
        verifier: infra_bearerauthn::Verifier,
        // template:end authn:grpc-server-prepare-verifier
        options: ServerOptions,
    ) -> Result<PreparedServer, Error> {
        if options.effective_drain_budget < UNARY_DEADLINE {
            return Err(Error::IncompatibleDrainBudget);
        }
        let (routes, registry) = services.into_parts();
        let startup = Arc::new(HealthState::new());
        let cancel = CancellationToken::new();
        let tracker = TaskTracker::new();
        let routes = routes
            .add_service(health::server(
                readiness.clone(),
                &registry,
                Arc::clone(&startup),
                cancel.child_token(),
                tracker.clone(),
            ))
            .prepare();
        if let ServerSecurity::Tls(material) = &options.security {
            crate::tls::server_config(material)?;
        }
        crate::call::install_panic_hook();
        Ok(PreparedServer {
            routes,
            registry,
            // template:begin authn:grpc-server-prepared-verifier-init
            verifier,
            // template:end authn:grpc-server-prepared-verifier-init
            options,
            startup,
            business_open: Arc::new(AtomicBool::new(false)),
            cancel,
            tracker,
        })
    }
}

impl PreparedServer {
    /// Binds the listener without admitting any business RPCs.
    pub async fn bind(self, address: SocketAddr) -> Result<BoundServer, Error> {
        let listener = TcpListener::bind(address)
            .await
            .map_err(|_| Error::Transport)?;
        let local_addr = listener.local_addr().map_err(|_| Error::Transport)?;
        Ok(BoundServer {
            prepared: self,
            listener,
            local_addr,
        })
    }
}

impl BoundServer {
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Starts the listener while health remains NOT_SERVING until
    /// [`RunningServer::open_admission`] is called.
    pub fn start(self) -> RunningServer {
        let business = Arc::new(Semaphore::new(BUSINESS_LIMIT));
        let health = Arc::new(Semaphore::new(HEALTH_LIMIT));
        let connections = Arc::new(Semaphore::new(CONNECTION_LIMIT));
        let service = Inbound {
            routes: self.prepared.routes,
            registry: self.prepared.registry,
            // template:begin authn:grpc-server-bound-verifier-move
            verifier: self.prepared.verifier,
            // template:end authn:grpc-server-bound-verifier-move
            startup: Arc::clone(&self.prepared.startup),
            business_open: Arc::clone(&self.prepared.business_open),
            business: Arc::clone(&business),
            health,
            cancel: self.prepared.cancel.child_token(),
            tracker: self.prepared.tracker.clone(),
        };
        let accept_task = self.prepared.tracker.spawn(accept_loop(
            self.listener,
            service,
            self.prepared.options.security,
            Arc::clone(&connections),
            self.prepared.cancel.child_token(),
            self.prepared.tracker.clone(),
        ));
        RunningServer {
            local_addr: self.local_addr,
            startup: self.prepared.startup,
            business_open: self.prepared.business_open,
            cancel: self.prepared.cancel,
            business,
            tracker: self.prepared.tracker,
            accept_task: Some(accept_task),
        }
    }
}

impl RunningServer {
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The one startup transition after all service listeners and probes are
    /// ready.  It is monotone until drain begins.
    pub fn open_admission(&self) {
        if !self.cancel.is_cancelled() {
            self.business_open.store(true, Ordering::Release);
            self.startup.set_open(true);
        }
    }

    /// Reject new business work immediately and publish health NOT_SERVING.
    /// Health remains reachable until the later transport drain.
    pub fn begin_drain(&self) {
        self.business_open.store(false, Ordering::Release);
        self.startup.set_open(false);
        self.business.close();
    }

    /// Finishes admitted business bodies or cancels them at the shared process
    /// deadline, then closes listener/connection/H2 task ownership.
    pub async fn drain(&mut self, deadline: Instant) -> Result<(), Error> {
        self.begin_drain();
        while self.business.available_permits() != BUSINESS_LIMIT && Instant::now() < deadline {
            tokio::task::yield_now().await;
        }
        let forced = self.business.available_permits() != BUSINESS_LIMIT;
        let joined = self.join_shutdown(deadline).await;
        if forced {
            Err(Error::DrainTimedOut)
        } else {
            joined
        }
    }

    /// Cancel and join transport tasks within an existing cleanup budget.
    /// A timeout retains task custody in this server for the caller's next
    /// cleanup stage or final runtime shutdown; it never detaches a handle.
    pub async fn join_shutdown(&mut self, deadline: Instant) -> Result<(), Error> {
        self.cancel.cancel();
        self.tracker.close();
        let mut failed = false;
        if let Some(task) = self.accept_task.as_mut() {
            match tokio::time::timeout_at(deadline, &mut *task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failed = !error.is_cancelled(),
                Err(_) => {
                    task.abort();
                    return Err(Error::DrainTimedOut);
                }
            }
            self.accept_task.take();
        }
        tokio::time::timeout_at(deadline, self.tracker.wait())
            .await
            .map_err(|_| Error::DrainTimedOut)?;
        if failed {
            Err(Error::Transport)
        } else {
            Ok(())
        }
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.business_open.store(false, Ordering::Release);
        self.startup.set_open(false);
        self.cancel.cancel();
        self.tracker.close();
        if let Some(task) = self.accept_task.as_ref() {
            task.abort();
        }
    }
}

#[derive(Clone)]
struct Inbound {
    routes: Routes,
    registry: Registry,
    // template:begin authn:grpc-server-inbound-verifier-field
    verifier: infra_bearerauthn::Verifier,
    // template:end authn:grpc-server-inbound-verifier-field
    startup: Arc<HealthState>,
    business_open: Arc<AtomicBool>,
    business: Arc<Semaphore>,
    health: Arc<Semaphore>,
    cancel: CancellationToken,
    tracker: TaskTracker,
}

impl<B> Service<Request<B>> for Inbound
where
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>> + 'static,
{
    type Response = Response<GuardedBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        let path = request.uri().path();
        let health_check = path == "/grpc.health.v1.Health/Check";
        let health_watch = path == "/grpc.health.v1.Health/Watch";
        let known = self.registry.method(path);
        if known.is_none() && !health_check && !health_watch {
            return Box::pin(async {
                Ok(status_response_state(
                    CallState::new(None, None, Validation::empty(), None),
                    tonic::Status::unimplemented(""),
                ))
            });
        }
        let validation = self
            .registry
            .validation(path)
            .unwrap_or_else(Validation::empty);
        let observation =
            known.map(|method| crate::observe::Observation::server(method, request.headers()));
        let permit = if known.is_some() {
            if !self.startup.is_open() || !self.business_open.load(Ordering::Acquire) {
                Err(Error::Stopping)
            } else {
                self.business
                    .clone()
                    .try_acquire_owned()
                    .map_err(|error| match error {
                        tokio::sync::TryAcquireError::Closed => Error::Stopping,
                        tokio::sync::TryAcquireError::NoPermits => Error::AtCapacity,
                    })
            }
        } else {
            self.health
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::AtCapacity)
        };
        let routes = self.routes.clone();
        // template:begin authn:grpc-server-authn-verifier-clone
        let verifier = self.verifier.clone();
        // template:end authn:grpc-server-authn-verifier-clone
        let startup = Arc::clone(&self.startup);
        let business_open = Arc::clone(&self.business_open);
        let cancellation = self.cancel.child_token();
        let tracker = self.tracker.clone();
        let deadline = deadline(&request, known.map(|method| method.cardinality()));
        Box::pin(async move {
            let permit = match permit {
                Ok(permit) => permit,
                Err(error) => {
                    return Ok(status_response_state(
                        CallState::new(None, deadline, validation, observation),
                        error.status(),
                    ));
                }
            };
            let state = CallState::new(Some(permit), deadline, validation, observation);
            if metadata_bytes(request.headers()) > MAX_METADATA_BYTES {
                return Ok(status_response_state(
                    state,
                    Error::MetadataTooLarge.status(),
                ));
            }
            if known.is_some() && (!startup.is_open() || !business_open.load(Ordering::Acquire)) {
                return Ok(status_response_state(state, Error::Stopping.status()));
            }
            request
                .extensions_mut()
                .insert(state.cancellation().child_token());
            if let Some(deadline) = deadline {
                request
                    .extensions_mut()
                    .insert(crate::Operation { deadline });
            }

            // Declared before the exchange so peer cancellation drops all
            // authentication/handler work before this guard releases admission.
            let mut initial = InitialCall(Some(Arc::clone(&state)));
            let response = {
                let exchange = async {
                    // Public Health/Check ignores supplied credentials. Other
                    // known methods authenticate once under the call deadline.
                    // template:begin authn:grpc-server-authn-verify
                    if !health_check {
                        let headers = request
                            .headers()
                            .get_all(AUTHORIZATION)
                            .iter()
                            .map(http::HeaderValue::as_bytes);
                        let bearer = infra_bearerauthn::parse_bearer(headers)
                            .map_err(authentication_status)?;
                        let principal = verifier
                            .verify(&bearer)
                            .await
                            .map_err(authentication_status)?;
                        request.extensions_mut().insert(principal);
                    }
                    // template:end authn:grpc-server-authn-verify
                    request.headers_mut().remove(AUTHORIZATION);
                    match crate::call::recover(routes.oneshot(request)).await {
                        Ok(Ok(response)) => Ok(response),
                        Ok(Err(never)) => match never {},
                        Err(()) => Err(crate::status::panic_status()),
                    }
                };
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => Err(tonic::Status::cancelled("request cancelled")),
                    () = wait_deadline(deadline) => Err(tonic::Status::deadline_exceeded("request deadline exceeded")),
                    response = CallState::scope(Arc::clone(&state), exchange) => response,
                }
            };
            // The losing initial future is gone. Move the actual body into the
            // abort slot before the independent deadline waiter gains authority.
            let response = match (state.validation_failure(), response) {
                (Some(status), response) => {
                    drop(response);
                    status_response_state(Arc::clone(&state), status)
                }
                (None, Ok(response)) => guarded_response(Arc::clone(&state), response),
                (None, Err(status)) => status_response_state(Arc::clone(&state), status),
            };
            initial.0.take();
            let completed = state.cancellation();
            if !completed.is_cancelled() {
                tracker.spawn(async move {
                    tokio::select! {
                        biased;
                        () = completed.cancelled() => {},
                        () = cancellation.cancelled() => { state.finish_with_status(tonic::Status::cancelled("request cancelled")); },
                        () = wait_deadline(deadline) => state.finish_with_deadline(),
                    }
                });
            }
            Ok(response)
        })
    }
}

struct InitialCall(Option<Arc<CallState>>);

impl Drop for InitialCall {
    fn drop(&mut self) {
        if let Some(state) = self.0.take() {
            state.finish_with_status(tonic::Status::cancelled("request cancelled"));
        }
    }
}

async fn wait_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

// template:begin authn:grpc-server-authentication-status
fn authentication_status(failure: infra_bearerauthn::Failure) -> tonic::Status {
    match failure {
        infra_bearerauthn::Failure::Malformed => {
            tonic::Status::invalid_argument("authentication failed")
        }
        infra_bearerauthn::Failure::Unavailable => {
            tonic::Status::unavailable("authentication is unavailable")
        }
        infra_bearerauthn::Failure::Missing | infra_bearerauthn::Failure::Invalid => {
            tonic::Status::unauthenticated("authentication failed")
        }
    }
}
// template:end authn:grpc-server-authentication-status

pub(crate) struct HealthState {
    open: AtomicBool,
    changed: watch::Sender<()>,
}

impl HealthState {
    fn new() -> Self {
        let (changed, _) = watch::channel(());
        Self {
            open: AtomicBool::new(false),
            changed,
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    fn set_open(&self, open: bool) {
        let changed = self.open.swap(open, Ordering::AcqRel) != open;
        if changed {
            self.changed.send_replace(());
        }
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }
}

fn status_response_state(state: Arc<CallState>, status: tonic::Status) -> Response<GuardedBody> {
    guarded_response(state, status.into_http::<Body>())
}

fn guarded_response(state: Arc<CallState>, response: Response<Body>) -> Response<GuardedBody> {
    let (parts, body) = response.into_parts();
    let initial_status = crate::observe::status_code(&parts.headers);
    Response::from_parts(parts, GuardedBody::new(state, body, initial_status))
}

fn metadata_bytes(headers: &http::HeaderMap) -> usize {
    headers.iter().fold(0usize, |total, (name, value)| {
        total
            .saturating_add(name.as_str().len())
            .saturating_add(value.as_bytes().len())
    })
}

fn deadline<B>(request: &Request<B>, cardinality: Option<crate::Cardinality>) -> Option<Instant> {
    let caller = request
        .headers()
        .get("grpc-timeout")
        .and_then(|value| parse_grpc_timeout(value.as_bytes()));
    match cardinality {
        Some(crate::Cardinality::Unary) => Some(
            caller
                .map(|duration| Instant::now() + duration)
                .unwrap_or_else(|| Instant::now() + UNARY_DEADLINE)
                .min(Instant::now() + UNARY_DEADLINE),
        ),
        Some(_) | None => caller.map(|duration| Instant::now() + duration),
    }
}

fn parse_grpc_timeout(value: &[u8]) -> Option<Duration> {
    if value.is_empty() || value.len() > 9 {
        return None;
    }
    let (&unit, digits) = value.split_last()?;
    let amount = std::str::from_utf8(digits).ok()?.parse::<u64>().ok()?;
    let duration = match unit {
        b'H' => Duration::from_secs(amount.checked_mul(60 * 60)?),
        b'M' => Duration::from_secs(amount.checked_mul(60)?),
        b'S' => Duration::from_secs(amount),
        b'm' => Duration::from_millis(amount),
        b'u' => Duration::from_micros(amount),
        b'n' => Duration::from_nanos(amount),
        _ => return None,
    };
    Some(duration)
}

#[derive(Clone)]
struct TrackedExecutor {
    cancel: CancellationToken,
    tracker: TaskTracker,
}

impl<F> Executor<F> for TrackedExecutor
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, future: F) {
        if self.cancel.is_cancelled() || self.tracker.is_closed() {
            return;
        }
        let cancel = self.cancel.child_token();
        self.tracker.spawn(async move {
            tokio::select! {
                () = cancel.cancelled() => {},
                _ = future => {},
            }
        });
    }
}

async fn accept_loop(
    listener: TcpListener,
    service: Inbound,
    security: ServerSecurity,
    connections: Arc<Semaphore>,
    cancel: CancellationToken,
    tracker: TaskTracker,
) {
    let mut tasks = JoinSet::new();
    let tls = match security {
        ServerSecurity::Plaintext => None,
        ServerSecurity::Tls(material) => match crate::tls::server_config(&material) {
            Ok(config) => Some(TlsAcceptor::from(config)),
            Err(_) => return,
        },
    };
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            Some(_done) = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = listener.accept() => {
                let Ok((stream, _peer)) = accepted else { continue; };
                let Ok(permit) = connections.clone().try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let service = service.clone();
                let cancel = cancel.child_token();
                let tracker = tracker.clone();
                let tls = tls.clone();
                tasks.spawn(tracker.clone().track_future(async move {
                    let _permit = permit;
                    match tls {
                        Some(acceptor) => match tokio::time::timeout(INITIAL_CONNECTION_TIMEOUT, acceptor.accept(stream)).await {
                            Ok(Ok(stream)) => serve_connection(stream, service, cancel, tracker).await,
                            Err(_) => {}
                            Ok(Err(_)) => {}
                        },
                        None => {
                            if wait_for_first_byte(&stream).await {
                                serve_connection(stream, service, cancel, tracker).await;
                            }
                        }
                    }
                }));
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

async fn wait_for_first_byte(stream: &TcpStream) -> bool {
    let mut first = [0u8; 1];
    matches!(
        tokio::time::timeout(INITIAL_CONNECTION_TIMEOUT, stream.peek(&mut first)).await,
        Ok(Ok(read)) if read > 0
    )
}

async fn serve_connection<IO>(
    stream: IO,
    service: Inbound,
    cancel: CancellationToken,
    tracker: TaskTracker,
) where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let executor = TrackedExecutor {
        cancel: cancel.child_token(),
        tracker,
    };
    let mut builder = hyper::server::conn::http2::Builder::new(executor);
    builder
        .max_concurrent_streams(100)
        .max_header_list_size(MAX_METADATA_BYTES as u32);
    let connection =
        builder.serve_connection(TokioIo::new(stream), TowerToHyperService::new(service));
    tokio::select! {
        () = cancel.cancelled() => {},
        _ = connection => {},
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "native health fixture failures carry setup context"
)]
mod tests {
    use super::*;
    use http_body_util::BodyExt as _;
    use prost::Message as _;
    use tonic_health::pb::HealthCheckRequest;

    #[tokio::test]
    async fn dropping_known_or_unknown_watch_joins_its_task_without_process_cancellation() {
        for service in ["", "unknown"] {
            let readiness = ::health::Readiness::new(Vec::new());
            let cancel = CancellationToken::new();
            let tracker = TaskTracker::new();
            let server = health::server(
                readiness.reader(),
                &Registry::default(),
                Arc::new(HealthState::new()),
                cancel.child_token(),
                tracker.clone(),
            );
            let message = HealthCheckRequest {
                service: service.to_owned(),
            }
            .encode_to_vec();
            let mut encoded = vec![0];
            encoded.extend_from_slice(
                &u32::try_from(message.len())
                    .expect("small health message")
                    .to_be_bytes(),
            );
            encoded.extend_from_slice(&message);
            let request = Request::builder()
                .uri("/grpc.health.v1.Health/Watch")
                .header("content-type", "application/grpc")
                .body(Body::new(http_body_util::Full::new(Bytes::from(encoded))))
                .expect("valid health request");
            let mut response = server
                .oneshot(request)
                .await
                .expect("native health dispatch");
            let frame = tokio::time::timeout(Duration::from_secs(1), response.body_mut().frame())
                .await
                .expect("initial watch response")
                .expect("watch produces an initial message")
                .expect("watch message encodes");
            assert!(frame.is_data());
            assert_eq!(tracker.len(), 1);
            drop(response);
            tracker.close();
            let joined = tokio::time::timeout(Duration::from_secs(1), tracker.wait())
                .await
                .is_ok();
            // Also clean up against the defective implementation before the assertion.
            cancel.cancel();
            tokio::time::timeout(Duration::from_secs(1), tracker.wait())
                .await
                .expect("watch cleanup joins");
            assert!(
                joined,
                "dropping Watch must end its task independently of process shutdown"
            );
        }
    }
}
