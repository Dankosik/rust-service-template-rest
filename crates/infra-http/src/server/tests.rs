use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroU32;

use axum::routing::get;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing_subscriber::layer::SubscriberExt as _;

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

fn harden(app: Router) -> Router {
    crate::harden(
        app,
        &crate::HardenOptions {
            max_body_bytes: 64,
            request_timeout: Duration::from_secs(30),
            max_in_flight: None,
            log_health_probes: false,
        },
    )
}

fn capture_request_spans() -> (
    tracing::subscriber::DefaultGuard,
    SdkTracerProvider,
    InMemorySpanExporter,
) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    (
        tracing::subscriber::set_default(subscriber),
        provider,
        exporter,
    )
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
    let server = Server::bind(loopback(), app(), options()).await.unwrap();
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
    let server = Server::bind(loopback(), app(), options()).await.unwrap();
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
    let (_subscriber, _provider, exporter) = capture_request_spans();
    let (server, peer) = pending_request().await;
    let drained = server.drain(Duration::from_millis(50)).await.unwrap();
    assert_eq!(
        drained,
        Drained::TimedOut {
            remaining_connections: 1
        }
    );
    assert_peer_closed(peer).await;
    let spans = exporter.get_finished_spans().unwrap();
    let access = spans
        .iter()
        .flat_map(|span| span.events.iter())
        .find(|event| event.name == "http_request")
        .expect("the interrupted request has an access event");
    assert!(
        access
            .attributes
            .contains(&opentelemetry::KeyValue::new("status", "503"))
    );
    assert!(access.attributes.contains(&opentelemetry::KeyValue::new(
        "termination",
        "server_shutdown"
    )));
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
    let server = Server::bind(loopback(), harden(app), options())
        .await
        .unwrap();
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
    // Control the native acceptance owner independently of the real
    // open request: the deadline must include its unconfirmed join.
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
    // Fault observation leaves the native join for drain, while the
    // real open request still needs forced cleanup.
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
    let server = bind_listener(loopback(), app(), options(), move |stream| {
        let _ = &marker;
        async move { Some((stream, "http")) }
    })
    .await
    .unwrap();
    stop.set(server.connection_shutdown.finish.clone()).unwrap();
    let failure = server.accept_failure();
    let peer = TcpStream::connect(server.local_addr()).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), failure)
            .await
            .unwrap(),
        AcceptFailure::Panicked
    );
    // Observing the fault does not consume acceptance's join outcome.
    std::assert_matches!(server.drain(Duration::from_secs(1)).await, Err(ServerError::AcceptTask(error)) if error.is_panic()
    );
    drop(peer);
}

#[tokio::test]
async fn silent_client_is_closed_after_the_header_timeout() {
    let server = Server::bind(loopback(), app(), options()).await.unwrap();
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
async fn a_stalled_http2_preface_is_closed_after_the_header_timeout() {
    let server = Server::bind(loopback(), app(), options()).await.unwrap();
    let mut stalled = TcpStream::connect(server.local_addr()).await.unwrap();
    // A strict prefix of the HTTP/2 preface leaves the protocol undecided,
    // which is before hyper's own header timer starts.
    stalled.write_all(b"PRI * HTTP/2.0\r\n").await.unwrap();
    let mut buf = [0u8; 1];
    let closed = tokio::time::timeout(Duration::from_secs(2), stalled.read(&mut buf)).await;
    std::assert_matches!(
        closed,
        Ok(Ok(0)),
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
    std::assert_matches!(
        closed,
        Ok(Ok(0)),
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
    std::assert_matches!(outcome, Ok(Ok(_) | Err(_)), "{outcome:?}");
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
    let outcome = tokio::time::timeout(Duration::from_secs(2), second.read_to_end(&mut buf)).await;
    // Closed with unread bytes pending: EOF or a reset, never a response.
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
    std::assert_matches!(err, ServerError::Bind { .. }, "{err}");
    occupied.drain(Duration::from_secs(1)).await.unwrap();
}

// template:begin grpc:http-server-tls-test
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use tokio_rustls::TlsConnector;

/// A server and a client configuration for `localhost` under one fresh CA,
/// both TLS 1.3 with the requested ALPN protocol.
fn localhost_tls(alpn: &[u8]) -> (ServerConfig, ClientConfig) {
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
    server.alpn_protocols = vec![alpn.to_vec()];
    let mut roots = RootCertStore::empty();
    roots.add(issuer.der().clone()).unwrap();
    let mut client = ClientConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client.alpn_protocols = vec![alpn.to_vec()];
    (server, client)
}

#[tokio::test]
async fn drain_does_not_wait_for_an_unfinished_tls_handshake() {
    let (server_config, client_config) = localhost_tls(b"h2");
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
    std::assert_matches!(
        closed,
        Ok(Ok(0)),
        "expected EOF from the server, got {closed:?}"
    );
}

#[tokio::test]
async fn tls_listener_serves_http2() {
    use http_body_util::BodyExt as _;

    let (server_config, client_config) = localhost_tls(b"h2");
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

#[tokio::test]
async fn tls_http1_origin_form_reports_https_in_the_server_span() {
    let (_subscriber, _provider, exporter) = capture_request_spans();
    let (server_config, client_config) = localhost_tls(b"http/1.1");
    let opts = ServerOptions {
        header_read_timeout: Duration::from_secs(2),
        ..options()
    };
    let server = Server::bind_tls(loopback(), harden(app()), opts, Arc::new(server_config))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        let stream = TcpStream::connect(server.local_addr()).await.unwrap();
        let mut tls = TlsConnector::from(Arc::new(client_config))
            .connect(ServerName::try_from("localhost").unwrap(), stream)
            .await
            .unwrap();
        tls.write_all(b"GET /ok HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut status = [0; 12];
        tls.read_exact(&mut status).await.unwrap();
        assert_eq!(&status, b"HTTP/1.1 200");
    })
    .await
    .unwrap();
    assert_eq!(
        server.drain(Duration::from_secs(1)).await.unwrap(),
        Drained::Complete
    );
    let spans = exporter.get_finished_spans().unwrap();
    let span = spans.iter().find(|span| span.name == "GET /ok").unwrap();
    assert!(
        span.attributes
            .contains(&opentelemetry::KeyValue::new("url.scheme", "https"))
    );
}
// template:end grpc:http-server-tls-test
