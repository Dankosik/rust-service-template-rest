use std::future::Future;
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
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;
use tokio_util::task::{AbortOnDropHandle, TaskTracker};
use tower::ServiceBuilder;

mod sniff;

use sniff::SniffDeadline;

pub(crate) const CONNECTIONS_REFUSED_METRIC: &str = "http_server_connections_refused_total";

#[derive(Clone, Copy, Debug)]
pub struct ServerOptions {
    pub header_read_timeout: Duration,
    pub max_header_bytes: usize,
    pub max_connections: Option<NonZeroU32>,
    pub max_connection_age: Option<Duration>,
}

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

#[derive(Clone, Copy, Debug)]
pub(crate) enum ListenerScheme {
    Http,
    Https,
}

impl ListenerScheme {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

#[derive(Clone)]
pub(crate) struct ConnectionInfo {
    pub(crate) scheme: ListenerScheme,
    forced: Arc<AtomicBool>,
}

impl ConnectionInfo {
    pub(crate) fn was_forced(&self) -> bool {
        self.forced.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
pub struct BoundListener {
    listener: TcpListener,
    local_addr: SocketAddr,
}

impl BoundListener {
    pub async fn bind(addr: SocketAddr) -> Result<Self, ServerError> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|source| ServerError::Bind { addr, source })?;
        let local_addr = listener
            .local_addr()
            .map_err(|source| ServerError::Bind { addr, source })?;
        Ok(Self {
            listener,
            local_addr,
        })
    }

    #[must_use]
    pub fn serve(self, app: Router, options: ServerOptions) -> Server {
        serve_listener(self, app, options, |stream| async move {
            Some((stream, ListenerScheme::Http))
        })
    }

    // template:begin grpc:http-bound-listener-tls
    #[must_use]
    pub fn serve_tls(
        self,
        app: Router,
        options: ServerOptions,
        tls: Arc<rustls::ServerConfig>,
    ) -> Server {
        let acceptor = tokio_rustls::TlsAcceptor::from(tls);
        let handshake_timeout = options.header_read_timeout;
        serve_listener(self, app, options, move |stream| {
            let acceptor = acceptor.clone();
            async move {
                let peer = stream.peer_addr().ok();
                match tokio::time::timeout(handshake_timeout, acceptor.accept(stream)).await {
                    Ok(Ok(tls_stream)) => Some((tls_stream, ListenerScheme::Https)),
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
    // template:end grpc:http-bound-listener-tls
}

impl Server {
    pub async fn bind(
        addr: SocketAddr,
        app: Router,
        options: ServerOptions,
    ) -> Result<Self, ServerError> {
        Ok(BoundListener::bind(addr).await?.serve(app, options))
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

const HTTP2_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(20);
const HTTP2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);

pub const HTTP2_CONNECTION_WINDOW: u32 = 8 * 1024 * 1024;
pub const HTTP2_STREAM_WINDOW: u32 = 2 * 1024 * 1024;

const HTTP2_MAX_CONCURRENT_STREAMS: u32 = 4096;

fn connection_builder(options: ServerOptions) -> auto::Builder<TokioExecutor> {
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(options.header_read_timeout)
        .max_buf_size(
            options
                .max_header_bytes
                .max(usize::from(HTTP1_MIN_READ_BUFFER_BYTES)),
        )
        .keep_alive(true);
    builder
        .http2()
        .timer(TokioTimer::new())
        .max_header_list_size(u32::try_from(options.max_header_bytes).unwrap_or(u32::MAX))
        .keep_alive_interval(Some(HTTP2_KEEP_ALIVE_INTERVAL))
        .keep_alive_timeout(HTTP2_KEEP_ALIVE_TIMEOUT)
        .initial_connection_window_size(HTTP2_CONNECTION_WINDOW)
        .initial_stream_window_size(HTTP2_STREAM_WINDOW)
        .max_concurrent_streams(HTTP2_MAX_CONCURRENT_STREAMS);
    builder
}

fn serve_listener<F, Fut, IO>(
    bound: BoundListener,
    app: Router,
    options: ServerOptions,
    prepare_io: F,
) -> Server
where
    F: Fn(TcpStream) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Option<(IO, ListenerScheme)>> + Send + 'static,
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
    if options.max_connections.is_some() {
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
        options,
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
    options: ServerOptions,
    connection_shutdown: ConnectionShutdown,
    connections: TaskTracker,
    prepare_io: F,
) where
    F: Fn(TcpStream) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Option<(IO, ListenerScheme)>> + Send + 'static,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let finish = &connection_shutdown.finish;
    let builder = connection_builder(options);
    let permits = options
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
                tracing::warn!(error = %err, "http_accept_failed");
                tokio::select! {
                    () = finish.cancelled() => break,
                    () = tokio::time::sleep(Duration::from_millis(50)) => {}
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
        let _ = stream.set_nodelay(true);
        let connection = serve_connection(
            stream,
            peer,
            prepare_io.clone(),
            builder.clone(),
            app.clone(),
            options,
            connection_shutdown.clone(),
        );
        connections.spawn(async move {
            let _permit = permit;
            connection.await;
        });
    }
    drop(listener);
}

fn serve_connection<F, Fut, IO>(
    stream: TcpStream,
    peer: SocketAddr,
    prepare_io: F,
    builder: auto::Builder<TokioExecutor>,
    app: Router,
    options: ServerOptions,
    shutdown: ConnectionShutdown,
) -> impl Future<Output = ()>
where
    F: Fn(TcpStream) -> Fut,
    Fut: Future<Output = Option<(IO, ListenerScheme)>>,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let age_deadline = options
        .max_connection_age
        .map(|age| deadline_after(spread(age)));
    async move {
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
                return;
            };
            let io = SniffDeadline::new(io, options.header_read_timeout);
            let service = ServiceBuilder::new()
                .layer(Extension(ConnectionInfo {
                    scheme,
                    forced: connection_forced,
                }))
                .service(app);
            let connection =
                builder.serve_connection(TokioIo::new(io), TowerToHyperService::new(service));
            tokio::pin!(connection);
            let ended = tokio::select! {
                biased;
                () = closing => {
                    connection.as_mut().graceful_shutdown();
                    connection.await
                }
                ended = connection.as_mut() => ended,
            };
            if let Err(err) = ended {
                tracing::debug!(%peer, error = %err, "http_connection_failed");
            }
        };
        tokio::select! {
            biased;
            () = async {
                force.cancelled().await;
                forced.store(true, Ordering::Relaxed);
            } => {},
            () = serve => {},
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
