use std::io;
use std::net::{IpAddr, Ipv4Addr};

use axum::routing::get;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::sniff::HTTP2_PREFACE;
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
    #[expect(
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
    let server = Server::bind(loopback(), app(), options()).unwrap();
    let addr = server.local_addr();
    let slow = tokio::spawn(async move { fetch(addr, "/slow").await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let drained = server.drain(Duration::from_secs(2)).await.unwrap();
    assert_eq!(drained, Drained::Complete);
    assert!(slow.await.unwrap().ends_with("late"));
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
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let app = Router::new().route("/pending", get(handler_1));
    let server = Server::bind(loopback(), app, options()).unwrap();
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
    std::assert_matches!(
        closed,
        Ok(Ok(0) | Err(_)),
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
    let (mut server, peer) = pending_request().await;
    let stalled = tokio::spawn(std::future::pending());
    let abort = stalled.abort_handle();
    let accept = std::mem::replace(
        &mut server.accept_loop,
        tokio_util::task::AbortOnDropHandle::new(stalled),
    );
    accept.abort();
    assert!(accept.await.unwrap_err().is_cancelled());
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        server.drain(Duration::from_millis(20)),
    )
    .await
    .unwrap();
    std::assert_matches!(result, Err(ServerError::AcceptTimeout));
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
    std::assert_matches!(result, Err(ServerError::AcceptTask(error)) if error.is_cancelled());
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
    let listener = BoundListener::bind(loopback()).unwrap();
    let server = serve_listener(
        listener,
        app(),
        options(),
        connection_builder(options()),
        move |stream| {
            let _ = &marker;
            async move { Some((stream, ListenerScheme::Http)) }
        },
    );
    stop.set(server.connection_shutdown.finish.clone()).unwrap();
    let failure = server.accept_failure();
    let peer = TcpStream::connect(server.local_addr()).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), failure)
            .await
            .unwrap(),
        AcceptFailure::Panicked
    );
    std::assert_matches!(server.drain(Duration::from_secs(1)).await, Err(ServerError::AcceptTask(error)) if error.is_panic()
    );
    drop(peer);
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
    server.drain(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn an_http1_listener_opens_no_http2_connection() {
    let options = ServerOptions {
        header_read_timeout: Duration::from_secs(30),
        ..options()
    };
    let server = Server::bind(loopback(), app(), options).unwrap();
    for sent in [&b"PRI * HTTP/2.0\r\n"[..], HTTP2_CLIENT_PREFACE] {
        let peer = TcpStream::connect(server.local_addr()).await.unwrap();
        assert_http2_rejected(peer, sent).await;
    }
    server.drain(Duration::from_secs(1)).await.unwrap();
}

const HTTP2_CLIENT_PREFACE: &[u8] =
    b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\x00\x00\x00\x04\x00\x00\x00\x00\x00";

async fn assert_http2_rejected(
    mut peer: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    sent: &[u8],
) {
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
    let options = ServerOptions {
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
async fn an_undecided_protocol_cannot_be_read_after_the_deadline() {
    let timeout = Duration::from_secs(5);
    let (mut client, server) = tokio::io::duplex(64);
    let mut io = SniffDeadline::new(server, timeout);
    tokio::time::advance(timeout).await;
    client.write_all(HTTP2_PREFACE).await.unwrap();

    let error = io.read(&mut [0; 64]).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

#[tokio::test(start_paused = true)]
async fn a_decided_protocol_is_never_timed_out() {
    let cases: [&[&[u8]]; 4] = [
        &[b"GET"],
        &[b"PR", b"OPFIND"],
        &[b"PRI * HTTP/2.0\r\n", b"\r\nSM\r\n\r\n"],
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
    server.drain(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn connection_cap_refuses_the_excess() {
    let mut opts = options();
    opts.max_connections = NonZeroU32::new(1);
    let server = Server::bind(loopback(), app(), opts).unwrap();
    let addr = server.local_addr();
    let first = TcpStream::connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut second = TcpStream::connect(addr).await.unwrap();
    second
        .write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    let outcome = tokio::time::timeout(Duration::from_secs(2), second.read_to_end(&mut buf)).await;
    std::assert_matches!(
        outcome,
        Ok(Ok(0) | Err(_)),
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
    let spreads = Vec::from_fn(64, |_| spread(age));
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
    let occupied = Server::bind(loopback(), app(), options()).unwrap();
    let err =
        Server::bind(occupied.local_addr(), app(), options()).expect_err("second bind must fail");
    std::assert_matches!(err, ServerError::Bind { .. }, "{err}");
    occupied.drain(Duration::from_secs(1)).await.unwrap();
}

// template:begin grpc:http-server-tls-test
use rustls::pki_types::ServerName;
use tls_test_support::localhost_tls;
use tokio_rustls::TlsConnector;

#[tokio::test]
async fn drain_does_not_wait_for_an_unfinished_tls_handshake() {
    let (server_config, client_config) = localhost_tls(b"h2");
    let mut opts = options();
    opts.header_read_timeout = Duration::from_secs(30);
    let server = BoundListener::bind(loopback()).unwrap().serve_h2(
        app(),
        opts,
        Some(Arc::new(server_config)),
    );
    let addr = server.local_addr();
    let mut stalled = TcpStream::connect(addr).await.unwrap();
    stalled.write_all(&[0x16]).await.unwrap();
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
    std::assert_matches!(
        closed,
        Ok(Ok(0)),
        "expected EOF from the server, got {closed:?}"
    );
}

#[tokio::test]
async fn connection_age_closes_a_socket_still_waiting_for_tls() {
    let (server_config, _) = localhost_tls(b"h2");
    let options = ServerOptions {
        header_read_timeout: Duration::from_secs(10),
        max_connection_age: Some(Duration::from_millis(200)),
        ..options()
    };
    let server = BoundListener::bind(loopback()).unwrap().serve_h2(
        app(),
        options,
        Some(Arc::new(server_config)),
    );
    let mut client = TcpStream::connect(server.local_addr()).await.unwrap();
    let mut byte = [0];
    let closed = tokio::time::timeout(Duration::from_secs(2), client.read(&mut byte)).await;
    std::assert_matches!(closed, Ok(Ok(0)), "expected EOF before the TLS timeout");
    assert_eq!(
        server.drain(Duration::from_secs(1)).await.unwrap(),
        Drained::Complete
    );
}

#[tokio::test]
async fn http2_listener_serves_http2_over_tls() {
    use http_body_util::BodyExt as _;

    let (server_config, client_config) = localhost_tls(b"h2");
    let mut opts = options();
    opts.header_read_timeout = Duration::from_secs(2);
    let server = BoundListener::bind(loopback()).unwrap().serve_h2(
        app(),
        opts,
        Some(Arc::new(server_config)),
    );
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
    let request = axum::http::Request::builder()
        .uri(format!("https://localhost:{}/ok", addr.port()))
        .body(axum::body::Body::empty())
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), sender.send_request(request))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ok");
    server.drain(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn http1_tls_listener_rejects_http2_in_alpn_and_on_the_connection() {
    let (server_config, mut client_config) = localhost_tls(b"h2");
    let server = BoundListener::bind(loopback()).unwrap().serve_tls(
        app(),
        ServerOptions {
            header_read_timeout: Duration::from_secs(2),
            ..options()
        },
        Arc::new(server_config),
    );
    let addr = server.local_addr();
    let name = ServerName::try_from("localhost").unwrap();
    let peer = TcpStream::connect(addr).await.unwrap();
    let error = TlsConnector::from(Arc::new(client_config.clone()))
        .connect(name.clone(), peer)
        .await
        .expect_err("the HTTP/1 listener negotiated TLS with an h2-only client");
    std::assert_matches!(
        error
            .get_ref()
            .and_then(|err| err.downcast_ref::<rustls::Error>()),
        Some(rustls::Error::AlertReceived(
            rustls::AlertDescription::NoApplicationProtocol
        )),
        "{error:?}"
    );

    client_config.alpn_protocols.push(b"http/1.1".to_vec());
    let peer = TcpStream::connect(addr).await.unwrap();
    let tls = TlsConnector::from(Arc::new(client_config))
        .connect(name, peer)
        .await
        .unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
    assert_http2_rejected(tls, HTTP2_CLIENT_PREFACE).await;
    server.drain(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn a_stalled_http2_preface_is_closed_after_the_header_timeout() {
    let server = BoundListener::bind(loopback())
        .unwrap()
        .serve_h2(app(), options(), None);
    let mut stalled = TcpStream::connect(server.local_addr()).await.unwrap();
    stalled.write_all(b"PRI * HTTP/2.0\r\n").await.unwrap();
    let mut sent_by_server = Vec::new();
    let closed = tokio::time::timeout(
        Duration::from_secs(2),
        stalled.read_to_end(&mut sent_by_server),
    )
    .await;
    assert!(closed.is_ok(), "the stalled connection stayed open");
    server.drain(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn an_http2_only_listener_answers_no_http1_request() {
    let server = BoundListener::bind(loopback())
        .unwrap()
        .serve_h2(app(), options(), None);
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
    server.drain(Duration::from_secs(1)).await.unwrap();
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
