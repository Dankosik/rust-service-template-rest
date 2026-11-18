use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::mem::DropGuard;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::{Extension, Router};
use hyper::rt::Executor;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use shared_rules::hyper::HTTP1_MIN_READ_BUFFER_BYTES;
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;
use tokio_util::task::{AbortOnDropHandle, TaskTracker};
use tower::ServiceBuilder;

pub(crate) const CONNECTIONS_REFUSED_METRIC: &str = "http_server_connections_refused_total";

const LISTEN_BACKLOG: u32 = 65_535;

#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
const ACCEPTED_TCP_USER_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug)]
pub struct Http1Options {
    pub header_read_timeout: Duration,
    pub max_read_buffer_bytes: usize,
    pub connections: ConnectionLimits,
}

#[derive(Clone, Copy, Debug)]
pub struct ConnectionLimits {
    pub max_connections: Option<NonZeroU32>,
    pub max_connection_age: Option<Duration>,
}

// template:begin grpc:http2-listener-options
#[derive(Clone, Copy, Debug)]
pub struct Http2Options {
    /// Time to read the 24-octet client preface magic in RFC 9113 §3.4, before the SETTINGS frame.
    pub preface_timeout: Duration,
    pub max_header_list_size: u32,
    pub connections: ConnectionLimits,
}

