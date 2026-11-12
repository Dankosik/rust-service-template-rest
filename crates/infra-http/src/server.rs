//! Bounded HTTP server over hyper.
//!
//! `axum::serve` is intentionally unconfigurable: it sets no timer, so
//! hyper's header-read timeout is silently disabled, and it exposes no
//! header-size or connection bounds. This accept loop owns those limits and
//! the graceful drain; the composition root decides when to stop and how
//! long to wait.

use std::future::Future;
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::{Extension, Router};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;
use tokio_util::task::{AbortOnDropHandle, TaskTracker};
use tower::ServiceBuilder;

mod sniff;

use sniff::SniffDeadline;

pub(crate) const CONNECTIONS_REFUSED_METRIC: &str = "http_server_connections_refused_total";

/// Connection-level policy the router cannot express.
#[derive(Clone, Copy, Debug)]
pub struct ServerOptions {
    /// Time an HTTP/1 connection may take to deliver a complete request head.
    /// hyper restarts it whenever an HTTP/1 connection goes idle, so it is
    /// also the HTTP/1 keep-alive idle bound. hyper starts it only once it
    /// knows the protocol, so the same duration first bounds that decision: a
    /// client that connects and sends nothing, or only the start of the
    /// HTTP/2 preface, is closed after it. The two waits can add, not replace.
    /// HTTP/2 request headers are bounded by `max_header_bytes`, not by this
    /// value; HTTP/2 idle uses a separate PING cadence.
    pub header_read_timeout: Duration,
    /// Read-buffer ceiling for one request head. HTTP/1 overflow answers
    /// hyper-native 431 before the router (not a [`crate::Problem`]). hyper
    /// refuses an HTTP/1 buffer below 8 KiB, so a smaller value is raised to
    /// 8 KiB for HTTP/1. HTTP/2 applies the value unchanged as the
    /// uncompressed header-list size.
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
    TimedOut {
        /// Connections still open when force cancellation was requested.
        remaining_connections: usize,
    },
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
    /// Stop accepting and ask every connection to finish its current request
    /// and close; a connection still being prepared is dropped.
    finish: CancellationToken,
    /// Drop every connection at once, when the drain budget is spent.
    force: CancellationToken,
}

#[derive(Clone)]
pub(crate) struct ConnectionInfo {
    pub(crate) scheme: &'static str,
    forced: Arc<AtomicBool>,
}

impl ConnectionInfo {
    pub(crate) fn was_forced(&self) -> bool {
        self.forced.load(Ordering::Relaxed)
    }
}

struct ReportAcceptFailure {
    finish: CancellationToken,
    failure: watch::Sender<Option<AcceptFailure>>,
}

