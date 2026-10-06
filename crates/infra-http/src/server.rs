//! Bounded HTTP server over hyper.
//!
//! `axum::serve` is intentionally unconfigurable: it sets no timer, so
//! hyper's header-read timeout is silently disabled, and it exposes no
//! header-size or connection bounds. This accept loop owns those limits and
//! the graceful drain; the composition root decides when to stop and how
//! long to wait.

use std::future::Future;
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::io;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, Sleep, timeout_at};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Connection-level policy the router cannot express.
#[derive(Clone, Copy, Debug)]
pub struct ServerOptions {
    /// Time a connection may take to deliver a complete request head. hyper
    /// restarts it whenever an HTTP/1 connection goes idle, so it is also
    /// the HTTP/1 keep-alive idle bound. hyper starts it only once it knows
    /// the protocol, so the same duration first bounds that decision: a
    /// client that connects and sends nothing, or only the start of the
    /// HTTP/2 preface, is closed after it. The two waits can add, not
    /// replace. HTTP/2 idle uses a separate PING cadence, not this value.
    pub header_read_timeout: Duration,
    /// Read-buffer ceiling for one request head. HTTP/1 overflow answers
    /// hyper-native 431 before the router (not a [`crate::Problem`]). HTTP/2
    /// applies the same number as uncompressed header-list size.
    /// hyper refuses HTTP/1 values below 8 KiB.
    pub max_header_bytes: usize,
    /// Accepted connections at once; `None` accepts without a bound.
    pub max_connections: Option<NonZeroU32>,
    /// Age after which a connection is told to finish and close, as at
    /// drain: HTTP/2 gets GOAWAY and its open streams complete, HTTP/1
    /// closes after the response in flight. A client that reconnects is
    /// balanced again, which a long-lived connection behind a
    /// connection-level balancer never is. Each connection's age is spread
    /// by up to 10% either way, as grpc-go spreads `MaxConnectionAge`, so
    /// connections opened together do not all close together. `None` keeps
    /// a connection for as long as its peer does.
    pub max_connection_age: Option<Duration>,
}

/// Server failures visible to the composition root.
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

/// An unexpected accept-loop termination, independent of joining the task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptFailure {
    Ended,
    Panicked,
}

/// How the drain ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drained {
    /// Every connection closed inside the budget.
    Complete,
    /// The accept loop joined, but the budget expired with connections still
    /// open. Force cancellation was requested; completion is not confirmed.
    /// `remaining_connections` is the count at force cancellation.
    TimedOut { remaining_connections: usize },
}

/// A bound, serving HTTP server.
#[derive(Debug)]
pub struct Server {
    local_addr: SocketAddr,
    stop_accepting: CancellationToken,
    accept_loop: Option<JoinHandle<()>>,
    accept_failure: watch::Receiver<Option<AcceptFailure>>,
    connection_shutdown: ConnectionShutdown,
    connections: TaskTracker,
}

#[derive(Clone, Debug)]
struct ConnectionShutdown {
    finish: CancellationToken,
    force: CancellationToken,
}

struct ReportAcceptFailure {
    stop: CancellationToken,
    failure: watch::Sender<Option<AcceptFailure>>,
}

impl Drop for ReportAcceptFailure {
    fn drop(&mut self) {
        let failure = if std::thread::panicking() {
            Some(AcceptFailure::Panicked)
        } else if !self.stop.is_cancelled() {
            Some(AcceptFailure::Ended)
        } else {
            None
        };
        if let Some(failure) = failure {
            self.failure.send_if_modified(|current| {
                if current.is_some() {
                    return false;
                }
                *current = Some(failure);
                true
            });
        }
    }
}

impl Server {
    /// Bind `addr` and start accepting on the current Tokio runtime.
    ///
    /// Returns once the listener is bound. Initialize request-visible state
    /// before calling: the accept task can serve requests before this returns.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Bind`] when the address cannot be bound.
    pub async fn bind(
        addr: SocketAddr,
        app: Router,
        options: ServerOptions,
    ) -> Result<Self, ServerError> {
        bind_listener(addr, app, options, |stream| async move { Some(stream) }).await
    }