#[derive(Clone, Debug)]
pub struct TlsOptions {
    pub config: Arc<rustls::ServerConfig>,
    pub handshake_timeout: Duration,
}
// template:end grpc:http2-listener-options

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("bind http listener {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("http accept loop task: {0}")]
    AcceptTask(#[source] tokio::task::JoinError),
    #[error("http accept loop termination was not confirmed before the drain deadline")]
    AcceptTimeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptFailure {
    Ended,
    Panicked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drained {
    Complete,
    TimedOut { remaining_connections: usize },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainResidue {
    pub accept_unconfirmed: bool,
    pub remaining_connections: usize,
    pub remaining_delegated_tasks: usize,
}

#[derive(Debug)]
pub struct Server {
    local_addr: SocketAddr,
    accept_loop: AbortOnDropHandle<()>,
    accept_failure: watch::Receiver<Option<AcceptFailure>>,
    connection_shutdown: ConnectionShutdown,
    connections: TaskTracker,
    delegated: ListenerExecutor,
}

#[derive(Clone, Debug)]
struct ConnectionShutdown {
    finish: CancellationToken,
    force: CancellationToken,
}

#[derive(Clone, Debug)]
pub struct ConnectionInfo {
    scheme: &'static str,
    forced: Arc<AtomicBool>,
    drain: CancellationToken,
}

impl ConnectionInfo {
    #[must_use]
    pub fn child_drain_token(&self) -> CancellationToken {
        self.drain.child_token()
    }

    #[must_use]
    pub const fn scheme(&self) -> &'static str {
        self.scheme
    }

    #[must_use]
    pub fn was_forced(&self) -> bool {
        self.forced.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
pub struct BoundListener {
    listener: TcpListener,
    local_addr: SocketAddr,
}

impl BoundListener {
    pub fn bind(addr: SocketAddr) -> Result<Self, ServerError> {
        let listener = listen(addr).map_err(|source| ServerError::Bind { addr, source })?;
        let local_addr = listener
            .local_addr()
            .map_err(|source| ServerError::Bind { addr, source })?;
        Ok(Self {
            listener,
            local_addr,
        })
    }

    #[must_use]
    pub fn serve(self, app: Router, options: Http1Options) -> Server {
        serve_plaintext(self, app, options.connections, http1_driver(options))
    }

    // template:begin grpc:http-bound-listener-tls
    #[must_use]
    pub fn serve_tls(self, app: Router, options: Http1Options, tls: TlsOptions) -> Server {
        serve_tls(self, app, options.connections, http1_driver(options), tls)
    }

    #[must_use]
    pub fn serve_h2(self, app: Router, options: Http2Options, tls: Option<TlsOptions>) -> Server {
        let driver = http2_driver(options);
        match tls {
            Some(tls) => serve_tls(self, app, options.connections, driver, tls),
            None => serve_plaintext(self, app, options.connections, driver),
        }
    }
    // template:end grpc:http-bound-listener-tls
}

fn serve_plaintext(
    bound: BoundListener,
    app: Router,
    limits: ConnectionLimits,
    driver: ConnectionDriver,
) -> Server {
    serve_listener(bound, app, limits, driver, |stream| async move {
        Some((stream, "http"))
    })
}

// template:begin grpc:http-serve-tls
fn serve_tls(
    bound: BoundListener,
    app: Router,
    limits: ConnectionLimits,
    driver: ConnectionDriver,
    tls: TlsOptions,
) -> Server {
    let TlsOptions {
        mut config,
        handshake_timeout,
    } = tls;
    let require_h2 = driver.builder.is_http2_available();
    let protocol: &[u8] = if require_h2 { b"h2" } else { b"http/1.1" };
    if config.alpn_protocols.as_slice() != [protocol] {
        Arc::make_mut(&mut config).alpn_protocols = vec![protocol.to_vec()];
    }
    let acceptor = tokio_rustls::TlsAcceptor::from(config);
    serve_listener(bound, app, limits, driver, move |stream| {
        let acceptor = acceptor.clone();
        async move {
            let peer = stream.peer_addr().ok();
            match tokio::time::timeout(handshake_timeout, acceptor.accept(stream)).await {
                Ok(Ok(tls_stream)) => {
                    if require_h2 && tls_stream.get_ref().1.alpn_protocol() != Some(b"h2") {
                        tracing::debug!(
                            peer = peer.map(tracing::field::display),
                            "http2_alpn_missing"
                        );
                        return None;
                    }
                    Some((tls_stream, "https"))
                }
                Ok(Err(error)) => {
                    tracing::debug!(
                        peer = peer.map(tracing::field::display),
                        %error,
                        "http_tls_handshake_failed"
                    );
                    None
                }
                Err(_) => {
                    tracing::debug!(
                        peer = peer.map(tracing::field::display),
                        "http_tls_handshake_timed_out"
                    );
                    None
                }
            }
        }
    })
}
// template:end grpc:http-serve-tls

fn listen(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let socket = match addr {
        SocketAddr::V4(_) => TcpSocket::new_v4(),
        SocketAddr::V6(_) => TcpSocket::new_v6(),
    }?;
    #[cfg(not(windows))]
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    socket.listen(LISTEN_BACKLOG)
}

fn configure_accepted(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    let _ = socket2::SockRef::from(stream).set_tcp_user_timeout(Some(ACCEPTED_TCP_USER_TIMEOUT));
}

impl Server {
    pub fn bind(addr: SocketAddr, app: Router, options: Http1Options) -> Result<Self, ServerError> {
        Ok(BoundListener::bind(addr)?.serve(app, options))
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn accept_failure(&self) -> impl Future<Output = AcceptFailure> + Send + 'static + use<> {
        let mut failure = self.accept_failure.clone();
        async move {
            let seen = failure
                .wait_for(Option::is_some)
                .await
                .ok()
                .and_then(|seen| *seen);
            let Some(failure) = seen else {
                return std::future::pending().await;
            };
            failure
        }
    }

    pub async fn drain(self, budget: Duration) -> Result<Drained, ServerError> {
        let deadline = deadline_after(budget);
        self.drain_until(deadline, deadline).await.0
    }

    pub async fn drain_until(
        mut self,
        graceful_deadline: Instant,
        confirmation_deadline: Instant,
    ) -> (Result<Drained, ServerError>, DrainResidue) {
        self.connection_shutdown.finish.cancel();
        let joined = self.join_accept_until(graceful_deadline).await;
        let mut accept_unconfirmed = joined.is_none();
        let cause = match joined {
            None => {
                self.accept_loop.abort();
                Err(ServerError::AcceptTimeout)
            }
            Some(Err(error)) => Err(ServerError::AcceptTask(error)),
            Some(Ok(())) => {
                if self.wait_for_work(graceful_deadline).await {
                    return (Ok(Drained::Complete), DrainResidue::default());
                }
                Ok(Drained::TimedOut {
                    remaining_connections: self.connections.len(),
                })
            }
        };
        self.connection_shutdown.force.cancel();
        if accept_unconfirmed {
            accept_unconfirmed = self
                .join_accept_until(confirmation_deadline)
                .await
                .is_none();
        }
        let producers_stopped =
            !accept_unconfirmed && close_and_wait(&self.connections, confirmation_deadline).await;
        self.delegated.force.cancel();
        if producers_stopped {
            close_and_wait(&self.delegated.tasks, confirmation_deadline).await;
        }
        let residue = DrainResidue {
            accept_unconfirmed,
            remaining_connections: self.connections.len(),
            remaining_delegated_tasks: self.delegated.tasks.len(),
        };
        (cause, residue)
    }

    async fn join_accept_until(
        &mut self,
        deadline: Instant,
    ) -> Option<Result<(), tokio::task::JoinError>> {
        if !self.accept_loop.is_finished() && deadline <= Instant::now() {
            return None;
        }
        timeout_at(deadline, &mut self.accept_loop).await.ok()
    }

    async fn wait_for_work(&self, deadline: Instant) -> bool {
        close_and_wait(&self.connections, deadline).await
            && close_and_wait(&self.delegated.tasks, deadline).await
    }
}

async fn close_and_wait(tasks: &TaskTracker, deadline: Instant) -> bool {
    tasks.close();
    tasks.is_empty()
        || (deadline > Instant::now() && timeout_at(deadline, tasks.wait()).await.is_ok())
}

fn deadline_after(budget: Duration) -> Instant {
    const FAR_FUTURE: Duration = Duration::from_hours(24 * 365 * 30);
    let now = Instant::now();
    now.checked_add(budget).unwrap_or(now + FAR_FUTURE)
}

impl Drop for Server {
    fn drop(&mut self) {
        self.connection_shutdown.finish.cancel();
        self.connection_shutdown.force.cancel();
        self.delegated.force.cancel();
    }
}

#[derive(Clone, Debug, Default)]
struct ListenerExecutor {
    tasks: TaskTracker,
    force: CancellationToken,
}

impl<F: Future + Send + 'static> Executor<F> for ListenerExecutor {
    fn execute(&self, future: F) {
        let force = self.force.clone();
        let task = self.tasks.track_future(async move {
            tokio::select! {
                biased;
                () = force.cancelled() => {},
                _ = future => {},
            }
        });
        TokioExecutor::new().execute(task);
    }
}

#[derive(Clone)]
struct ConnectionDriver {
    builder: auto::Builder<ListenerExecutor>,
    executor: ListenerExecutor,
    // template:begin grpc:http2-driver-preface-budget
    preface_timeout: Option<Duration>,
    // template:end grpc:http2-driver-preface-budget
}

// template:begin grpc:http2-wire-limits
const HTTP2_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(20);
const HTTP2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);

const HTTP2_CONNECTION_WINDOW: u32 = 8 * 1024 * 1024;
const HTTP2_STREAM_WINDOW: u32 = 2 * 1024 * 1024;

const HTTP2_MAX_CONCURRENT_STREAMS: u32 = 4096;
// template:end grpc:http2-wire-limits

fn http1_driver(options: Http1Options) -> ConnectionDriver {
    let executor = ListenerExecutor::default();
    let mut builder = auto::Builder::new(executor.clone());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(options.header_read_timeout)
        .max_buf_size(
            options
                .max_read_buffer_bytes
                .max(usize::from(HTTP1_MIN_READ_BUFFER_BYTES)),
        )
        .keep_alive(true);
    ConnectionDriver {
        builder: builder.http1_only(),
        executor,
        // template:begin grpc:http1-driver-no-preface
        preface_timeout: None,
        // template:end grpc:http1-driver-no-preface
    }
}

// template:begin grpc:http2-driver
fn http2_driver(options: Http2Options) -> ConnectionDriver {
    let executor = ListenerExecutor::default();
    let mut builder = auto::Builder::new(executor.clone());
    builder
        .http2()
        .timer(TokioTimer::new())
        .max_header_list_size(options.max_header_list_size)
        .keep_alive_interval(Some(HTTP2_KEEP_ALIVE_INTERVAL))
        .keep_alive_timeout(HTTP2_KEEP_ALIVE_TIMEOUT)
        .initial_connection_window_size(HTTP2_CONNECTION_WINDOW)
        .initial_stream_window_size(HTTP2_STREAM_WINDOW)
        .max_concurrent_streams(HTTP2_MAX_CONCURRENT_STREAMS);
    ConnectionDriver {
        builder: builder.http2_only(),
        executor,
        preface_timeout: Some(options.preface_timeout),
    }
}
// template:end grpc:http2-driver

fn serve_listener<F, Fut, IO>(
    bound: BoundListener,
    app: Router,
    limits: ConnectionLimits,
    driver: ConnectionDriver,
    prepare_io: F,
) -> Server
where
    F: Fn(TcpStream) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Option<(IO, &'static str)>> + Send + 'static,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let local_addr = bound.local_addr;
    let connection_shutdown = ConnectionShutdown {
        finish: CancellationToken::new(),
        force: CancellationToken::new(),
    };
    let connections = TaskTracker::new();
    metrics::describe_counter!(
        CONNECTIONS_REFUSED_METRIC,
        metrics::Unit::Count,
        "Accepted connections closed without a response because the connection cap was reached."
    );
    if limits.max_connections.is_some() {
        metrics::counter!(CONNECTIONS_REFUSED_METRIC).increment(0);
    }
    let (failure, accept_failure) = watch::channel(None);
    let guard = DropGuard::new(
        (connection_shutdown.finish.clone(), failure),
        |(finish, failure)| {
            let observed = if std::thread::panicking() {
                Some(AcceptFailure::Panicked)
            } else if !finish.is_cancelled() {
                Some(AcceptFailure::Ended)
            } else {
                None
            };
            if let Some(observed) = observed {
                failure.send_replace(Some(observed));
            }
        },
    );
    let delegated = driver.executor.clone();
    let accept = accept_loop(
        bound,
        app,
        limits,
        driver,
        connection_shutdown.clone(),
        connections.clone(),
        prepare_io,
    );
    let accept_loop = AbortOnDropHandle::new(tokio::spawn(async move {
        let _guard = guard;
        accept.await;
    }));
    Server {
        local_addr,
        accept_loop,
        accept_failure,
        connection_shutdown,
        connections,
        delegated,
    }
}

async fn accept_loop<F, Fut, IO>(
    BoundListener {
        listener,
        local_addr,
    }: BoundListener,
    app: Router,
    limits: ConnectionLimits,
    driver: ConnectionDriver,
    connection_shutdown: ConnectionShutdown,
    connections: TaskTracker,
    prepare_io: F,
) where
    F: Fn(TcpStream) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Option<(IO, &'static str)>> + Send + 'static,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let finish = &connection_shutdown.finish;
    let permits = limits
        .max_connections
        .map(|limit| Arc::new(Semaphore::new(limit.get() as usize)));

    loop {
        let accepted = tokio::select! {
            () = finish.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(err) => {
                let delay = accept_retry_delay(&err);
                if delay.is_zero() {
                    continue;
                }
                tracing::warn!(error = %err, "http_accept_failed");
                tokio::select! {
                    () = finish.cancelled() => break,
                    () = tokio::time::sleep(delay) => {}
                }
                continue;
            }
        };
        let permit = match permits.as_ref().map(|p| p.clone().try_acquire_owned()) {
            None => None,
            Some(Ok(permit)) => Some(permit),
            Some(Err(_)) => {
                metrics::counter!(CONNECTIONS_REFUSED_METRIC).increment(1);
                tracing::debug!(%local_addr, %peer, "http_connection_refused");
                drop(stream);
                continue;
            }
        };
        configure_accepted(&stream);
        connections.spawn(serve_connection(
            (stream, peer),
            permit,
            prepare_io.clone(),
            driver.clone(),
            app.clone(),
            limits,
            connection_shutdown.clone(),
        ));
    }
    drop(listener);
}

fn accept_retry_delay(error: &std::io::Error) -> Duration {
    use std::io::ErrorKind::{ConnectionAborted, ConnectionRefused, ConnectionReset, Interrupted};
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use nix::errno::Errno::{
            self, EHOSTDOWN, EHOSTUNREACH, ENETDOWN, ENETUNREACH, ENONET, ENOPROTOOPT, EOPNOTSUPP,
            EPROTO,
        };
        // Linux accept(2), Error handling.
        if matches!(
            error.raw_os_error().map(Errno::from_raw),
            Some(
                ENETDOWN
                    | EPROTO
                    | ENOPROTOOPT
                    | EHOSTDOWN
                    | ENONET
                    | EHOSTUNREACH
                    | EOPNOTSUPP
                    | ENETUNREACH
            )
        ) {
            return Duration::ZERO;
        }
    }
    match error.kind() {
        ConnectionAborted | ConnectionRefused | ConnectionReset | Interrupted => Duration::ZERO,
        _ => Duration::from_millis(50),
    }
}

fn serve_connection<F, Fut, IO>(
    (stream, peer): (TcpStream, SocketAddr),
    permit: Option<OwnedSemaphorePermit>,
    prepare_io: F,
    driver: ConnectionDriver,
    app: Router,
    limits: ConnectionLimits,
    shutdown: ConnectionShutdown,
) -> impl Future<Output = ()>
where
    F: Fn(TcpStream) -> Fut,
    Fut: Future<Output = Option<(IO, &'static str)>>,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let age_deadline = limits
        .max_connection_age
        .map(|age| deadline_after(spread(age)));
    async move {
        let _permit = permit;
        let ConnectionShutdown { finish, force } = shutdown;
        let forced = Arc::new(AtomicBool::new(false));
        let connection_forced = Arc::clone(&forced);
        let serve = async move {
            let closing = wind_down(&finish, age_deadline);
            tokio::pin!(closing);
            let io = tokio::select! {
                biased;
                () = closing.as_mut() => None,
                io = prepare_io(stream) => io,
            };
            let Some((io, scheme)) = io else {
                return Ok(());
            };
            // template:begin grpc:http2-observe-initial-read
            let (io, preface) = if let Some(timeout) = driver.preface_timeout {
                let (notify, received) = tokio::sync::oneshot::channel();
                let mut notify = Some(notify);
                // RFC 9113 §3.4, client connection preface magic.
                let mut remaining = 24_usize;
                let io = tokio_util::io::InspectReader::new(io, move |bytes| {
                    remaining = remaining.saturating_sub(bytes.len());
                    if remaining == 0
                        && let Some(notify) = notify.take()
                    {
                        let _ = notify.send(());
                    }
                });
                (
                    tokio_util::either::Either::Left(io),
                    Some((deadline_after(timeout), received)),
                )
            } else {
                (tokio_util::either::Either::Right(io), None)
            };
            // template:end grpc:http2-observe-initial-read
            let service = ServiceBuilder::new()
                .layer(Extension(ConnectionInfo {
                    scheme,
                    forced: connection_forced,
                    drain: finish.clone(),
                }))
                .service(app);
            let connection = driver
                .builder
                .serve_connection(TokioIo::new(io), TowerToHyperService::new(service));
            tokio::pin!(connection);
            // template:begin grpc:http2-wait-initial-read
            if let Some((deadline, received)) = preface {
                tokio::select! {
                    biased;
                    _ = received => {}
                    () = closing.as_mut() => return Ok(()),
                    () = tokio::time::sleep_until(deadline) => {
                        tracing::debug!(%peer, "http2_preface_timed_out");
                        return Ok(());
                    }
                    ended = connection.as_mut() => return ended,
                }
            }
            // template:end grpc:http2-wait-initial-read
            tokio::select! {
                biased;
                () = closing => {
                    connection.as_mut().graceful_shutdown();
                    connection.await
                }
                ended = connection.as_mut() => ended,
            }
        };
        tokio::select! {
            biased;
            () = async {
                force.cancelled().await;
                forced.store(true, Ordering::Relaxed);
            } => {},
            ended = serve => {
                if let Err(err) = ended {
                    tracing::debug!(%peer, error = %err, "http_connection_failed");
                }
            },
        }
    }
}

async fn wind_down(finish: &CancellationToken, age_deadline: Option<Instant>) {
    if let Some(deadline) = age_deadline {
        finish
            .run_until_cancelled(tokio::time::sleep_until(deadline))
            .await;
    } else {
        finish.cancelled().await;
    }
}

fn spread(age: Duration) -> Duration {
    const AGE_SPREAD_PERMILLE: u32 = 100;
    let draw = RandomState::new().build_hasher().finish() % u64::from(2 * AGE_SPREAD_PERMILLE + 1);
    let permille = 1000 - AGE_SPREAD_PERMILLE + u32::try_from(draw).unwrap_or(AGE_SPREAD_PERMILLE);
    age.checked_mul(permille)
        .map_or(age, |scaled| scaled / 1000)
}

#[cfg(test)]
mod tests;
