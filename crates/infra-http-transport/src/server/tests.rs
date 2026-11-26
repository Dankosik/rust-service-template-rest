use std::io;
use std::net::{IpAddr, Ipv4Addr};

use axum::Extension;
use axum::routing::get;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::*;

fn loopback() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

fn options() -> Http1Options {
    Http1Options {
        header_read_timeout: Duration::from_millis(300),
        max_read_buffer_bytes: 8 * 1024,
        connections: ConnectionLimits {
            max_connections: NonZeroU32::new(4),
            max_connection_age: None,
        },
    }
}

fn app() -> Router {
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let routes = Router::new().route("/ok", get(|| async { "ok" }));
    routes
}

async fn fetch(addr: SocketAddr, path: &str) -> String {
    let stream = TcpStream::connect(addr).await.unwrap();
    fetch_http1(stream, path).await
}

async fn fetch_http1(mut stream: impl AsyncRead + AsyncWrite + Unpin, path: &str) -> String {
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

#[tokio::test]
async fn serves_and_drains_completely() {
    let server = Server::bind(loopback(), app(), options()).unwrap();
    let failure = server.accept_failure();
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
async fn a_request_can_cancel_its_own_work_while_listener_drain_reaches_every_child() {
    let (send, mut tokens) = tokio::sync::mpsc::channel(2);
    let handler = move |Extension(info): Extension<ConnectionInfo>| {
        let send = send.clone();
        async move {
            assert_eq!(info.scheme(), TransportScheme::Http);
            send.send(info.child_drain_token()).await.unwrap();
            "ok"
        }
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "transport fixture registers a raw route"
    )]
    let app = Router::new().route("/", get(handler));
    let server = Server::bind(loopback(), app, options()).unwrap();
    for _ in 0..2 {
        assert!(fetch(server.local_addr(), "/").await.ends_with("ok"));
    }
    let first = tokens.recv().await.unwrap();
    let second = tokens.recv().await.unwrap();
    first.cancel();
    assert!(!second.is_cancelled());
    assert!(fetch(server.local_addr(), "/").await.ends_with("ok"));
    let third = tokens.recv().await.unwrap();
    assert_eq!(
        server.drain(Duration::from_secs(2)).await.unwrap(),
        Drained::Complete
    );
    assert!(second.is_cancelled());
    assert!(third.is_cancelled());
}

#[tokio::test]
async fn an_unbounded_drain_budget_drains_completely() {
    let server = Server::bind(loopback(), app(), options()).unwrap();
    assert!(
        fetch(server.local_addr(), "/ok")
            .await
            .starts_with("HTTP/1.1 200 ")
    );
    let drained = server.drain(Duration::MAX).await.unwrap();
    assert_eq!(drained, Drained::Complete);
}

#[tokio::test]
async fn in_flight_request_finishes_during_drain() {
    let (server, mut peer, release) = pending_request(options()).await;
    let graceful = server.connection_shutdown.drain.clone();
    let draining = AbortOnDropHandle::new(tokio::spawn(server.drain(Duration::from_secs(2))));
    tokio::time::timeout(Duration::from_secs(2), graceful.cancelled())
        .await
        .unwrap();
    release.cancel();
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(2), peer.read_to_string(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with("HTTP/1.1 200 "));
    assert!(response.ends_with("finished"));
    assert_eq!(draining.await.unwrap().unwrap(), Drained::Complete);
}

async fn pending_request(options: Http1Options) -> (Server, TcpStream, CancellationToken) {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = CancellationToken::new();
    let handler = {
        let entered = entered.clone();
        let release = release.clone();
        move || {
            let entered = entered.clone();
            let release = release.clone();
            async move {
                entered.notify_one();
                release.cancelled().await;
                "finished"
            }
        }
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let app = Router::new().route("/pending", get(handler));
    let server = Server::bind(loopback(), app, options).unwrap();
    let mut peer = TcpStream::connect(server.local_addr()).await.unwrap();
    peer.write_all(b"GET /pending HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    (server, peer, release)
}

async fn assert_peer_closed(mut peer: impl AsyncRead + Unpin) {
    let mut response = Vec::new();
    let closed =
        tokio::time::timeout(Duration::from_secs(2), peer.read_to_end(&mut response)).await;
    std::assert_matches!(
        closed,
        Ok(Ok(0) | Err(_)),
        "connection remained open: {closed:?}"
    );
    assert_eq!(response, [] as [u8; 0]);
}

#[tokio::test]
async fn dropping_server_forces_in_flight_connection_cleanup() {
    let (server, peer, _) = pending_request(options()).await;
    drop(server);
    assert_peer_closed(peer).await;
}

#[tokio::test]
async fn cancelling_drain_forces_in_flight_connection_cleanup() {
    let (server, peer, _) = pending_request(options()).await;
    let graceful = server.connection_shutdown.drain.clone();
    let draining = tokio::spawn(server.drain(Duration::from_mins(1)));
    tokio::time::timeout(Duration::from_secs(2), graceful.cancelled())
        .await
        .unwrap();
    draining.abort();
    assert!(draining.await.unwrap_err().is_cancelled());
    assert_peer_closed(peer).await;
}

#[tokio::test]
async fn accept_timeout_is_distinct_and_forces_connection_cleanup() {
    let (mut server, peer, _) = pending_request(options()).await;
    let stalled = tokio::spawn(std::future::pending());
    let abort = stalled.abort_handle();
    let accept = std::mem::replace(
        &mut server.accept_loop,
        tokio_util::task::AbortOnDropHandle::new(stalled),
    );
    accept.abort();
    assert!(accept.await.unwrap_err().is_cancelled());
    let now = Instant::now();
    let result = server
        .drain_until(
            now + Duration::from_millis(20),
            now + Duration::from_secs(2),
        )
        .await;
    std::assert_matches!(
        result,
        Err(DrainError::AcceptTimeout { residue }) if residue == DrainResidue::default()
    );
    assert!(abort.is_finished());
    assert_peer_closed(peer).await;
}

#[tokio::test]
async fn expired_confirmation_reports_a_live_accept_producer_even_with_no_connections() {
    let mut server = Server::bind(loopback(), app(), options()).unwrap();
    let stalled = tokio::spawn(std::future::pending::<()>());
    let accept = std::mem::replace(&mut server.accept_loop, AbortOnDropHandle::new(stalled));
    accept.abort();
    assert!(accept.await.unwrap_err().is_cancelled());
    let now = Instant::now();
    let error = server.drain_until(now, now).await.unwrap_err();
    std::assert_matches!(error, DrainError::AcceptTimeout { .. });
    let residue = error.residue();
    assert!(residue.accept_unconfirmed);
    assert_eq!(residue.remaining_connections, 0);
    assert_eq!(residue.remaining_delegated_tasks, 0);
}

#[tokio::test]
async fn accept_failure_stays_sticky_and_drain_preserves_it_while_cleaning_connections() {
    let (server, peer, _) = pending_request(options()).await;
    server.accept_loop.abort();
    let failure = tokio::time::timeout(Duration::from_secs(2), server.accept_failure())
        .await
        .unwrap();
    assert_eq!(failure, AcceptFailure::Ended);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), server.accept_failure())
            .await
            .unwrap(),
        failure
    );
    let result = server.drain(Duration::from_millis(20)).await;
    std::assert_matches!(
        result,
        Err(DrainError::AcceptTask { source, .. }) if source.is_cancelled()
    );
    assert_peer_closed(peer).await;
}