    // template:begin grpc:http-server-tls
    /// Bind `addr` and start accepting TLS connections on the current Tokio runtime.
    ///
    /// The handshake is bounded by [`ServerOptions::header_read_timeout`],
    /// which also closes a client that never starts it. A handshake error or
    /// timeout closes the connection without a response, and so does drain:
    /// no request can be in flight before the handshake ends.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Bind`] when the address cannot be bound.
    pub async fn bind_tls(
        addr: SocketAddr,
        app: Router,
        options: ServerOptions,
        tls: Arc<rustls::ServerConfig>,
    ) -> Result<Self, ServerError> {
        let acceptor = tokio_rustls::TlsAcceptor::from(tls);
        let handshake_timeout = options.header_read_timeout;
        bind_listener(addr, app, options, move |stream| {
            let acceptor = acceptor.clone();
            async move {
                match tokio::time::timeout(handshake_timeout, acceptor.accept(stream)).await {
                    Ok(Ok(tls_stream)) => Some(tls_stream),
                    Ok(Err(_)) | Err(_) => None,
                }
            }
        })
        .await
    }
    // template:end grpc:http-server-tls

    /// The address the listener actually bound, including an OS-assigned port.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Observe a sticky accept-loop fault without borrowing or joining this
    /// server. An expected stop leaves this future pending.
    pub fn failure(&self) -> impl Future<Output = AcceptFailure> + Send + 'static + use<> {
        let mut failure = self.accept_failure.clone();
        async move {
            loop {
                if let Some(failure) = *failure.borrow_and_update() {
                    return failure;
                }
                if failure.changed().await.is_err() {
                    return std::future::pending().await;
                }
            }
        }
    }

    /// Stop accepting, tell every connection to finish its current request
    /// and close, and wait up to `budget` for them to do so.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::AcceptTask`] when the accept loop failed, or
    /// [`ServerError::AcceptTimeout`] when its termination is unconfirmed.
    /// Either error is preserved while connections are cleaned up.
    pub async fn drain(mut self, budget: Duration) -> Result<Drained, ServerError> {
        let deadline = Instant::now() + budget;
        self.stop_accepting.cancel();
        self.connection_shutdown.finish.cancel();
        // Keep the handle in self across await: dropping this waiter must
        // still abort acceptance rather than detach its task.
        let accept_error = if let Some(accept_loop) = self.accept_loop.as_mut() {
            if let Ok(joined) = timeout_at(deadline, accept_loop).await {
                drop(self.accept_loop.take());
                joined.err().map(ServerError::AcceptTask)
            } else {
                self.connection_shutdown.force.cancel();
                if let Some(accept_loop) = &self.accept_loop {
                    accept_loop.abort();
                }
                return Err(ServerError::AcceptTimeout);
            }
        } else {
            None
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
        accept_error.map_or(Ok(drained), Err)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Covers a dropped server and a cancelled drain waiter. These are
        // requests only; synchronous Drop cannot confirm task completion.
        self.stop_accepting.cancel();
        self.connection_shutdown.finish.cancel();
        self.connection_shutdown.force.cancel();
        if let Some(accept_loop) = self.accept_loop.take() {
            accept_loop.abort();
        }
    }
}

/// hyper refuses an HTTP/1 read buffer below this size (builder panic
/// minimum). Config admission rejects the same floor so operators never see
/// a silent raise; this clamp still protects `Server::bind` when validation
/// did not run.
const HYPER_MIN_HEADER_BUF: usize = 8 * 1024;
/// Template HTTP/2 PING cadence. Independent of `header_read_timeout`,
/// which is the HTTP/1 idle bound.
const HTTP2_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(20);
const HTTP2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);

fn connection_builder(options: ServerOptions) -> auto::Builder<TokioExecutor> {
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        // Without a timer, `header_read_timeout` is not enforced; the timer
        // is installed so this builder does not repeat `axum::serve`'s
        // silent disable.
        .timer(TokioTimer::new())
        .header_read_timeout(options.header_read_timeout)
        .max_buf_size(options.max_header_bytes.max(HYPER_MIN_HEADER_BUF))
        .keep_alive(true);
    builder
        .http2()
        .timer(TokioTimer::new())
        .max_header_list_size(u32::try_from(options.max_header_bytes).unwrap_or(u32::MAX))
        // HTTP/2 PING keep-alive is not `http.header_read_timeout`: that
        // key is the HTTP/1 idle bound. `max_header_bytes` here is
        // uncompressed HTTP/2 header-list size, not the HTTP/1 read buffer
        // whose overflow is hyper-native 431.
        .keep_alive_interval(Some(HTTP2_KEEP_ALIVE_INTERVAL))
        .keep_alive_timeout(HTTP2_KEEP_ALIVE_TIMEOUT)
        // Grow the receive windows to the measured bandwidth-delay product,
        // as grpc-go does, so a large request is not limited to one fixed
        // window per round trip.
        .adaptive_window(true);
    builder
}

