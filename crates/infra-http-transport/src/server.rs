use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::mem::DropGuard;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::{Extension, Router};
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

#[derive(Debug)]
pub struct Server {
    local_addr: SocketAddr,
    accept_loop: AbortOnDropHandle<()>,
    accept_failure: watch::Receiver<Option<AcceptFailure>>,
    connection_shutdown: ConnectionShutdown,
    connections: TaskTracker,
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

    pub async fn drain(mut self, budget: Duration) -> Result<Drained, ServerError> {
        let deadline = deadline_after(budget);
        self.connection_shutdown.finish.cancel();
        let Ok(joined) = timeout_at(deadline, &mut self.accept_loop).await else {
            self.connection_shutdown.force.cancel();
            return Err(ServerError::AcceptTimeout);
        };

        self.connections.close();
        let drained = if timeout_at(deadline, self.connections.wait()).await.is_ok() {
            Drained::Complete
        } else {
            let remaining_connections = self.connections.len();
            self.connection_shutdown.force.cancel();
            Drained::TimedOut {
                remaining_connections,
            }
        };
        joined.map(|()| drained).map_err(ServerError::AcceptTask)
    }
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
    }
}

#[derive(Clone)]
struct ConnectionDriver {
    builder: auto::Builder<TokioExecutor>,
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
    let mut builder = auto::Builder::new(TokioExecutor::new());
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
        // template:begin grpc:http1-driver-no-preface
        preface_timeout: None,
        // template:end grpc:http1-driver-no-preface
    }
}

// template:begin grpc:http2-driver
fn http2_driver(options: Http2Options) -> ConnectionDriver {
    let mut builder = auto::Builder::new(TokioExecutor::new());
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
    let BoundListener {
        listener,
        local_addr,
    } = bound;
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
    let accept = accept_loop(
        listener,
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
    }
}

async fn accept_loop<F, Fut, IO>(
    listener: TcpListener,
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
                tracing::debug!(%peer, "http_connection_refused");
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
    use std::io::ErrorKind::{ConnectionAborted, ConnectionRefused, ConnectionReset};
    match error.kind() {
        ConnectionAborted | ConnectionRefused | ConnectionReset => Duration::ZERO,
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