#[tokio::test]
async fn accept_panic_after_stop_is_still_observed() {
    #[derive(Debug)]
    struct PanicOnClone(Arc<std::sync::OnceLock<Arc<CancellationToken>>>);
    impl Clone for PanicOnClone {
        fn clone(&self) -> Self {
            self.0.get().unwrap().cancel();
            panic!("accept preparation clone failed");
        }
    }
    let stop = Arc::new(std::sync::OnceLock::new());
    let marker = PanicOnClone(stop.clone());
    let listener = BoundListener::bind(loopback()).unwrap();
    let server = serve_listener(
        listener,
        app(),
        options().connections,
        http1_driver(options()),
        TransportScheme::Http,
        move |stream, _| {
            let _ = &marker;
            async move { Some(stream) }
        },
    );
    stop.set(server.connection_shutdown.drain.clone()).unwrap();
    let failure = server.accept_failure();
    let peer = TcpStream::connect(server.local_addr()).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), failure)
            .await
            .unwrap(),
        AcceptFailure::Panicked
    );
    std::assert_matches!(
        server.drain(Duration::from_secs(1)).await,
        Err(DrainError::AcceptTask { source, .. }) if source.is_panic()
    );
    drop(peer);
}

#[test]
fn transient_accept_errors_retry_without_the_resource_backoff() {
    for error in [
        io::ErrorKind::ConnectionAborted,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::ConnectionRefused,
        io::ErrorKind::Interrupted,
    ] {
        assert_eq!(accept_retry_delay(&error.into()), None, "{error:?}");
    }
    assert_eq!(
        accept_retry_delay(&io::ErrorKind::OutOfMemory.into()),
        Some(Duration::from_millis(50))
    );
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use nix::errno::Errno;
        assert_eq!(accept_retry_delay(&Errno::EPROTO.into()), None);
        assert_eq!(
            accept_retry_delay(&Errno::EMFILE.into()),
            Some(Duration::from_millis(50))
        );
    }
}