// Plain HTTP passes the socket through; TLS performs its bounded handshake.
// The callback returns None to close a connection before hyper owns it.
async fn bind_listener<F, Fut, IO>(
    addr: SocketAddr,
    app: Router,
    options: ServerOptions,
    prepare_io: F,
) -> Result<Server, ServerError>
where
    F: Fn(TcpStream) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Option<IO>> + Send + 'static,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|source| ServerError::Bind { addr, source })?;
    let local_addr = listener
        .local_addr()
        .map_err(|source| ServerError::Bind { addr, source })?;
    let stop_accepting = CancellationToken::new();
    let connection_shutdown = ConnectionShutdown {
        finish: CancellationToken::new(),
        force: CancellationToken::new(),
    };
    let connections = TaskTracker::new();
    let (failure, accept_failure) = watch::channel(None);
    let guard = ReportAcceptFailure {
        stop: stop_accepting.clone(),
        failure,
    };
    let accept = accept_loop(
        listener,
        app,
        options,
        stop_accepting.clone(),
        connection_shutdown.clone(),
        connections.clone(),
        prepare_io,
    );
    let accept_loop = tokio::spawn(async move {
        let _guard = guard;
        accept.await;
    });
    Ok(Server {
        local_addr,
        stop_accepting,
        accept_loop: Some(accept_loop),
        accept_failure,
        connection_shutdown,
        connections,
    })
}

async fn accept_loop<F, Fut, IO>(
    listener: TcpListener,
    app: Router,
    options: ServerOptions,
    stop: CancellationToken,
    connection_shutdown: ConnectionShutdown,
    connections: TaskTracker,
    prepare_io: F,
) where
    F: Fn(TcpStream) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Option<IO>> + Send + 'static,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let builder = connection_builder(options);
    let permits = options
        .max_connections
        .map(|limit| Arc::new(Semaphore::new(limit.get() as usize)));

    loop {
        let accepted = tokio::select! {
            () = stop.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(err) => {
                // Transient accept errors (EMFILE, ECONNABORTED) recover on
                // their own; back off briefly instead of spinning.
                tracing::warn!(error = %err, "accept failed");
                tokio::select! {
                    () = stop.cancelled() => break,
                    () = tokio::time::sleep(Duration::from_millis(50)) => {}
                }
                continue;
            }
        };
        let permit = match permits.as_ref().map(|p| p.clone().try_acquire_owned()) {
            None => None,
            Some(Ok(permit)) => Some(permit),
            Some(Err(_)) => {
                // Over the cap: close without a response. Excess callers
                // otherwise wait in the kernel backlog, which is what the
                // headroom over max_in_flight is for.
                metrics::counter!(CONNECTIONS_REFUSED_METRIC).increment(1);
                tracing::debug!(%peer, "connection refused at the connection cap");
                drop(stream);
                continue;
            }
        };
        let finish = connection_shutdown.finish.clone();
        let force = connection_shutdown.force.clone();
        let builder = builder.clone();
        let app = app.clone();
        let prepare_io = prepare_io.clone();
        // HTTP/2 writes headers, data, and trailers as separate small
        // segments; with Nagle on, each can wait for the peer's delayed ACK
        // (about 40 ms). Failing to set it only costs latency.
        let _ = stream.set_nodelay(true);
        connections.spawn(async move {
            // Keep admission for the TLS handshake and the entire hyper
            // connection, releasing it on every exit path.
            let _permit = permit;
            let serve = async move {
                // No request can be in flight before the handshake ends, so drain
                // drops the connection instead of waiting the handshake out.
                let io = tokio::select! {
                    io = prepare_io(stream) => io,
                    () = finish.cancelled() => None,
                };
                let Some(io) = io else {
                    return;
                };
                let io = SniffDeadline::new(io, options.header_read_timeout);
                let connection = builder.serve_connection_with_upgrades(
                    TokioIo::new(io),
                    TowerToHyperService::new(app),
                );
                tokio::pin!(connection);
                let ended = tokio::select! {
                    ended = connection.as_mut() => Some(ended),
                    () = finish.cancelled() => None,
                    () = reached_age(options.max_connection_age) => None,
                };
                let ended = if let Some(ended) = ended {
                    ended
                } else {
                    connection.as_mut().graceful_shutdown();
                    connection.await
                };
                if let Err(err) = ended {
                    tracing::debug!(%peer, error = %err, "connection ended with error");
                }
            };
            tokio::select! {
                biased;
                () = force.cancelled() => {},
                () = serve => {},
            }
        });
    }
    // Dropping the listener here refuses new connections at once; the
    // accepted ones keep running in their tracked tasks.
    drop(listener);
}

