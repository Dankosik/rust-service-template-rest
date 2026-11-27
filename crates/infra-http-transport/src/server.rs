use std::mem::DropGuard;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use hyper::rt::Executor;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use shared_rules::hyper::HTTP1_MIN_READ_BUFFER_BYTES;
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;
use tokio_util::task::{AbortOnDropHandle, TaskTracker};
use tower::ServiceExt as _;

pub(crate) const CONNECTIONS_REFUSED_METRIC: &str = "http_server_connections_refused_total";

static CONNECTIONS_METRIC_METADATA: metrics::Metadata<'static> =
    metrics::Metadata::new(module_path!(), metrics::Level::INFO, Some(module_path!()));
static CONNECTIONS_REFUSED_KEY: metrics::Key =
    metrics::Key::from_static_name(CONNECTIONS_REFUSED_METRIC);

fn connections_refused_counter() -> metrics::Counter {
    metrics::with_recorder(|recorder| {
        recorder.register_counter(&CONNECTIONS_REFUSED_KEY, &CONNECTIONS_METRIC_METADATA)
    })
}

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
#[error("bind http listener {addr}: {source}")]
pub struct BindError {
    addr: SocketAddr,
    #[source]
    source: std::io::Error,
}

impl BindError {
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DrainError {
    #[error("http accept loop task: {source}")]
    AcceptTask {
        #[source]
        source: tokio::task::JoinError,
        residue: DrainResidue,
    },
    #[error("http accept loop termination was not confirmed before the drain deadline")]
    AcceptTimeout { residue: DrainResidue },
}

impl DrainError {
    #[must_use]
    pub const fn residue(&self) -> DrainResidue {
        match *self {
            Self::AcceptTask { residue, .. } | Self::AcceptTimeout { residue } => residue,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptFailure {
    Ended,
    Panicked,
}

#[must_use = "inspect drain outcome to detect timed-out shutdown"]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drained {
    Complete,
    TimedOut {
        remaining_at_deadline: usize,
        residue: DrainResidue,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainResidue {
    pub accept_unconfirmed: bool,
    pub remaining_connections: usize,
    pub remaining_delegated_tasks: usize,
}

#[must_use = "dropping Server aborts the HTTP accept loop immediately"]
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
    drain: Arc<CancellationToken>,
    force: CancellationToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransportScheme {
    Http,
    Https,
}

impl TransportScheme {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    #[must_use]
    pub const fn is_tls(self) -> bool {
        matches!(self, Self::Https)
    }
}

impl std::fmt::Display for TransportScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct ConnectionInfo {
    scheme: TransportScheme,
    state: Arc<ConnectionState>,
}

#[derive(Debug)]
struct ConnectionState {
    forced: AtomicBool,
    drain: Arc<CancellationToken>,
}

impl ConnectionInfo {
    #[must_use]
    pub fn child_drain_token(&self) -> CancellationToken {
        self.state.drain.child_token()
    }

    #[must_use]
    pub const fn scheme(&self) -> TransportScheme {
        self.scheme
    }

    #[must_use]
    pub fn was_forced(&self) -> bool {
        self.state.forced.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
pub struct BoundListener {
    listener: TcpListener,
    local_addr: SocketAddr,
}

impl BoundListener {
    pub fn bind(addr: SocketAddr) -> Result<Self, BindError> {
        listen(addr).map_err(|source| BindError { addr, source })
    }

    pub fn serve(self, app: Router, options: Http1Options) -> Server {
        serve_plaintext(self, app, options.connections, http1_driver(options))
    }

    // template:begin grpc:http-bound-listener-tls
    pub fn serve_tls(self, app: Router, options: Http1Options, tls: TlsOptions) -> Server {
        serve_tls(self, app, options.connections, http1_driver(options), tls)
    }

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
    serve_listener(
        bound,
        app,
        limits,
        driver,
        TransportScheme::Http,
        |stream, _| async move { Some(stream) },
    )
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
    let protocol = driver.protocol;
    let alpn = protocol.alpn();
    if config.alpn_protocols.as_slice() != [alpn] {
        Arc::make_mut(&mut config).alpn_protocols = vec![alpn.to_vec()];
    }
    let acceptor = tokio_rustls::TlsAcceptor::from(config);
    serve_listener(
        bound,
        app,
        limits,
        driver,
        TransportScheme::Https,
        move |stream, peer| accept_tls(acceptor, stream, peer, handshake_timeout, protocol),
    )
}

async fn accept_tls(
    acceptor: tokio_rustls::TlsAcceptor,
    stream: TcpStream,
    peer: SocketAddr,
    handshake_timeout: Duration,
    protocol: DriverProtocol,
) -> Option<tokio_rustls::server::TlsStream<TcpStream>> {
    let tls_stream = match tokio::time::timeout(handshake_timeout, acceptor.accept(stream)).await {
        Ok(Ok(tls_stream)) => tls_stream,
        Ok(Err(error)) => {
            tracing::debug!(%peer, %error, "http_tls_handshake_failed");
            return None;
        }
        Err(_) => {
            tracing::debug!(%peer, "http_tls_handshake_timed_out");
            return None;
        }
    };
    if !protocol.accepts_tls_alpn(tls_stream.get_ref().1.alpn_protocol()) {
        tracing::debug!(%peer, "http2_alpn_missing");
        return None;
    }
    Some(tls_stream)
}
// template:end grpc:http-serve-tls

fn listen(addr: SocketAddr) -> std::io::Result<BoundListener> {
    let socket = match addr {
        SocketAddr::V4(_) => TcpSocket::new_v4(),
        SocketAddr::V6(_) => TcpSocket::new_v6(),
    }?;
    #[cfg(not(windows))]
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    let listener = socket.listen(LISTEN_BACKLOG)?;
    let local_addr = listener.local_addr()?;
    Ok(BoundListener {
        listener,
        local_addr,
    })
}

fn configure_accepted(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    let _ = socket2::SockRef::from(stream).set_tcp_user_timeout(Some(ACCEPTED_TCP_USER_TIMEOUT));
}

impl Server {
    pub fn bind(addr: SocketAddr, app: Router, options: Http1Options) -> Result<Self, BindError> {
        Ok(BoundListener::bind(addr)?.serve(app, options))
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn accept_failure(&self) -> impl Future<Output = AcceptFailure> + Send + 'static + use<> {
        let mut receiver = self.accept_failure.clone();
        async move {
            match receiver
                .wait_for(Option::is_some)
                .await
                .ok()
                .and_then(|seen| *seen)
            {
                Some(failure) => failure,
                None => std::future::pending().await,
            }
        }
    }

    pub async fn drain(self, budget: Duration) -> Result<Drained, DrainError> {
        let deadline = deadline_after(budget);
        self.drain_until(deadline, deadline).await
    }

    pub async fn drain_until(
        mut self,
        graceful_deadline: Instant,
        confirmation_deadline: Instant,
    ) -> Result<Drained, DrainError> {
        enum GracefulStop {
            AcceptTimeout,
            AcceptTask(tokio::task::JoinError),
            TimedOut(usize),
        }

        self.connection_shutdown.drain.cancel();
        let stop = match self.join_accept_until(graceful_deadline).await {
            None => {
                self.accept_loop.abort();
                GracefulStop::AcceptTimeout
            }
            Some(Err(error)) => GracefulStop::AcceptTask(error),
            Some(Ok(())) => {
                if self.drain_tasks_until(graceful_deadline).await {
                    return Ok(Drained::Complete);
                }
                GracefulStop::TimedOut(self.connections.len())
            }
        };
        self.connection_shutdown.force.cancel();
        let accept_confirmed = !matches!(stop, GracefulStop::AcceptTimeout)
            || self
                .join_accept_until(confirmation_deadline)
                .await
                .is_some();
        let connections_stopped =
            accept_confirmed && close_and_wait(&self.connections, confirmation_deadline).await;
        self.delegated.force.cancel();
        if connections_stopped {
            close_and_wait(&self.delegated.tasks, confirmation_deadline).await;
        }
        let residue = DrainResidue {
            accept_unconfirmed: !accept_confirmed,
            remaining_connections: self.connections.len(),
            remaining_delegated_tasks: self.delegated.tasks.len(),
        };
        match stop {
            GracefulStop::AcceptTimeout => Err(DrainError::AcceptTimeout { residue }),
            GracefulStop::AcceptTask(source) => Err(DrainError::AcceptTask { source, residue }),
            GracefulStop::TimedOut(remaining_at_deadline) => Ok(Drained::TimedOut {
                remaining_at_deadline,
                residue,
            }),
        }
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

    async fn drain_tasks_until(&self, deadline: Instant) -> bool {
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
        self.connection_shutdown.drain.cancel();
        self.connection_shutdown.force.cancel();
        self.delegated.force.cancel();
    }
}

#[derive(Clone, Debug, Default)]
struct ListenerExecutor {
    tasks: TaskTracker,
    force: Arc<CancellationToken>,
}

impl<F: Future + Send + 'static> Executor<F> for ListenerExecutor {
    fn execute(&self, future: F) {
        let force = Arc::clone(&self.force);
        let task = self.tasks.track_future(async move {
            force.run_until_cancelled(future).await;
        });
        TokioExecutor::new().execute(task);
    }
}

struct ConnectionDriver {
    builder: auto::Builder<ListenerExecutor>,
    executor: ListenerExecutor,
    // template:begin grpc:http2-driver-preface-budget
    protocol: DriverProtocol,
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
        protocol: DriverProtocol::Http1,
        // template:end grpc:http1-driver-no-preface
    }
}

// template:begin grpc:http2-driver
#[derive(Clone, Copy)]
enum DriverProtocol {
    Http1,
    Http2 { preface_timeout: Duration },
}

impl DriverProtocol {
    const fn alpn(self) -> &'static [u8] {
        match self {
            Self::Http1 => b"http/1.1",
            Self::Http2 { .. } => b"h2",
        }
    }

    fn accepts_tls_alpn(self, negotiated: Option<&[u8]>) -> bool {
        match self {
            Self::Http1 => true,
            Self::Http2 { .. } => negotiated == Some(self.alpn()),
        }
    }

    const fn preface_timeout(self) -> Option<Duration> {
        match self {
            Self::Http1 => None,
            Self::Http2 { preface_timeout } => Some(preface_timeout),
        }
    }
}

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
        protocol: DriverProtocol::Http2 {
            preface_timeout: options.preface_timeout,
        },
    }
}

fn observe_http2_preface<IO>(
    io: IO,
    timeout: Option<Duration>,
) -> (
    impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    Option<(Instant, tokio::sync::oneshot::Receiver<()>)>,
)
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    // RFC 9113 §3.4.
    const HTTP2_CLIENT_PREFACE_BYTES: usize = 24;

    let Some(timeout) = timeout else {
        return (tokio_util::either::Either::Right(io), None);
    };
    let (notify, received) = tokio::sync::oneshot::channel();
    let mut notify = Some(notify);
    let mut remaining_bytes = HTTP2_CLIENT_PREFACE_BYTES;
    let io = tokio_util::io::InspectReader::new(io, move |bytes| {
        remaining_bytes = remaining_bytes.saturating_sub(bytes.len());
        if remaining_bytes == 0
            && let Some(notify) = notify.take()
        {
            let _ = notify.send(());
        }
    });
    (
        tokio_util::either::Either::Left(io),
        Some((deadline_after(timeout), received)),
    )
}
// template:end grpc:http2-driver

struct ConnectionContext {
    app: Router,
    driver: ConnectionDriver,
    max_connection_age: Option<Duration>,
    shutdown: ConnectionShutdown,
    scheme: TransportScheme,
}

fn serve_listener<F, Fut, IO>(
    bound: BoundListener,
    app: Router,
    limits: ConnectionLimits,
    driver: ConnectionDriver,
    scheme: TransportScheme,
    prepare_io: F,
) -> Server
where
    F: FnOnce(TcpStream, SocketAddr) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Option<IO>> + Send + 'static,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let local_addr = bound.local_addr;
    let connection_shutdown = ConnectionShutdown {
        drain: Arc::new(CancellationToken::new()),
        force: CancellationToken::new(),
    };
    let connections = TaskTracker::new();
    describe_connections_refused(limits.max_connections.is_some());
    let (accept_failure_guard, accept_failure) =
        accept_failure_signal(Arc::clone(&connection_shutdown.drain));
    let delegated = driver.executor.clone();
    let context = Arc::new(ConnectionContext {
        app,
        driver,
        max_connection_age: limits.max_connection_age,
        shutdown: connection_shutdown.clone(),
        scheme,
    });
    let accept = accept_loop(
        bound,
        limits.max_connections,
        connections.clone(),
        context,
        prepare_io,
    );
    let accept_loop = AbortOnDropHandle::new(tokio::spawn(async move {
        let _accept_failure_guard = accept_failure_guard;
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

fn describe_connections_refused(capped: bool) {
    metrics::describe_counter!(
        CONNECTIONS_REFUSED_METRIC,
        metrics::Unit::Count,
        "Accepted connections closed without a response because the connection cap was reached."
    );
    if capped {
        connections_refused_counter().increment(0);
    }
}

type AcceptFailureReporter = (Arc<CancellationToken>, watch::Sender<Option<AcceptFailure>>);

fn accept_failure_signal(
    drain: Arc<CancellationToken>,
) -> (
    DropGuard<AcceptFailureReporter, impl FnOnce(AcceptFailureReporter)>,
    watch::Receiver<Option<AcceptFailure>>,
) {
    let (accept_failure_tx, accept_failure) = watch::channel(None);
    let guard = DropGuard::new((drain, accept_failure_tx), |(drain, accept_failure_tx)| {
        let observed = if std::thread::panicking() {
            AcceptFailure::Panicked
        } else if !drain.is_cancelled() {
            AcceptFailure::Ended
        } else {
            return;
        };
        accept_failure_tx.send_replace(Some(observed));
    });
    (guard, accept_failure)
}

async fn accept_loop<F, Fut, IO>(
    BoundListener {
        listener,
        local_addr,
    }: BoundListener,
    max_connections: Option<NonZeroU32>,
    connections: TaskTracker,
    context: Arc<ConnectionContext>,
    prepare_io: F,
) where
    F: FnOnce(TcpStream, SocketAddr) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Option<IO>> + Send + 'static,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let draining = context.shutdown.drain.cancelled();
    let mut draining = std::pin::pin!(draining);
    let permits = max_connections.map(|limit| Arc::new(Semaphore::new(limit.get() as usize)));

    loop {
        let accepted = tokio::select! {
            () = draining.as_mut() => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(err) => {
                let Some(delay) = accept_retry_delay(&err) else {
                    continue;
                };
                tracing::warn!(error = %err, "http_accept_failed");
                tokio::select! {
                    () = draining.as_mut() => break,
                    () = tokio::time::sleep(delay) => {}
                }
                continue;
            }
        };
        let Ok(permit) = permits
            .as_ref()
            .map(|semaphore| Arc::clone(semaphore).try_acquire_owned())
            .transpose()
        else {
            connections_refused_counter().increment(1);
            tracing::debug!(%local_addr, %peer, "http_connection_refused");
            drop(stream);
            continue;
        };
        configure_accepted(&stream);
        connections.spawn(serve_connection(
            stream,
            peer,
            permit,
            Arc::clone(&context),
            prepare_io.clone(),
        ));
    }
}

fn accept_retry_delay(error: &std::io::Error) -> Option<Duration> {
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
            return None;
        }
    }
    match error.kind() {
        ConnectionAborted | ConnectionRefused | ConnectionReset | Interrupted => None,
        _ => Some(Duration::from_millis(50)),
    }
}

fn serve_connection<F, Fut, IO>(
    stream: TcpStream,
    peer: SocketAddr,
    permit: Option<OwnedSemaphorePermit>,
    context: Arc<ConnectionContext>,
    prepare_io: F,
) -> impl Future<Output = ()>
where
    F: FnOnce(TcpStream, SocketAddr) -> Fut,
    Fut: Future<Output = Option<IO>>,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let age_deadline = context
        .max_connection_age
        .map(|age| deadline_after(jitter_age(age)));
    async move {
        let _permit = permit;
        let info = ConnectionInfo {
            scheme: context.scheme,
            state: Arc::new(ConnectionState {
                forced: AtomicBool::new(false),
                drain: Arc::clone(&context.shutdown.drain),
            }),
        };
        let serve = drive_connection(stream, peer, &context, age_deadline, &info, prepare_io);
        tokio::select! {
            biased;
            () = async {
                context.shutdown.force.cancelled().await;
                info.state.forced.store(true, Ordering::Relaxed);
            } => {},
            ended = serve => {
                if let Err(err) = ended {
                    tracing::debug!(%peer, error = %err, "http_connection_failed");
                }
            },
        }
    }
}

async fn drive_connection<F, Fut, IO>(
    stream: TcpStream,
    peer: SocketAddr,
    context: &ConnectionContext,
    age_deadline: Option<Instant>,
    info: &ConnectionInfo,
    prepare_io: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    F: FnOnce(TcpStream, SocketAddr) -> Fut,
    Fut: Future<Output = Option<IO>>,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let closing = wait_for_close(&context.shutdown.drain, age_deadline);
    let mut closing = std::pin::pin!(closing);
    let io = tokio::select! {
        biased;
        () = closing.as_mut() => None,
        io = prepare_io(stream, peer) => io,
    };
    let Some(io) = io else {
        return Ok(());
    };
    // template:begin grpc:http2-observe-initial-read
    let (io, preface) = observe_http2_preface(io, context.driver.protocol.preface_timeout());
    // template:end grpc:http2-observe-initial-read
    let service =
        hyper::service::service_fn(|mut request: hyper::Request<hyper::body::Incoming>| {
            request.extensions_mut().insert(info.clone());
            context.app.clone().oneshot(request)
        });
    let connection = context
        .driver
        .builder
        .serve_connection(TokioIo::new(io), service);
    let mut connection = std::pin::pin!(connection);
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
}

async fn wait_for_close(drain: &CancellationToken, age_deadline: Option<Instant>) {
    if let Some(deadline) = age_deadline {
        drain
            .run_until_cancelled(tokio::time::sleep_until(deadline))
            .await;
    } else {
        drain.cancelled().await;
    }
}

fn jitter_age(age: Duration) -> Duration {
    const AGE_SPREAD_PERMILLE: u32 = 100;
    let permille = fastrand::u32(1000 - AGE_SPREAD_PERMILLE..=1000 + AGE_SPREAD_PERMILLE);
    age.checked_mul(permille)
        .map_or(age, |scaled| scaled / 1000)
}

#[cfg(test)]
mod tests;