#[tokio::test]
async fn silent_client_is_closed_after_the_header_timeout() {
    let server = Server::bind(loopback(), app(), options()).unwrap();
    let mut silent = TcpStream::connect(server.local_addr()).await.unwrap();
    let mut buf = [0u8; 1];
    let closed = tokio::time::timeout(Duration::from_secs(2), silent.read(&mut buf)).await;
    std::assert_matches!(
        closed,
        Ok(Ok(0)),
        "expected EOF from the server, got {closed:?}"
    );
    let _ = server.drain(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn an_http1_listener_opens_no_http2_connection() {
    let options = Http1Options {
        header_read_timeout: Duration::from_secs(30),
        ..options()
    };
    let server = Server::bind(loopback(), app(), options).unwrap();
    for sent in [&b"PRI * HTTP/2.0\r\n"[..], HTTP2_CLIENT_PREFACE] {
        let peer = TcpStream::connect(server.local_addr()).await.unwrap();
        assert_http2_rejected(peer, sent).await;
    }
    let _ = server.drain(Duration::from_secs(1)).await.unwrap();
}

const HTTP2_CLIENT_PREFACE: &[u8] =
    b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\x00\x00\x00\x04\x00\x00\x00\x00\x00";

async fn assert_http2_rejected(mut peer: impl AsyncRead + AsyncWrite + Unpin, sent: &[u8]) {
    peer.write_all(sent).await.unwrap();
    let mut response = Vec::new();
    let closed =
        tokio::time::timeout(Duration::from_secs(2), peer.read_to_end(&mut response)).await;
    assert!(
        response.is_empty() || response.starts_with(b"HTTP/1."),
        "an HTTP/2 frame was sent: {response:?}"
    );
    if let Err(error) = closed.expect("the rejected connection stayed open") {
        std::assert_matches!(
            error.kind(),
            io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof,
            "{error}"
        );
    }
}

#[tokio::test]
async fn an_http1_listener_reads_the_first_request_head_under_one_timeout() {
    let options = Http1Options {
        header_read_timeout: Duration::from_secs(1),
        ..options()
    };
    let server = Server::bind(loopback(), app(), options).unwrap();
    let mut slow = TcpStream::connect(server.local_addr()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    slow.write_all(b"GET /ok HTTP/1.1\r\n").await.unwrap();
    let mut buf = [0u8; 1];
    let closed = tokio::time::timeout(Duration::from_millis(900), slow.read(&mut buf)).await;
    std::assert_matches!(
        closed,
        Ok(Ok(0)),
        "expected EOF one timeout after accept, got {closed:?}"
    );
    let _ = server.drain(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn drain_does_not_wait_for_a_connection_that_sent_nothing() {
    let mut opts = options();
    opts.header_read_timeout = Duration::from_secs(30);
    let server = Server::bind(loopback(), app(), opts).unwrap();
    let addr = server.local_addr();
    let mut silent = TcpStream::connect(addr).await.unwrap();
    assert!(fetch(addr, "/ok").await.starts_with("HTTP/1.1 200 "));
    let drained = server.drain(Duration::from_secs(2)).await.unwrap();
    assert_eq!(drained, Drained::Complete);
    let mut buf = [0u8; 1];
    let closed = tokio::time::timeout(Duration::from_secs(2), silent.read(&mut buf)).await;
    std::assert_matches!(
        closed,
        Ok(Ok(0)),
        "expected EOF from the server, got {closed:?}"
    );
}

#[tokio::test]
async fn oversized_header_is_431() {
    let server = Server::bind(loopback(), app(), options()).unwrap();
    let mut stream = TcpStream::connect(server.local_addr()).await.unwrap();
    let huge = "x".repeat(9 * 1024);
    let request = format!("GET /ok HTTP/1.1\r\nHost: localhost\r\nX-Big: {huge}\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    let outcome =
        tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut response)).await;
    let text = String::from_utf8_lossy(&response);
    std::assert_matches!(outcome, Ok(Ok(_) | Err(_)), "{outcome:?}");
    assert!(
        text.starts_with("HTTP/1.1 431 "),
        "{}",
        text.get(..text.floor_char_boundary(80)).unwrap_or(&text)
    );
    let _ = server.drain(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn connection_cap_refuses_the_excess() {
    let mut opts = options();
    opts.header_read_timeout = Duration::from_secs(30);
    opts.connections.max_connections = NonZeroU32::new(1);
    let (server, first, _) = pending_request(opts).await;
    let second = TcpStream::connect(server.local_addr()).await.unwrap();
    assert_peer_closed(second).await;
    drop(first);
    let _ = server.drain(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn a_connection_past_its_age_closes_once_idle_and_new_ones_are_served() {
    let mut opts = options();
    opts.header_read_timeout = Duration::from_secs(5);
    opts.connections.max_connection_age = Some(Duration::from_millis(200));
    let server = Server::bind(loopback(), app(), opts).unwrap();
    let addr = server.local_addr();
    let mut kept = TcpStream::connect(addr).await.unwrap();
    kept.write_all(b"GET /ok HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    let closed =
        tokio::time::timeout(Duration::from_secs(2), kept.read_to_end(&mut response)).await;
    std::assert_matches!(closed, Ok(Ok(_)), "{closed:?}");
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
    let spreads = Vec::from_fn(64, |_| jitter_age(age));
    assert!(
        spreads
            .iter()
            .all(|spread| (age * 9 / 10..=age * 11 / 10).contains(spread)),
        "{spreads:?}"
    );
    assert!(spreads.iter().any(|spread| *spread != spreads[0]));
    assert_eq!(jitter_age(Duration::MAX), Duration::MAX);
}

#[tokio::test]
async fn bind_failure_is_reported() {
    let occupied = Server::bind(loopback(), app(), options()).unwrap();
    let err =
        Server::bind(occupied.local_addr(), app(), options()).expect_err("second bind must fail");
    assert_eq!(err.addr(), occupied.local_addr(), "{err}");
    let _ = occupied.drain(Duration::from_secs(1)).await.unwrap();
}

// template:begin grpc:http-server-tls-test
use rustls::pki_types::ServerName;
use tls_test_support::localhost_tls;
use tokio_rustls::TlsConnector;

fn http2_options() -> Http2Options {
    Http2Options {
        preface_timeout: Duration::from_millis(300),
        max_header_list_size: 8 * 1024,
        connections: options().connections,
    }
}

fn hold_stream_handler(
    entered: &CancellationToken,
    held: &std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
) -> &'static str {
    entered.cancel();
    tokio::task::block_in_place(|| {
        held.lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
    });
    "released"
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forced_http2_drain_waits_for_the_stream_handler_or_reports_it_at_cutoff() {
    for confirm in [false, true] {
        let entered = CancellationToken::new();
        let (release, held) = std::sync::mpsc::channel();
        let held = Arc::new(std::sync::Mutex::new(held));
        let handler_entered = entered.clone();
        let handler = move || {
            let entered = handler_entered.clone();
            let held = held.clone();
            async move { hold_stream_handler(&entered, &held) }
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "transport fixture registers a raw route"
        )]
        let app = Router::new().route("/", get(handler));
        let server = BoundListener::bind(loopback())
            .unwrap()
            .serve_h2(app, http2_options(), None);
        let stream = TcpStream::connect(server.local_addr()).await.unwrap();
        let (mut sender, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
                .await
                .unwrap();
        let client = tokio::spawn(connection);
        let request = http::Request::builder()
            .uri("http://localhost/")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = sender.send_request(request);
        tokio::time::timeout(Duration::from_secs(2), entered.cancelled())
            .await
            .unwrap();
        let force = server.connection_shutdown.force.clone();
        let grace = Instant::now() + Duration::from_millis(30);
        let cutoff = if confirm {
            grace + Duration::from_secs(2)
        } else {
            grace
        };
        let mut draining = tokio::spawn(server.drain_until(grace, cutoff));
        force.cancelled().await;
        if confirm {
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut draining)
                    .await
                    .is_err()
            );
            release.send(()).unwrap();
        }
        let outcome = tokio::time::timeout(Duration::from_secs(1), draining)
            .await
            .unwrap()
            .unwrap();
        let Ok(Drained::TimedOut { residue, .. }) = outcome else {
            panic!("expected drain timeout, got {outcome:?}");
        };
        if confirm {
            assert_eq!(residue, DrainResidue::default());
        } else {
            assert_eq!(residue.remaining_delegated_tasks, 1);
            release.send(()).unwrap();
        }
        drop(response);
        drop(sender);
        client.abort();
        let _ = client.await;
    }
}

fn tls_options(config: rustls::ServerConfig) -> TlsOptions {
    TlsOptions {
        config: Arc::new(config),
        handshake_timeout: Duration::from_secs(2),
    }
}

async fn connect_tls(
    addr: SocketAddr,
    config: rustls::ClientConfig,
) -> io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let peer = TcpStream::connect(addr).await?;
    TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("localhost").unwrap(), peer)
        .await
}

fn assert_no_application_protocol(error: &io::Error) {
    std::assert_matches!(
        error
            .get_ref()
            .and_then(|err| err.downcast_ref::<rustls::Error>()),
        Some(rustls::Error::AlertReceived(
            rustls::AlertDescription::NoApplicationProtocol
        )),
        "{error:?}"
    );
}

#[tokio::test]
async fn drain_does_not_wait_for_an_unfinished_tls_handshake() {
    let (server_config, client_config) = localhost_tls(b"h2");
    let options = http2_options();
    let tls = TlsOptions {
        handshake_timeout: Duration::from_secs(30),
        ..tls_options(server_config)
    };
    let server = BoundListener::bind(loopback())
        .unwrap()
        .serve_h2(app(), options, Some(tls));
    let addr = server.local_addr();
    let mut stalled = TcpStream::connect(addr).await.unwrap();
    stalled.write_all(&[0x16]).await.unwrap();
    let later = connect_tls(addr, client_config).await.unwrap();
    drop(later);
    tokio::time::pause();
    let before_drain =
        tokio::time::timeout(options.preface_timeout * 2, stalled.read(&mut [0])).await;
    tokio::time::resume();
    assert!(before_drain.is_err(), "{before_drain:?}");
    let drained = server.drain(Duration::from_secs(2)).await.unwrap();
    assert_eq!(drained, Drained::Complete);
    assert_peer_closed(stalled).await;
}

#[tokio::test]
async fn connection_age_closes_a_socket_still_waiting_for_tls() {
    let (server_config, _) = localhost_tls(b"h2");
    let mut options = http2_options();
    options.connections.max_connection_age = Some(Duration::from_millis(200));
    let tls = TlsOptions {
        handshake_timeout: Duration::from_secs(10),
        ..tls_options(server_config)
    };
    let server = BoundListener::bind(loopback())
        .unwrap()
        .serve_h2(app(), options, Some(tls));
    let client = TcpStream::connect(server.local_addr()).await.unwrap();
    assert_peer_closed(client).await;
    assert_eq!(
        server.drain(Duration::from_secs(1)).await.unwrap(),
        Drained::Complete
    );
}

#[tokio::test]
async fn an_http2_tls_connection_serves_requests_after_the_preface_deadline() {
    use http_body_util::BodyExt as _;

    let (mut server_config, client_config) = localhost_tls(b"h2");
    server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let options = http2_options();
    let server = BoundListener::bind(loopback()).unwrap().serve_h2(
        app(),
        options,
        Some(tls_options(server_config)),
    );
    let addr = server.local_addr();
    let tls = connect_tls(addr, client_config).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let (mut sender, connection) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        hyper_util::rt::TokioIo::new(tls),
    )
    .await
    .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let check_request = async |sender: &mut hyper::client::conn::http2::SendRequest<_>| {
        tokio::time::timeout(Duration::from_secs(2), async {
            let request = http::Request::builder()
                .uri(format!("https://localhost:{}/ok", addr.port()))
                .body(axum::body::Body::empty())
                .unwrap();
            let response = sender.send_request(request).await.unwrap();
            assert_eq!(response.status(), http::StatusCode::OK);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(&*body, b"ok");
        })
        .await
        .unwrap();
    };
    check_request(&mut sender).await;
    tokio::time::pause();
    tokio::time::advance(options.preface_timeout * 2).await;
    tokio::time::resume();
    check_request(&mut sender).await;
    let _ = server.drain(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn an_http2_tls_listener_requires_h2_alpn() {
    let (server_config, mut client_config) = localhost_tls(b"http/1.1");
    let server = BoundListener::bind(loopback()).unwrap().serve_h2(
        app(),
        http2_options(),
        Some(tls_options(server_config)),
    );
    let addr = server.local_addr();
    let error = connect_tls(addr, client_config.clone()).await.unwrap_err();
    assert_no_application_protocol(&error);

    client_config.alpn_protocols.clear();
    let mut tls = connect_tls(addr, client_config).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), None);
    if let Err(error) = tls.write_all(HTTP2_CLIENT_PREFACE).await {
        std::assert_matches!(
            error.kind(),
            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset,
            "{error}"
        );
    }
    assert_peer_closed(tls).await;
    let _ = server.drain(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn an_http1_tls_listener_rejects_http2_and_allows_absent_alpn() {
    let (server_config, mut client_config) = localhost_tls(b"h2");
    let server = BoundListener::bind(loopback()).unwrap().serve_tls(
        app(),
        Http1Options {
            header_read_timeout: Duration::from_secs(2),
            ..options()
        },
        tls_options(server_config),
    );
    let addr = server.local_addr();
    let error = connect_tls(addr, client_config.clone()).await.unwrap_err();
    assert_no_application_protocol(&error);

    client_config.alpn_protocols.push(b"http/1.1".to_vec());
    let tls = connect_tls(addr, client_config.clone()).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
    assert_http2_rejected(tls, HTTP2_CLIENT_PREFACE).await;

    client_config.alpn_protocols.clear();
    let tls = connect_tls(addr, client_config).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), None);
    let response = fetch_http1(tls, "/ok").await;
    assert!(response.starts_with("HTTP/1.1 200 "));
    assert!(response.ends_with("ok"));
    let _ = server.drain(Duration::from_secs(1)).await.unwrap();
}

async fn read_http2_frame(peer: &mut TcpStream) -> ([u8; 9], Vec<u8>) {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut header = [0; 9];
        peer.read_exact(&mut header).await.unwrap();
        let length = u32::from_be_bytes([0, header[0], header[1], header[2]]);
        assert!(length <= 16_384);
        let mut payload = vec![0; length as usize];
        peer.read_exact(&mut payload).await.unwrap();
        (header, payload)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn an_http2_listener_negotiates_a_segmented_preface_and_advertises_the_header_limit() {
    let options = Http2Options {
        preface_timeout: Duration::from_secs(2),
        max_header_list_size: 12_345,
        ..http2_options()
    };
    let server = BoundListener::bind(loopback())
        .unwrap()
        .serve_h2(app(), options, None);
    let mut peer = TcpStream::connect(server.local_addr()).await.unwrap();
    let (first, last) = HTTP2_CLIENT_PREFACE.split_at(16);
    peer.write_all(first).await.unwrap();
    let (settings, payload) = read_http2_frame(&mut peer).await;
    assert_eq!(settings[3..], [4, 0, 0, 0, 0, 0]);
    let [a, b, c, d] = options.max_header_list_size.to_be_bytes();
    assert!(
        payload.as_chunks::<6>().0.contains(&[0, 6, a, b, c, d]),
        "the advertised SETTINGS omitted the header list limit: {payload:?}"
    );

    peer.write_all(last).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let (header, payload) = read_http2_frame(&mut peer).await;
            if header[3] == 4 {
                assert_eq!(header, [0, 0, 0, 4, 1, 0, 0, 0, 0]);
                assert_eq!(payload, [] as [u8; 0]);
                break;
            }
        }
    })
    .await
    .expect("the server did not acknowledge the client's SETTINGS");
    drop(peer);
    let _ = server.drain(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn stalled_http2_prefaces_close_after_the_preface_timeout() {
    let server = BoundListener::bind(loopback())
        .unwrap()
        .serve_h2(app(), http2_options(), None);
    let mut peers = Vec::new();
    for sent in [&b""[..], b"P", &HTTP2_CLIENT_PREFACE[..23]] {
        let mut peer = TcpStream::connect(server.local_addr()).await.unwrap();
        peer.write_all(sent).await.unwrap();
        let (settings, _) = read_http2_frame(&mut peer).await;
        assert_eq!(settings[3..], [4, 0, 0, 0, 0, 0]);
        peers.push(peer);
    }
    for peer in peers {
        assert_peer_closed(peer).await;
    }
    let _ = server.drain(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn drain_closes_an_http2_connection_already_waiting_for_the_preface() {
    let options = Http2Options {
        preface_timeout: Duration::from_secs(30),
        ..http2_options()
    };
    let server = BoundListener::bind(loopback())
        .unwrap()
        .serve_h2(app(), options, None);
    let mut stalled = TcpStream::connect(server.local_addr()).await.unwrap();
    stalled.write_all(b"P").await.unwrap();
    let (settings, _) = read_http2_frame(&mut stalled).await;
    assert_eq!(settings[3..], [4, 0, 0, 0, 0, 0]);
    assert_eq!(
        server.drain(Duration::from_secs(1)).await.unwrap(),
        Drained::Complete
    );
    assert_peer_closed(stalled).await;
}

#[tokio::test]
async fn an_http2_only_listener_answers_no_http1_request() {
    let server = BoundListener::bind(loopback())
        .unwrap()
        .serve_h2(app(), http2_options(), None);
    let mut peer = TcpStream::connect(server.local_addr()).await.unwrap();
    peer.write_all(b"GET /ok HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    let closed =
        tokio::time::timeout(Duration::from_secs(2), peer.read_to_end(&mut response)).await;
    assert!(closed.is_ok(), "the connection stayed open");
    assert!(
        !response.starts_with(b"HTTP/1."),
        "an HTTP/1 response was sent: {:?}",
        String::from_utf8_lossy(&response)
    );
    let _ = server.drain(Duration::from_secs(1)).await.unwrap();
}
// template:end grpc:http-server-tls-test

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_burst_of_connections_waits_in_the_listen_queue_instead_of_being_dropped() {
    let listener = BoundListener::bind(loopback()).unwrap();
    let addr = listener.local_addr;
    let mut pending = tokio::task::JoinSet::new();
    for _ in 0..300 {
        pending.spawn(tokio::time::timeout(
            Duration::from_millis(900),
            TcpStream::connect(addr),
        ));
    }
    let connected = pending
        .join_all()
        .await
        .into_iter()
        .filter(|outcome| matches!(outcome, Ok(Ok(_))))
        .count();
    assert_eq!(
        connected, 300,
        "the kernel drops the handshake of a connection that finds the queue full, and the client retries a second later"
    );
    drop(listener);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn an_accepted_connection_carries_the_user_timeout_and_no_delay() {
    let listener = TcpListener::bind(loopback()).await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (accepted, _) = listener.accept().await.unwrap();

    configure_accepted(&accepted);

    let socket = socket2::SockRef::from(&accepted);
    assert_eq!(
        socket.tcp_user_timeout().unwrap(),
        Some(Duration::from_secs(30))
    );
    assert!(accepted.nodelay().unwrap());
    drop(client);
}