/// Resolves when a connection has lived for its spread age; never without one.
async fn reached_age(max_connection_age: Option<Duration>) {
    match max_connection_age {
        Some(age) => tokio::time::sleep(spread(age)).await,
        None => std::future::pending().await,
    }
}

/// `age` scaled by a factor drawn from 0.9 to 1.1. The draw is the standard
/// library's randomly keyed hasher: enough to spread closes, not a secret.
fn spread(age: Duration) -> Duration {
    let draw = RandomState::new().build_hasher().finish() % 201;
    let permille = 900 + u32::try_from(draw).unwrap_or(100);
    age.checked_mul(permille)
        .map_or(age, |scaled| scaled / 1000)
}

/// Counter of connections closed at accept because `max_connections` was
/// reached.
pub const CONNECTIONS_REFUSED_METRIC: &str = "http_server_connections_refused_total";

/// The HTTP/2 connection preface, which hyper-util's protocol sniff compares
/// the first bytes of a connection against.
const HTTP2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// A socket whose reads fail once `timeout` passes with the protocol still
/// undecided.
///
/// hyper-util's sniff reads until the bytes stop matching the HTTP/2 preface
/// and has no timer (hyper #3756), so a client that connects and sends
/// nothing, or only the start of the preface, would never be timed out.
/// hyper's header timer and HTTP/2 keep-alive start once the protocol is
/// decided; from then on this is a pass-through. The failed read ends the
/// connection without a response.
struct SniffDeadline<IO> {
    io: IO,
    /// The preface bytes matched so far and the deadline, until decided.
    undecided: Option<(usize, Pin<Box<Sleep>>)>,
}

impl<IO> SniffDeadline<IO> {
    fn new(io: IO, timeout: Duration) -> Self {
        Self {
            io,
            undecided: Some((0, Box::pin(tokio::time::sleep(timeout)))),
        }
    }
}

impl<IO: AsyncRead + Unpin> AsyncRead for SniffDeadline<IO> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let Some((matched, deadline)) = this.undecided.as_mut() else {
            return Pin::new(&mut this.io).poll_read(cx, buf);
        };
        let filled = buf.filled().len();
        match Pin::new(&mut this.io).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let read = &buf.filled()[filled..];
                let rest = &HTTP2_PREFACE[*matched..];
                // Undecided only while everything read is a strict prefix of
                // the preface; the end of the stream decides as well.
                if !read.is_empty() && read.len() < rest.len() && rest.starts_with(read) {
                    *matched += read.len();
                } else {
                    this.undecided = None;
                }
                Poll::Ready(Ok(()))
            }
            Poll::Pending => deadline
                .as_mut()
                .poll(cx)
                .map(|()| Err(io::ErrorKind::TimedOut.into())),
            failed @ Poll::Ready(Err(_)) => failed,
        }
    }
}