impl Drop for ReportAcceptFailure {
    fn drop(&mut self) {
        let failure = if std::thread::panicking() {
            Some(AcceptFailure::Panicked)
        } else if !self.finish.is_cancelled() {
            Some(AcceptFailure::Ended)
        } else {
            None
        };
        if let Some(failure) = failure {
            self.failure.send_replace(Some(failure));
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
        bind_listener(addr, app, options, |stream| async move {
            Some((stream, "http"))
        })
        .await
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
                let peer = stream.peer_addr().ok();
                match tokio::time::timeout(handshake_timeout, acceptor.accept(stream)).await {
                    Ok(Ok(tls_stream)) => Some((tls_stream, "https")),
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
        .await
    }
    // template:end grpc:http-server-tls

    /// The address the listener actually bound, including an OS-assigned port.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Observe a sticky accept-loop fault without borrowing or joining this
    /// server. An expected stop leaves this future pending. The first poll
    /// reports a fault already recorded, so a non-blocking check may poll once
    /// (`now_or_never`); after a normal stop or sender drop the future stays
    /// pending.
    pub fn accept_failure(&self) -> impl Future<Output = AcceptFailure> + Send + 'static + use<> {
        let mut failure = self.accept_failure.clone();
        async move {
            let seen = failure
                .wait_for(Option::is_some)
                .await
                .ok()
                .and_then(|seen| *seen);
            match seen {
                Some(failure) => failure,
                None => std::future::pending().await,
            }
        }
    }

    /// Stop accepting, tell every connection to finish its current request
    /// and close, and wait up to `budget` for them to do so.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::AcceptTask`] when the accept loop failed. The
    /// connection drain still runs within the same deadline, but its
    /// [`Drained`] result is not returned. Returns
    /// [`ServerError::AcceptTimeout`] when the accept loop's termination is
    /// unconfirmed; that path force-cancels the remaining connections and
    /// returns without joining them.
    pub async fn drain(mut self, budget: Duration) -> Result<Drained, ServerError> {
        let deadline = deadline_after(budget);
        self.connection_shutdown.finish.cancel();
        // Keep the handle in self across await: dropping this waiter must
        // still abort acceptance rather than detach its task.
        let Ok(joined) = timeout_at(deadline, &mut self.accept_loop).await else {
            self.connection_shutdown.force.cancel();
            return Err(ServerError::AcceptTimeout);
        };

        // Close the tracker only after the join: the accept loop is the only
        // task that spawns connections into it.
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

/// `Instant + Duration` panics past the clock's range, so a budget too long to
/// represent waits as long as tokio's own `timeout` would: thirty years.
fn deadline_after(budget: Duration) -> Instant {
    const FAR_FUTURE: Duration = Duration::from_hours(24 * 365 * 30);
    let now = Instant::now();
    now.checked_add(budget).unwrap_or(now + FAR_FUTURE)
}

impl Drop for Server {
    fn drop(&mut self) {
        // Covers a dropped server and a cancelled drain waiter. These are
        // requests only; synchronous Drop cannot confirm task completion.
        self.connection_shutdown.finish.cancel();
        self.connection_shutdown.force.cancel();
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

/// Fixed HTTP/2 receive windows for this listener and every
/// `infra_grpc::Client`. h2 0.4 closes a connection with
/// `ENHANCE_YOUR_CALM` once its unread DATA frames under 256 bytes exhaust a
/// budget of half the initial connection window (`DataFrameBudget::Auto`);
/// hyper exposes no other knob for it. Adaptive windows pin that window to
/// 65,535 bytes, so about 140 unread tiny frames (a token-by-token stream, a
/// burst of small records) reset every call on the connection. 8 MiB gives
/// a 4 MiB budget, over 16,000 unread tiny frames. A window is only what a
/// peer may send ahead of the reader, so a stalled connection holds at most
/// the connection window, half the 16 MiB adaptive windows could reach.
pub const HTTP2_CONNECTION_WINDOW: u32 = 8 * 1024 * 1024;
/// Per-stream share of [`HTTP2_CONNECTION_WINDOW`], so one stalled stream
/// cannot take the whole connection. 2 MiB is hyper's client default.
pub const HTTP2_STREAM_WINDOW: u32 = 2 * 1024 * 1024;

/// Concurrent HTTP/2 streams per connection. hyper's default of 200 queues
/// every call beyond it silently on the one channel a service keeps to its
/// neighbour, below the default `max_in_flight` of 256 a caller may send.
/// Admission, which queues a request for a permit until `request_timeout`
/// and then sheds it, is the intended limit; this only has to stay above it.
/// `http.max_in_flight` accepts up to 100,000, so a value above 4096 brings
/// the silent stream queue back: raise this constant with it. Memory is
/// bounded by the connection window, not by the stream count.
const HTTP2_MAX_CONCURRENT_STREAMS: u32 = 4096;

fn connection_builder(options: ServerOptions) -> auto::Builder<TokioExecutor> {
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        // hyper panics when `header_read_timeout` is set without a timer, and
        // `axum::serve` sets none, which is why it cannot bound header reads.
        .timer(TokioTimer::new())
        .header_read_timeout(options.header_read_timeout)
        .max_buf_size(options.max_header_bytes.max(HYPER_MIN_HEADER_BUF))
        .keep_alive(true);
    builder
        .http2()
        .timer(TokioTimer::new())
        // `max_header_bytes` here is the uncompressed HTTP/2 header-list
        // size, not the HTTP/1 read buffer whose overflow is hyper-native 431.
        .max_header_list_size(u32::try_from(options.max_header_bytes).unwrap_or(u32::MAX))
        // HTTP/2 PING keep-alive is not `http.header_read_timeout`: that key
        // is the HTTP/1 idle bound.
        .keep_alive_interval(Some(HTTP2_KEEP_ALIVE_INTERVAL))
        .keep_alive_timeout(HTTP2_KEEP_ALIVE_TIMEOUT)
        .initial_connection_window_size(HTTP2_CONNECTION_WINDOW)
        .initial_stream_window_size(HTTP2_STREAM_WINDOW)
        .max_concurrent_streams(HTTP2_MAX_CONCURRENT_STREAMS);
    builder
}

// A failed preparation closes the connection before hyper owns it.
async fn bind_listener<F, Fut, IO>(
    addr: SocketAddr,
    app: Router,
    options: ServerOptions,
    prepare_io: F,
) -> Result<Server, ServerError>
where
    F: Fn(TcpStream) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Option<(IO, &'static str)>> + Send + 'static,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|source| ServerError::Bind { addr, source })?;
    let local_addr = listener
        .local_addr()
        .map_err(|source| ServerError::Bind { addr, source })?;
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
        // Published at zero, so a scrape tells "never refused" from "not exported".
        metrics::counter!(CONNECTIONS_REFUSED_METRIC).increment(0);
    }
    let (failure, accept_failure) = watch::channel(None);
    let guard = ReportAcceptFailure {
        finish: connection_shutdown.finish.clone(),
        failure,
    };
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
    Ok(Server {
        local_addr,
        accept_loop,
        accept_failure,
        connection_shutdown,
        connections,
    })
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
    Fut: Future<Output = Option<(IO, &'static str)>> + Send + 'static,
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
                // Transient accept errors (EMFILE, ECONNABORTED) recover on
                // their own; back off briefly instead of spinning.
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
                // Over the cap: close without a response. Excess callers
                // otherwise wait in the kernel backlog, which is what the
                // headroom over max_in_flight is for: a connection reset here
                // gets no answer, while a shed request gets 503 with
                // Retry-After.
                metrics::counter!(CONNECTIONS_REFUSED_METRIC).increment(1);
                tracing::debug!(%peer, "http_connection_refused");
                drop(stream);
                continue;
            }
        };
        // HTTP/2 writes headers, data, and trailers as separate small
        // segments; with Nagle on, each can wait for the peer's delayed ACK
        // (about 40 ms). Failing to set it only costs latency.
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
            // Keep admission for connection preparation and the entire hyper
            // connection, releasing it on every exit path.
            let _permit = permit;
            connection.await;
        });
    }
    // Dropping the listener here refuses new connections at once; the
    // accepted ones keep running in their tracked tasks.
    drop(listener);
}

