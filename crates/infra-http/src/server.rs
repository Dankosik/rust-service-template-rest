//! Bounded HTTP server over hyper.
//!
//! `axum::serve` is intentionally unconfigurable: it sets no timer, so
//! hyper's header-read timeout is silently disabled, and it exposes no
//! header-size or connection bounds. This accept loop owns those limits and
//! the graceful drain; the composition root decides when to stop and how
//! long to wait.

use std::future::Future;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Connection-level policy the router cannot express.
#[derive(Clone, Copy, Debug)]
pub struct ServerOptions {
    /// Time a connection may take to deliver a complete request head. hyper
    /// restarts it whenever an HTTP/1 connection goes idle, so it is also
    /// the HTTP/1 keep-alive idle bound. Also bounds a client that connects
    /// and sends nothing, which hyper's protocol sniff would otherwise leave
    /// open forever. The accept loop peeks for that first byte with this
    /// same duration, then hyper's timer starts for the rest of the head:
    /// a slow first byte plus a slow remainder can add, not replace.
    /// HTTP/2 idle uses a separate PING cadence, not this value.
    pub header_read_timeout: Duration,
    /// Read-buffer ceiling for one request head. HTTP/1 overflow answers
    /// hyper-native 431 before the router (not a [`crate::Problem`]). HTTP/2
    /// applies the same number as uncompressed header-list size.
    /// hyper refuses HTTP/1 values below 8 KiB.
    pub max_header_bytes: usize,
    /// Accepted connections at once; `None` accepts without a bound.
    pub max_connections: Option<NonZeroU32>,
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
}

/// How the drain ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drained {
    /// Every connection closed inside the budget.
    Complete,
    /// The budget expired with connections still open. Those tasks are not
    /// in the process `TaskTracker`. `pool.close` waits for any pooled
    /// connections they still hold; the composition root's
    /// `runtime.shutdown_timeout` is the last drop if they outlive close.
    /// `remaining_connections` is the count at cancel, not after the wait.
    TimedOut { remaining_connections: usize },
}

/// A bound, serving HTTP server.
#[derive(Debug)]
pub struct Server {
    local_addr: SocketAddr,
    stop_accepting: CancellationToken,
    accept_loop: Option<JoinHandle<GracefulShutdown>>,
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
    /// The handshake runs after the first-byte peek and is bounded by
    /// [`ServerOptions::header_read_timeout`]. A handshake error or timeout
    /// closes the connection without a response.
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

    /// Stop accepting, tell every connection to finish its current request
    /// and close, and wait up to `budget` for them to do so.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::AcceptTask`] when the accept loop panicked.
    pub async fn drain(mut self, budget: Duration) -> Result<Drained, ServerError> {
        self.stop_accepting.cancel();
        let Some(accept_loop) = self.accept_loop.take() else {
            return Ok(Drained::Complete);
        };
        let graceful = accept_loop.await.map_err(ServerError::AcceptTask)?;
        let remaining_connections = graceful.count();
        match tokio::time::timeout(budget, graceful.shutdown()).await {
            Ok(()) => Ok(Drained::Complete),
            Err(_elapsed) => Ok(Drained::TimedOut {
                remaining_connections,
            }),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Reached only when `drain` was not awaited: stop accepting so the
        // listener is released, but do not abort in-flight connections. This
        // is the failed-bind / partial-startup path, not the ordered drain.
        // `take()` detaches the accept task; Drop must not wait for drain.
        self.stop_accepting.cancel();
        drop(self.accept_loop.take());
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
    let accept_loop = tokio::spawn(accept_loop(
        listener,
        app,
        options,
        stop_accepting.clone(),
        prepare_io,
    ));
    Ok(Server {
        local_addr,
        stop_accepting,
        accept_loop: Some(accept_loop),
    })
}

async fn accept_loop<F, Fut, IO>(
    listener: TcpListener,
    app: Router,
    options: ServerOptions,
    stop: CancellationToken,
    prepare_io: F,
) -> GracefulShutdown
where
    F: Fn(TcpStream) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Option<IO>> + Send + 'static,
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let builder = connection_builder(options);
    let graceful = GracefulShutdown::new();
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
        let watcher = graceful.watcher();
        let builder = builder.clone();
        let app = app.clone();
        let prepare_io = prepare_io.clone();
        // HTTP/2 writes headers, data, and trailers as separate small
        // segments; with Nagle on, each can wait for the peer's delayed ACK
        // (about 40 ms). Failing to set it only costs latency.
        let _ = stream.set_nodelay(true);
        tokio::spawn(async move {
            // Keep admission for the first-byte wait, TLS handshake, and the
            // entire hyper connection, releasing it on every exit path.
            let _permit = permit;
            if !wait_for_first_byte(&stream, options.header_read_timeout).await {
                return;
            }
            let Some(io) = prepare_io(stream).await else {
                return;
            };
            let connection = builder
                .serve_connection_with_upgrades(TokioIo::new(io), TowerToHyperService::new(app))
                .into_owned();
            if let Err(err) = watcher.watch(connection).await {
                tracing::debug!(%peer, error = %err, "connection ended with error");
            }
        });
    }
    // Dropping the listener here refuses new connections at once; the
    // accepted ones keep running under their watchers.
    drop(listener);
    graceful
}