impl<IO: AsyncWrite + Unpin> AsyncWrite for SniffDeadline<IO> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }

    // hyper writes a response head and body as one vectored write; the
    // default implementation would send them as separate segments.
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::num::NonZeroU32;

    use axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn loopback() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
    }

    fn options() -> ServerOptions {
        ServerOptions {
            header_read_timeout: Duration::from_millis(300),
            max_header_bytes: 8 * 1024,
            max_connections: NonZeroU32::new(4),
            max_connection_age: None,
        }
    }

    fn app() -> Router {
        let handler_1 = || async { "ok" };
        let handler_2 = || async {
            tokio::time::sleep(Duration::from_millis(400)).await;
            "late"
        };
        #[allow(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let routes = Router::new()
            .route("/ok", get(handler_1))
            .route("/slow", get(handler_2));
        routes
    }

    async fn fetch(addr: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    #[tokio::test]
    async fn serves_and_drains_completely() {
        let server = Server::bind(loopback(), app(), options()).await.unwrap();
        let failure = server.failure();
        let addr = server.local_addr();
        assert!(fetch(addr, "/ok").await.starts_with("HTTP/1.1 200 "));
        let drained = server.drain(Duration::from_secs(2)).await.unwrap();
        assert_eq!(drained, Drained::Complete);
        assert!(
            TcpStream::connect(addr).await.is_err(),
            "listener still open"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), failure)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn in_flight_request_finishes_during_drain() {
        let server = Server::bind(loopback(), app(), options()).await.unwrap();
        let addr = server.local_addr();
        let slow = tokio::spawn(async move { fetch(addr, "/slow").await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let drained = server.drain(Duration::from_secs(2)).await.unwrap();
        assert_eq!(drained, Drained::Complete);
        assert!(slow.await.unwrap().ends_with("late"));
    }

    #[tokio::test]
    async fn drain_budget_expiry_forces_an_in_flight_connection() {
        let (server, peer) = pending_request().await;
        let drained = server.drain(Duration::from_millis(50)).await.unwrap();
        assert_eq!(
            drained,
            Drained::TimedOut {
                remaining_connections: 1
            }
        );
        assert_peer_closed(peer).await;
    }

    async fn pending_request() -> (Server, TcpStream) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let notify = entered.clone();
        let handler_1 = move || {
            let entered = notify.clone();
            async move {
                entered.notify_one();
                std::future::pending::<&'static str>().await
            }
        };
        #[allow(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let app = Router::new().route("/pending", get(handler_1));
        let server = Server::bind(loopback(), app, options()).await.unwrap();
        let mut peer = TcpStream::connect(server.local_addr()).await.unwrap();
        peer.write_all(b"GET /pending HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        (server, peer)
    }

    async fn assert_peer_closed(mut peer: TcpStream) {
        let mut response = Vec::new();
        let closed =
            tokio::time::timeout(Duration::from_secs(2), peer.read_to_end(&mut response)).await;
        assert!(
            matches!(closed, Ok(Ok(0) | Err(_))),
            "connection remained open: {closed:?}"
        );
        assert_eq!(response, [] as [u8; 0]);
    }

    #[tokio::test]
    async fn dropping_server_forces_in_flight_connection_cleanup() {
        let (server, peer) = pending_request().await;
        drop(server);
        assert_peer_closed(peer).await;
    }

    #[tokio::test]
    async fn cancelling_drain_forces_in_flight_connection_cleanup() {
        let (server, peer) = pending_request().await;
        let graceful = server.connection_shutdown.finish.clone();
        let draining = tokio::spawn(server.drain(Duration::from_secs(60)));
        tokio::time::timeout(Duration::from_secs(2), graceful.cancelled())
            .await
            .unwrap();
        draining.abort();
        assert!(draining.await.unwrap_err().is_cancelled());
        assert_peer_closed(peer).await;
    }

    #[tokio::test]
    async fn accept_timeout_is_distinct_and_forces_connection_cleanup() {
        let (mut server, peer) = pending_request().await;
        let accept = server.accept_loop.take().unwrap();
        accept.abort();
        assert!(accept.await.unwrap_err().is_cancelled());
        // Control the native acceptance owner independently of the real
        // open request: the deadline must include its unconfirmed join.
        let stalled = tokio::spawn(std::future::pending());
        let abort = stalled.abort_handle();
        server.accept_loop = Some(stalled);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            server.drain(Duration::from_millis(20)),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(ServerError::AcceptTimeout)));
        assert_peer_closed(peer).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !abort.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn accept_failure_stays_sticky_and_drain_preserves_it_while_cleaning_connections() {
        let (server, peer) = pending_request().await;
        server.accept_loop.as_ref().unwrap().abort();
        let failure = tokio::time::timeout(Duration::from_secs(2), server.failure())
            .await
            .unwrap();
        assert_eq!(failure, AcceptFailure::Ended);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), server.failure())
                .await
                .unwrap(),
            failure
        );
        // Fault observation leaves the native join for drain, while the
        // real open request still needs forced cleanup.
        let result = server.drain(Duration::from_millis(20)).await;
        assert!(matches!(result, Err(ServerError::AcceptTask(error)) if error.is_cancelled()));
        assert_peer_closed(peer).await;
    }

    #[tokio::test]
    async fn accept_panic_after_stop_is_still_observed() {
        #[derive(Debug)]
        struct PanicOnClone(Arc<std::sync::OnceLock<CancellationToken>>);
        impl Clone for PanicOnClone {
            fn clone(&self) -> Self {
                self.0.get().unwrap().cancel();
                panic!("accept preparation clone failed");
            }
        }
        let stop = Arc::new(std::sync::OnceLock::new());
        let marker = PanicOnClone(stop.clone());
        let server = bind_listener(loopback(), app(), options(), move |stream| {
            let _ = &marker;
            async move { Some(stream) }
        })
        .await
        .unwrap();
        stop.set(server.stop_accepting.clone()).unwrap();
        let failure = server.failure();
        let peer = TcpStream::connect(server.local_addr()).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), failure)
                .await
                .unwrap(),
            AcceptFailure::Panicked
        );
        // Observing the fault does not consume acceptance's join outcome.
        assert!(
            matches!(server.drain(Duration::from_secs(1)).await, Err(ServerError::AcceptTask(error)) if error.is_panic())
        );
        drop(peer);
    }

    #[tokio::test]
    async fn silent_client_is_closed_after_the_header_timeout() {
        let server = Server::bind(loopback(), app(), options()).await.unwrap();
        let mut silent = TcpStream::connect(server.local_addr()).await.unwrap();
        let mut buf = [0u8; 1];
        let closed = tokio::time::timeout(Duration::from_secs(2), silent.read(&mut buf)).await;
        assert!(
            matches!(closed, Ok(Ok(0))),
            "expected EOF from the server, got {closed:?}"
        );
        server.drain(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn a_stalled_http2_preface_is_closed_after_the_header_timeout() {
        let server = Server::bind(loopback(), app(), options()).await.unwrap();
        let mut stalled = TcpStream::connect(server.local_addr()).await.unwrap();
        // A strict prefix of the HTTP/2 preface leaves the protocol undecided,
        // which is before hyper's own header timer starts.
        stalled.write_all(b"PRI * HTTP/2.0\r\n").await.unwrap();
        let mut buf = [0u8; 1];
        let closed = tokio::time::timeout(Duration::from_secs(2), stalled.read(&mut buf)).await;
        assert!(
            matches!(closed, Ok(Ok(0))),
            "expected EOF from the server, got {closed:?}"
        );
        server.drain(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn an_undecided_protocol_fails_reads_at_the_deadline() {
        let timeout = Duration::from_secs(5);
        let preface_but_one = &HTTP2_PREFACE[..HTTP2_PREFACE.len() - 1];
        for sent in [&b""[..], b"P", preface_but_one] {
            let (mut client, server) = tokio::io::duplex(64);
            let mut io = SniffDeadline::new(server, timeout);
            let started = tokio::time::Instant::now();
            client.write_all(sent).await.unwrap();
            let mut read = [0u8; 64];
            let error = loop {
                match io.read(&mut read).await {
                    Ok(_) => {}
                    Err(error) => break error,
                }
            };
            assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{sent:?}");
            assert_eq!(started.elapsed(), timeout, "{sent:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_decided_protocol_is_never_timed_out() {
        let cases: [&[&[u8]]; 4] = [
            // An HTTP/1 request line differs from the preface at once.
            &[b"GET"],
            // It differs after a prefix that matched.
            &[b"PR", b"OPFIND"],
            // The whole preface, in two reads.
            &[b"PRI * HTTP/2.0\r\n", b"\r\nSM\r\n\r\n"],
            // The whole preface and the first frame bytes in one read.
            &[b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\0\0"],
        ];
        for chunks in cases {
            let (mut client, server) = tokio::io::duplex(64);
            let mut io = SniffDeadline::new(server, Duration::from_secs(5));
            let mut read = [0u8; 64];
            for chunk in chunks {
                client.write_all(chunk).await.unwrap();
                assert_eq!(io.read(&mut read).await.unwrap(), chunk.len());
            }
            let idle = tokio::time::timeout(Duration::from_mins(1), io.read(&mut read)).await;
            assert!(idle.is_err(), "{chunks:?}: {idle:?}");
        }
    }

    #[tokio::test]
    async fn drain_does_not_wait_for_a_connection_that_sent_nothing() {
        let mut opts = options();
        opts.header_read_timeout = Duration::from_secs(30);
        let server = Server::bind(loopback(), app(), opts).await.unwrap();
        let addr = server.local_addr();
        let mut silent = TcpStream::connect(addr).await.unwrap();
        // Connections are accepted in order, so an answer on a later one
        // proves the silent one already has its task.
        assert!(fetch(addr, "/ok").await.starts_with("HTTP/1.1 200 "));
        let drained = server.drain(Duration::from_secs(2)).await.unwrap();
        assert_eq!(drained, Drained::Complete);
        let mut buf = [0u8; 1];
        let closed = tokio::time::timeout(Duration::from_secs(2), silent.read(&mut buf)).await;
        assert!(
            matches!(closed, Ok(Ok(0))),
            "expected EOF from the server, got {closed:?}"
        );
    }

    #[tokio::test]
    async fn oversized_header_is_431() {
        let server = Server::bind(loopback(), app(), options()).await.unwrap();
        let mut stream = TcpStream::connect(server.local_addr()).await.unwrap();
        let huge = "x".repeat(9 * 1024);
        let request = format!("GET /ok HTTP/1.1\r\nHost: localhost\r\nX-Big: {huge}\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        let outcome =
            tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut response)).await;
        let text = String::from_utf8_lossy(&response);
        // hyper answers, then closes with unread request bytes pending, which
        // surfaces as EOF or a reset depending on timing.
        assert!(matches!(outcome, Ok(Ok(_) | Err(_))), "{outcome:?}");
        assert!(
            text.starts_with("HTTP/1.1 431 "),
            "{}",
            &text[..text.len().min(80)]
        );
        server.drain(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn connection_cap_refuses_the_excess() {
        let mut opts = options();
        opts.max_connections = NonZeroU32::new(1);
        let server = Server::bind(loopback(), app(), opts).await.unwrap();
        let addr = server.local_addr();
        let first = TcpStream::connect(addr).await.unwrap();
        // Let the accept loop take the permit before the second connect.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut second = TcpStream::connect(addr).await.unwrap();
        second
            .write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        let outcome =
            tokio::time::timeout(Duration::from_secs(2), second.read_to_end(&mut buf)).await;
        // Closed with unread bytes pending: EOF or a reset, never a response.
        assert!(
            matches!(outcome, Ok(Ok(0) | Err(_))),
            "second connection should be closed: {outcome:?}"
        );
        assert!(
            buf.is_empty(),
            "refused connection must not receive a response"
        );
        drop(first);
        server.drain(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn a_connection_past_its_age_closes_once_idle_and_new_ones_are_served() {
        let mut opts = options();
        opts.header_read_timeout = Duration::from_secs(5);
        opts.max_connection_age = Some(Duration::from_millis(200));
        let server = Server::bind(loopback(), app(), opts).await.unwrap();
        let addr = server.local_addr();
        let mut kept = TcpStream::connect(addr).await.unwrap();
        kept.write_all(b"GET /ok HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        // The keep-alive response, then EOF when the age is reached: far
        // sooner than the five-second idle bound.
        let mut response = Vec::new();
        let closed =
            tokio::time::timeout(Duration::from_secs(2), kept.read_to_end(&mut response)).await;
        assert!(matches!(closed, Ok(Ok(_))), "{closed:?}");
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200 "), "{text}");
        assert!(text.ends_with("ok"), "{text}");
        assert!(fetch(addr, "/ok").await.starts_with("HTTP/1.1 200 "));
        let drained = server.drain(Duration::from_secs(2)).await.unwrap();
        assert_eq!(drained, Drained::Complete);
    }

    #[test]
    fn a_connection_age_is_spread_within_a_tenth() {
        let age = Duration::from_mins(30);
        let spreads: Vec<Duration> = (0..64).map(|_| spread(age)).collect();
        assert!(
            spreads
                .iter()
                .all(|spread| (age * 9 / 10..=age * 11 / 10).contains(spread)),
            "{spreads:?}"
        );
        assert!(spreads.iter().any(|spread| *spread != spreads[0]));
        assert_eq!(spread(Duration::MAX), Duration::MAX);
    }

    #[tokio::test]
    async fn bind_failure_is_reported() {
        let occupied = Server::bind(loopback(), app(), options()).await.unwrap();
        let err = Server::bind(occupied.local_addr(), app(), options())
            .await
            .expect_err("second bind must fail");
        assert!(matches!(err, ServerError::Bind { .. }), "{err}");
        occupied.drain(Duration::from_secs(1)).await.unwrap();
    }

    // template:begin grpc:http-server-tls-test
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
    use rustls::{ClientConfig, RootCertStore, ServerConfig};
    use tokio_rustls::TlsConnector;

    /// A server and a client configuration for `localhost` under one fresh CA,
    /// both TLS 1.3 with `h2` as the only protocol.
    fn localhost_tls() -> (ServerConfig, ClientConfig) {
        use rcgen::{
            BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa,
            KeyPair, KeyUsagePurpose,
        };

        let mut ca = CertificateParams::default();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let issuer = CertifiedIssuer::self_signed(ca, KeyPair::generate().unwrap()).unwrap();
        let key = KeyPair::generate().unwrap();
        let mut leaf = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
        leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = leaf.signed_by(&key, &issuer).unwrap();
        let cert = CertificateDer::from(certificate.der().to_vec());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let provider = rustls::crypto::aws_lc_rs::default_provider();
        let mut server = ServerConfig::builder_with_provider(provider.clone().into())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();
        server.alpn_protocols = vec![b"h2".to_vec()];
        let mut roots = RootCertStore::empty();
        roots.add(issuer.der().clone()).unwrap();
        let mut client = ClientConfig::builder_with_provider(provider.into())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        client.alpn_protocols = vec![b"h2".to_vec()];
        (server, client)
    }

    #[tokio::test]
    async fn drain_does_not_wait_for_an_unfinished_tls_handshake() {
        let (server_config, client_config) = localhost_tls();
        let mut opts = options();
        opts.header_read_timeout = Duration::from_secs(30);
        let server = Server::bind_tls(loopback(), app(), opts, Arc::new(server_config))
            .await
            .unwrap();
        let addr = server.local_addr();
        let mut stalled = TcpStream::connect(addr).await.unwrap();
        // The first byte of a handshake record: the handshake has begun and
        // cannot finish.
        stalled.write_all(&[0x16]).await.unwrap();
        // Connections are accepted in order, so a completed handshake on a
        // later one proves the stalled one already has its task.
        let later = TcpStream::connect(addr).await.unwrap();
        let later = TlsConnector::from(Arc::new(client_config))
            .connect(ServerName::try_from("localhost").unwrap(), later)
            .await
            .unwrap();
        let drained = server.drain(Duration::from_secs(2)).await.unwrap();
        assert_eq!(drained, Drained::Complete);
        drop(later);
        let mut buf = [0u8; 1];
        let closed = tokio::time::timeout(Duration::from_secs(2), stalled.read(&mut buf)).await;
        assert!(
            matches!(closed, Ok(Ok(0))),
            "expected EOF from the server, got {closed:?}"
        );
    }

    #[tokio::test]
    async fn tls_listener_serves_http2() {
        use http_body_util::BodyExt as _;

        let (server_config, client_config) = localhost_tls();
        let mut opts = options();
        opts.header_read_timeout = Duration::from_secs(2);
        let server = Server::bind_tls(loopback(), app(), opts, Arc::new(server_config))
            .await
            .unwrap();
        let addr = server.local_addr();
        let stream = TcpStream::connect(addr).await.unwrap();
        let tls = TlsConnector::from(Arc::new(client_config))
            .connect(ServerName::try_from("localhost").unwrap(), stream)
            .await
            .unwrap();
        let (mut sender, connection) = hyper::client::conn::http2::handshake(
            hyper_util::rt::TokioExecutor::new(),
            hyper_util::rt::TokioIo::new(tls),
        )
        .await
        .unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let request = http::Request::builder()
            .uri(format!("https://localhost:{}/ok", addr.port()))
            .body(axum::body::Body::empty())
            .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(2), sender.send_request(request))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"ok");
        server.drain(Duration::from_secs(1)).await.unwrap();
    }
    // template:end grpc:http-server-tls-test
}