/// One accepted connection from preparation to close: graceful on `finish`
/// or at its age, dropped at once on `force`.
async fn serve_connection<F, Fut, IO>(
    stream: TcpStream,
    peer: SocketAddr,
    prepare_io: F,
    builder: auto::Builder<TokioExecutor>,
    app: Router,
    options: ServerOptions,
    shutdown: ConnectionShutdown,
) where
    F: Fn(TcpStream) -> Fut,
    Fut: Future<Output = Option<(IO, &'static str)>>,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let ConnectionShutdown { finish, force } = shutdown;
    let forced = Arc::new(AtomicBool::new(false));
    let connection_forced = Arc::clone(&forced);
    let serve = async move {
        // No request can be in flight before preparation ends, so drain
        // drops the connection instead of waiting preparation out.
        let io = tokio::select! {
            io = prepare_io(stream) => io,
            () = finish.cancelled() => None,
        };
        let Some((io, scheme)) = io else {
            return;
        };
        let io = SniffDeadline::new(io, options.header_read_timeout);
        // Not `serve_connection_with_upgrades`: hyper hands an upgraded socket
        // to a task outside this one, so drain and the connection cap would
        // stop counting it while it stays open.
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
            ended = connection.as_mut() => ended,
            () = wind_down(&finish, options.max_connection_age) => {
                connection.as_mut().graceful_shutdown();
                connection.await
            }
        };
        if let Err(err) = ended {
            tracing::debug!(%peer, error = %err, "http_connection_failed");
        }
    };
    tokio::select! {
        biased;
        () = async {
            force.cancelled().await;
            // select drops the losing connection future before its branch runs.
            forced.store(true, Ordering::Relaxed);
        } => {},
        () = serve => {},
    }
}

/// Resolves when drain asks connections to finish or this one reaches its age.
async fn wind_down(finish: &CancellationToken, max_connection_age: Option<Duration>) {
    tokio::select! {
        () = finish.cancelled() => {},
        () = reached_age(max_connection_age) => {},
    }
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
    /// Connection ages spread this many permille either way.
    const AGE_SPREAD_PERMILLE: u32 = 100;
    let draw = RandomState::new().build_hasher().finish() % u64::from(2 * AGE_SPREAD_PERMILLE + 1);
    let permille = 1000 - AGE_SPREAD_PERMILLE + u32::try_from(draw).unwrap_or(AGE_SPREAD_PERMILLE);
    age.checked_mul(permille)
        .map_or(age, |scaled| scaled / 1000)
}

#[cfg(test)]
mod tests;