/// Counter of connections closed at accept because `max_connections` was
/// reached.
pub const CONNECTIONS_REFUSED_METRIC: &str = "http_server_connections_refused_total";

/// hyper's protocol sniff reads the first bytes without a timer, so a client
/// that connects and stays silent is never timed out (hyper #3756). Peek
/// with our own bound before handing the socket over. The first byte must
/// stay queued for that sniff: this is `peek`, not `read`. Peek only
/// waits for the first queued byte; hyper's header-read timer starts
/// after handoff, so the operator key is shared and the two waits add.
/// `true` means a byte is available and still in the stream; timeout,
/// peek I/O error, and EOF are the same close-without-response.
async fn wait_for_first_byte(stream: &TcpStream, timeout: Duration) -> bool {
    let mut probe = [0u8; 1];
    match tokio::time::timeout(timeout, stream.peek(&mut probe)).await {
        Ok(Ok(read)) => read > 0,
        Ok(Err(_)) | Err(_) => false,
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
        }
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "connection-level fixture is outside application contract authoring"
    )]
    fn app() -> Router {
        Router::new().route("/ok", get(|| async { "ok" })).route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(400)).await;
                "late"
            }),
        )
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
        let addr = server.local_addr();
        assert!(fetch(addr, "/ok").await.starts_with("HTTP/1.1 200 "));
        let drained = server.drain(Duration::from_secs(2)).await.unwrap();
        assert_eq!(drained, Drained::Complete);
        assert!(
            TcpStream::connect(addr).await.is_err(),
            "listener still open"
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
    async fn drain_budget_expiry_is_reported() {
        let server = Server::bind(loopback(), app(), options()).await.unwrap();
        let addr = server.local_addr();
        let slow = tokio::spawn(async move { fetch(addr, "/slow").await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let drained = server.drain(Duration::from_millis(50)).await.unwrap();
        assert_eq!(
            drained,
            Drained::TimedOut {
                remaining_connections: 1
            }
        );
        slow.abort();
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
    async fn bind_failure_is_reported() {
        let occupied = Server::bind(loopback(), app(), options()).await.unwrap();
        let err = Server::bind(occupied.local_addr(), app(), options())
            .await
            .expect_err("second bind must fail");
        assert!(matches!(err, ServerError::Bind { .. }), "{err}");
        occupied.drain(Duration::from_secs(1)).await.unwrap();
    }

    // template:begin grpc:http-server-tls-test
    #[tokio::test]
    async fn tls_listener_serves_http2() {
        use std::sync::Arc;

        use http_body_util::BodyExt as _;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
        use rustls::{ClientConfig, RootCertStore, ServerConfig};
        use tokio_rustls::TlsConnector;

        fn localhost_tls() -> (ServerConfig, ClientConfig) {
            use rcgen::{
                BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose,
                IsCa, KeyPair, KeyUsagePurpose,
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
