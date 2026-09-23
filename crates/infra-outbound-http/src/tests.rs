#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "bounded local TLS fixtures make setup failures test failures"
)]

use std::{io, net::SocketAddr, process::Command, sync::Arc, time::Duration};

use infra_egress_dns::ResolveError;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
    time::Instant,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{Client, Error, Limits, Operation, Request, build_fixture_client, policy};

const FIXTURE_HOST: &str = "authn.fixture.test";
const CERT_DER: &[u8] = include_bytes!("../tests/fixtures/outbound-fixture-cert.der");
const KEY_DER: &[u8] = include_bytes!("../tests/fixtures/outbound-fixture-key.der");
const ROOT_DER: &[u8] = include_bytes!("../tests/fixtures/outbound-fixture-root.der");
const UNTRUSTED_ROOT_DER: &[u8] =
    include_bytes!("../tests/fixtures/outbound-fixture-untrusted-root.der");

#[derive(Clone)]
struct FixtureResolver {
    host: String,
    address: SocketAddr,
}

#[derive(Clone)]
struct RawAnswerResolver {
    answers: Vec<std::net::IpAddr>,
    admitted_fixture: SocketAddr,
}

impl Resolve for RawAnswerResolver {
    fn resolve(&self, _name: Name) -> Resolving {
        let answers = self.answers.clone();
        let admitted_fixture = self.admitted_fixture;
        Box::pin(async move {
            if answers.is_empty() {
                return Err(ResolveError::Denied.into());
            }
            for address in answers {
                infra_egress_dns::admit_address(address)?;
            }
            // Only the test maps an admitted public set to the local TLS peer.
            Ok(Box::new(std::iter::once(admitted_fixture)) as Addrs)
        })
    }
}

impl Resolve for FixtureResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = self.host.clone();
        let address = self.address;
        Box::pin(async move {
            if !name.as_str().eq_ignore_ascii_case(&host) {
                return Err(io::Error::other("fixture DNS denied").into());
            }
            Ok(Box::new(std::iter::once(address)) as Addrs)
        })
    }
}

fn limits() -> Limits {
    Limits {
        max_active: 1,
        operation_timeout: Duration::from_secs(1),
        request_header_count: 8,
        request_header_bytes: 512,
        response_header_count: 8,
        response_header_bytes: 512,
        request_body_bytes: 128,
        response_body_bytes: 128,
    }
}

fn fixture_client(address: SocketAddr, root: &[u8]) -> Client {
    fixture_client_with_limits(address, root, limits())
}

fn fixture_client_with_limits(address: SocketAddr, root: &[u8], limits: Limits) -> Client {
    let (base, authority) =
        policy::admit_base(&format!("https://{FIXTURE_HOST}/")).expect("fixture URL");
    let certificate = reqwest::Certificate::from_der(root).expect("fixture root certificate");
    let transport = build_fixture_client(
        FixtureResolver {
            host: FIXTURE_HOST.to_owned(),
            address,
        },
        &limits,
        certificate,
    )
    .expect("fixture client");
    Client {
        base,
        authority,
        limits,
        transport,
        admission: Arc::new(tokio::sync::Semaphore::new(limits.max_active)),
        shutdown: CancellationToken::new(),
        response_head_observed: None,
    }
}

fn denied_client(root: &[u8], address: SocketAddr) -> Client {
    let limits = limits();
    let (base, authority) =
        policy::admit_base(&format!("https://{FIXTURE_HOST}/")).expect("denied fixture URL");
    let certificate = reqwest::Certificate::from_der(root).expect("denied fixture root");
    let transport = build_fixture_client(
        RawAnswerResolver {
            answers: vec!["8.8.8.8".parse().expect("public answer"), address.ip()],
            admitted_fixture: address,
        },
        &limits,
        certificate,
    )
    .expect("denied fixture client");
    Client {
        base,
        authority,
        limits,
        transport,
        admission: Arc::new(tokio::sync::Semaphore::new(limits.max_active)),
        shutdown: CancellationToken::new(),
        response_head_observed: None,
    }
}

fn operation() -> Operation {
    Operation {
        deadline: Instant::now() + Duration::from_secs(1),
        cancel: CancellationToken::new(),
        timeout: None,
        response_body_bytes: None,
    }
}

fn request() -> Request {
    Request {
        method: reqwest::Method::GET,
        target: "/fixture".to_owned(),
        headers: reqwest::header::HeaderMap::new(),
        body: Vec::new(),
    }
}

fn fixture_acceptor() -> TlsAcceptor {
    let config = ServerConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("fixture TLS protocol versions")
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from(CERT_DER.to_vec())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY_DER.to_vec())),
    )
    .expect("fixture certificate and key");
    TlsAcceptor::from(Arc::new(config))
}

async fn read_request_headers<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> Vec<u8> {
    let mut headers = Vec::new();
    let mut chunk = [0_u8; 256];
    while !headers.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        let read = stream
            .read(&mut chunk)
            .await
            .expect("fixture reads request");
        assert!(read > 0, "request headers must have a complete frame");
        headers.extend_from_slice(&chunk[..read]);
        assert!(headers.len() <= 4096, "fixture request remains bounded");
    }
    headers
}

async fn tls_server(response: &'static [u8]) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let acceptor = fixture_acceptor();
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (socket, _) = listener.accept().await.expect("fixture accepts connection");
            let Ok(mut stream) = acceptor.accept(socket).await else {
                return;
            };
            read_request_headers(&mut stream).await;
            stream
                .write_all(response)
                .await
                .expect("fixture writes response");
            stream.shutdown().await.expect("fixture closes response");
        })
        .await
        .expect("TLS fixture completes within its budget");
    });
    (address, server)
}

async fn tls_server_capture(
    response: &'static [u8],
) -> (SocketAddr, oneshot::Receiver<Vec<u8>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("capture fixture listener");
    let address = listener.local_addr().expect("capture fixture address");
    let acceptor = fixture_acceptor();
    let (captured_send, captured) = oneshot::channel();
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (socket, _) = listener
                .accept()
                .await
                .expect("capture fixture accepts connection");
            let Ok(mut stream) = acceptor.accept(socket).await else {
                return;
            };
            let request = read_request_headers(&mut stream).await;
            captured_send
                .send(request)
                .expect("capture receiver stays available");
            stream
                .write_all(response)
                .await
                .expect("capture fixture writes response");
            stream
                .shutdown()
                .await
                .expect("capture fixture closes response");
        })
        .await
        .expect("TLS fixture completes within its budget");
    });
    (address, captured, server)
}

async fn tls_server_stall_after_headers() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stall fixture listener");
    let address = listener.local_addr().expect("stall fixture address");
    let acceptor = fixture_acceptor();
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (socket, _) = listener
                .accept()
                .await
                .expect("stall fixture accepts connection");
            let Ok(mut stream) = acceptor.accept(socket).await else {
                return;
            };
            read_request_headers(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n")
                .await
                .expect("stall fixture writes headers");
            stream.flush().await.expect("stall fixture flushes headers");
            std::future::pending::<()>().await;
        })
        .await
        .expect("TLS fixture completes within its budget");
    });
    (address, server)
}

#[tokio::test]
async fn trusted_named_tls_fixture_returns_a_complete_response() {
    let (address, server) =
        tls_server(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\nok").await;
    let result = fixture_client(address, ROOT_DER)
        .execute(request(), operation())
        .await;
    let response = result.expect("trusted TLS response");
    assert_eq!(response.status, reqwest::StatusCode::CREATED);
    assert_eq!(response.body, b"ok");
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .expect("fixture server completes")
        .expect("fixture server succeeds");
}

#[tokio::test]
async fn untrusted_tls_root_is_a_static_transport_failure() {
    let (address, server) = tls_server(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let result = fixture_client(address, UNTRUSTED_ROOT_DER)
        .execute(request(), operation())
        .await;
    assert!(matches!(result, Err(Error::Transport)));
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .expect("fixture server completes")
        .expect("fixture server succeeds");
}

#[tokio::test]
async fn a_trusted_root_does_not_disable_tls_hostname_verification() {
    let (address, server) = tls_server(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let mut client = fixture_client(address, ROOT_DER);
    client.base = Url::parse("https://different.fixture.test/").expect("different hostname");
    client.authority = policy::admit_base(client.base.as_str())
        .expect("different authority")
        .1;
    client.transport = build_fixture_client(
        FixtureResolver {
            host: "different.fixture.test".to_owned(),
            address,
        },
        &client.limits,
        reqwest::Certificate::from_der(ROOT_DER).expect("trusted fixture root"),
    )
    .expect("hostname fixture client");
    assert!(matches!(
        client.execute(request(), operation()).await,
        Err(Error::Transport)
    ));
    server.await.expect("hostname fixture joins");
}

#[tokio::test]
async fn framed_body_obeys_the_exact_operation_limit() {
    let mut limits = limits();
    limits.response_body_bytes = 2;
    let (exact_address, exact_server) =
        tls_server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
    let exact = fixture_client_with_limits(exact_address, ROOT_DER, limits)
        .execute(request(), operation())
        .await
        .expect("exact framed body limit succeeds");
    assert_eq!(exact.body, b"ok");
    tokio::time::timeout(Duration::from_secs(1), exact_server)
        .await
        .expect("exact fixture server completes")
        .expect("exact fixture server succeeds");

    let (over_address, over_server) =
        tls_server(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nno!").await;
    let over = fixture_client_with_limits(over_address, ROOT_DER, limits)
        .execute(request(), operation())
        .await;
    assert!(matches!(over, Err(Error::ResponseBodyTooLarge)));
    tokio::time::timeout(Duration::from_secs(1), over_server)
        .await
        .expect("over-limit fixture server completes")
        .expect("over-limit fixture server succeeds");
}

#[tokio::test]
async fn framed_chunked_and_unknown_length_bodies_enforce_the_streaming_cap() {
    let cases = [
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n".as_slice(),
            Ok(b"ok".as_slice()),
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nno!\r\n0\r\n\r\n"
                .as_slice(),
            Err(Error::ResponseBodyTooLarge),
        ),
        (
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nno!".as_slice(),
            Err(Error::ResponseBodyTooLarge),
        ),
    ];
    for (response, expected) in cases {
        let (address, server) = tls_server(response).await;
        let mut ceiling = limits();
        ceiling.response_body_bytes = 2;
        let result = fixture_client_with_limits(address, ROOT_DER, ceiling)
            .execute(request(), operation())
            .await;
        match (result, expected) {
            (Ok(actual), Ok(body)) => assert_eq!(actual.body, body),
            (Err(actual), Err(error)) => assert_eq!(actual, error),
            (actual, expected) => {
                panic!("unexpected framed body result: {actual:?}, expected {expected:?}")
            }
        }
        server.await.expect("framed body fixture joins");
    }
}

#[tokio::test]
async fn cancelled_or_expired_operation_never_starts_a_connection() {
    let client = fixture_client("127.0.0.1:9".parse().expect("discard address"), ROOT_DER);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancelled = client
        .execute(
            request(),
            Operation {
                deadline: Instant::now() + Duration::from_secs(1),
                cancel,
                timeout: None,
                response_body_bytes: None,
            },
        )
        .await;
    assert!(matches!(cancelled, Err(Error::Cancelled)));

    let expired = client
        .execute(
            request(),
            Operation {
                deadline: Instant::now() - Duration::from_millis(1),
                cancel: CancellationToken::new(),
                timeout: None,
                response_body_bytes: None,
            },
        )
        .await;
    assert!(matches!(expired, Err(Error::Timeout)));
}

#[tokio::test]
async fn typed_denied_dns_answer_refuses_before_any_connection() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("no-connect listener");
    let result = denied_client(ROOT_DER, listener.local_addr().expect("listener address"))
        .execute(request(), operation())
        .await;
    assert!(matches!(result, Err(Error::Denied)));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn truncated_body_is_transport_failure_and_statuses_are_not_replayed_or_redirected() {
    let (truncated_address, truncated_server) =
        tls_server(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nok").await;
    let truncated = fixture_client(truncated_address, ROOT_DER)
        .execute(request(), operation())
        .await;
    assert!(matches!(truncated, Err(Error::Transport)));
    truncated_server
        .await
        .expect("truncated fixture server succeeds");

    let (redirect_address, redirect_server) = tls_server(
        b"HTTP/1.1 302 Found\r\nLocation: https://other.fixture.test/\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    let redirect = fixture_client(redirect_address, ROOT_DER)
        .execute(request(), operation())
        .await
        .expect("redirect response remains local");
    assert_eq!(redirect.status, reqwest::StatusCode::FOUND);
    redirect_server
        .await
        .expect("redirect fixture server succeeds");

    let (failure_address, failure_server) =
        tls_server(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n").await;
    let failure = fixture_client(failure_address, ROOT_DER)
        .execute(request(), operation())
        .await
        .expect("non-success response is returned once");
    assert_eq!(failure.status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    failure_server
        .await
        .expect("failure fixture server succeeds");
}

#[tokio::test]
async fn a_request_is_not_replayed_after_the_peer_closes_without_a_response() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("replay listener");
    let address = listener.local_addr().expect("replay address");
    let acceptor = fixture_acceptor();
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (socket, _) = listener.accept().await.expect("first request connects");
            let mut stream = acceptor.accept(socket).await.expect("first TLS handshake");
            read_request_headers(&mut stream).await;
            drop(stream);
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_ok()
        })
        .await
        .expect("replay fixture is bounded")
    });
    assert!(matches!(
        fixture_client(address, ROOT_DER)
            .execute(request(), operation())
            .await,
        Err(Error::Transport)
    ));
    assert!(
        !server.await.expect("replay fixture joins"),
        "request was replayed"
    );
}

#[tokio::test]
async fn captured_wire_request_omits_propagation_and_encoding_headers() {
    let (address, captured, server) =
        tls_server_capture(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let mut request = request();
    request.headers.insert(
        "traceparent",
        "00-abc-def-01".parse().expect("trace header"),
    );
    request
        .headers
        .insert("tracestate", "vendor=value".parse().expect("state header"));
    request
        .headers
        .insert("baggage", "key=value".parse().expect("baggage header"));
    request
        .headers
        .insert("x-request-id", "correlation".parse().expect("request id"));
    request
        .headers
        .insert("accept-encoding", "gzip".parse().expect("encoding header"));
    fixture_client(address, ROOT_DER)
        .execute(request, operation())
        .await
        .expect("captured request response");
    let wire = String::from_utf8(captured.await.expect("captured request")).expect("ASCII request");
    for header in [
        "traceparent:",
        "tracestate:",
        "baggage:",
        "x-request-id:",
        "accept-encoding:",
    ] {
        assert!(
            !wire.to_ascii_lowercase().contains(header),
            "{header} leaked to the wire"
        );
    }
    server.await.expect("capture fixture server succeeds");
}

#[tokio::test]
async fn one_absolute_timeout_covers_headers_and_framed_body() {
    let (address, server) = tls_server_stall_after_headers().await;
    let head_observed = Arc::new(tokio::sync::Notify::new());
    let mut limits = limits();
    limits.operation_timeout = Duration::from_millis(500);
    let mut client = fixture_client_with_limits(address, ROOT_DER, limits);
    client.response_head_observed = Some(head_observed.clone());
    let exchange = tokio::spawn(async move { client.execute(request(), operation()).await });
    tokio::time::timeout(Duration::from_secs(2), head_observed.notified())
        .await
        .expect("client admits response headers before pending body read");
    assert!(matches!(
        exchange.await.expect("timed exchange joins"),
        Err(Error::Timeout)
    ));
    server.abort();
    assert!(
        server
            .await
            .expect_err("aborted fixture joins")
            .is_cancelled()
    );
}

#[tokio::test]
async fn admission_stays_held_through_body_and_releases_on_cancellation() {
    let (address, server) = tls_server_stall_after_headers().await;
    let head_observed = Arc::new(tokio::sync::Notify::new());
    let mut client = fixture_client(address, ROOT_DER);
    client.response_head_observed = Some(head_observed.clone());
    let cancel = CancellationToken::new();
    let first_client = client.clone();
    let first_cancel = cancel.clone();
    let first = tokio::spawn(async move {
        first_client
            .execute(
                request(),
                Operation {
                    deadline: Instant::now() + Duration::from_secs(1),
                    cancel: first_cancel,
                    timeout: None,
                    response_body_bytes: None,
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), head_observed.notified())
        .await
        .expect("client admits response headers before pending body read");
    assert!(matches!(
        client.execute(request(), operation()).await,
        Err(Error::AtCapacity)
    ));
    cancel.cancel();
    assert!(matches!(
        first.await.expect("first operation joins"),
        Err(Error::Cancelled)
    ));
    assert_eq!(client.admission.available_permits(), 1);
    server.abort();
    assert!(
        server
            .await
            .expect_err("aborted fixture joins")
            .is_cancelled()
    );
}

#[tokio::test]
async fn dropping_the_calling_task_releases_operation_admission() {
    let (address, server) = tls_server_stall_after_headers().await;
    let head_observed = Arc::new(tokio::sync::Notify::new());
    let mut client = fixture_client(address, ROOT_DER);
    client.response_head_observed = Some(head_observed.clone());
    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.execute(request(), operation()).await });
    tokio::time::timeout(Duration::from_secs(2), head_observed.notified())
        .await
        .expect("client admits response headers before pending body read");
    assert_eq!(client.admission.available_permits(), 0);
    first.abort();
    assert!(
        first
            .await
            .expect_err("dropped exchange joins")
            .is_cancelled()
    );
    assert_eq!(client.admission.available_permits(), 1);
    server.abort();
    assert!(
        server
            .await
            .expect_err("aborted fixture joins")
            .is_cancelled()
    );
}

#[tokio::test]
async fn parsed_response_header_bytes_and_parser_count_are_bounded() {
    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-Large: abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz\r\n\r\n";
    let (address, server) = tls_server(response).await;
    let mut ceiling = limits();
    ceiling.response_header_bytes = 32;
    assert!(matches!(
        fixture_client_with_limits(address, ROOT_DER, ceiling)
            .execute(request(), operation())
            .await,
        Err(Error::ResponseHeadersTooLarge)
    ));
    server.await.expect("header fixture joins");

    let (address, server) = tls_server(response).await;
    let mut ceiling = limits();
    ceiling.response_header_count = 1;
    assert!(matches!(
        fixture_client_with_limits(address, ROOT_DER, ceiling)
            .execute(request(), operation())
            .await,
        Err(Error::Transport)
    ));
    server.await.expect("parser fixture joins");
}

#[test]
fn proxy_environment_isolated_in_a_subprocess_cannot_divert_fixture_client() {
    const CHILD: &str = "INFRA_OUTBOUND_HTTP_PROXY_CHILD";
    if std::env::var_os(CHILD).is_some() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("child runtime")
            .block_on(async {
                let (address, server) =
                    tls_server(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
                fixture_client(address, ROOT_DER)
                    .execute(request(), operation())
                    .await
                    .expect("no_proxy client reaches fixture under proxy environment");
                server.await.expect("proxy fixture server succeeds");
            });
        return;
    }
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg("tests::proxy_environment_isolated_in_a_subprocess_cannot_divert_fixture_client")
        .arg("--nocapture")
        .env(CHILD, "1")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("https_proxy", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .spawn()
        .expect("proxy child starts");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().expect("observe proxy child") {
            assert!(status.success(), "proxy child must prove no_proxy behavior");
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().expect("terminate overdue proxy child");
            child.wait().expect("join terminated proxy child");
            panic!("proxy child exceeded its budget");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn reserved_propagation_headers_are_removed_before_accounting() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "traceparent",
        "oversized value".parse().expect("header value"),
    );
    headers.insert(
        "x-request-id",
        "oversized value".parse().expect("header value"),
    );
    headers.insert(
        "authorization",
        "Bearer fixture".parse().expect("header value"),
    );
    let admitted = policy::admit_request_headers(headers, 1, 64).expect("remaining header fits");
    assert!(!admitted.contains_key("traceparent"));
    assert!(!admitted.contains_key("x-request-id"));
    assert!(admitted.contains_key("authorization"));
}

#[test]
fn duplicate_field_values_count_individually() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.append("x-one", "a".parse().expect("first duplicate"));
    headers.append("x-one", "b".parse().expect("second duplicate"));
    assert_eq!(
        policy::admit_response_headers(&headers, 1, 128),
        Err(Error::ResponseHeadersTooLarge)
    );
}

#[test]
fn host_header_is_denied_before_transport() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("host", "authn.fixture.test".parse().expect("host header"));
    assert_eq!(
        policy::admit_request_headers(headers, 8, 512),
        Err(Error::Denied)
    );
}

#[test]
fn limits_reject_zero_overflow_and_unrepresentable_values() {
    let mut invalid = limits();
    invalid.max_active = 0;
    assert_eq!(
        policy::validate_limits(&invalid),
        Err(Error::InvalidConfiguration)
    );

    let mut invalid = limits();
    invalid.operation_timeout = Duration::MAX;
    assert_eq!(
        policy::validate_limits(&invalid),
        Err(Error::InvalidConfiguration)
    );

    let setters: [fn(&mut Limits); 8] = [
        |value: &mut Limits| value.request_header_count = usize::MAX,
        |value: &mut Limits| value.request_header_count = 32_769,
        |value: &mut Limits| value.response_header_count = 32_769,
        |value: &mut Limits| value.request_header_bytes = usize::MAX,
        |value: &mut Limits| value.response_header_count = usize::MAX,
        |value: &mut Limits| value.response_header_bytes = usize::MAX,
        |value: &mut Limits| value.request_body_bytes = usize::MAX,
        |value: &mut Limits| value.response_body_bytes = usize::MAX,
    ];
    for set_limit in setters {
        let mut invalid = limits();
        set_limit(&mut invalid);
        assert_eq!(
            policy::validate_limits(&invalid),
            Err(Error::InvalidConfiguration)
        );
    }
}

#[test]
fn target_admission_preserves_relative_and_canonical_authority_rules() {
    let (base, configured) =
        policy::admit_base("https://authn.fixture.test/base/").expect("base URL");
    assert_eq!(
        policy::admit_target(&base, &configured, "child?query=yes")
            .expect("relative target")
            .as_str(),
        "https://authn.fixture.test/base/child?query=yes"
    );
    assert!(
        policy::admit_target(&base, &configured, "https://authn.fixture.test:443/child").is_ok()
    );
    assert_eq!(
        policy::admit_target(&base, &configured, "https://other.fixture.test/").unwrap_err(),
        Error::Denied
    );
    for target in [
        "https://@authn.fixture.test/",
        "//@authn.fixture.test/",
        "///@authn.fixture.test/",
        "////@authn.fixture.test/",
        "https:\\@authn.fixture.test/",
        "\\\\@authn.fixture.test/",
    ] {
        assert_eq!(
            policy::admit_target(&base, &configured, target).unwrap_err(),
            Error::InvalidTarget,
            "{target} must retain raw userinfo syntax"
        );
    }
    assert!(policy::admit_target(&base, &configured, "/people/@self").is_ok());
    for target in ["child\u{0085}", "child\n", " child", "child "] {
        assert!(matches!(
            policy::admit_target(&base, &configured, target),
            Err(Error::InvalidTarget)
        ));
    }
    assert!(policy::admit_base("https://authn.fixture.test/\u{0085}").is_err());
}

#[test]
fn literal_and_mapped_private_bases_are_denied_before_resolver_setup() {
    for base in ["https://127.0.0.1/", "https://[::ffff:127.0.0.1]/"] {
        assert!(
            matches!(
                Client::new(
                    base,
                    limits(),
                    tokio_util::task::TaskTracker::new(),
                    CancellationToken::new(),
                ),
                Err(Error::Denied)
            ),
            "{base} must be denied"
        );
    }
}

#[test]
fn large_single_header_fields_are_rejected_for_each_direction() {
    let value = "x".repeat(32);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-field", value.parse().expect("request field"));
    assert_eq!(
        policy::admit_request_headers(headers, 1, 16),
        Err(Error::RequestHeadersTooLarge)
    );

    let value = "x".repeat(32);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-field", value.parse().expect("response field"));
    assert_eq!(
        policy::admit_response_headers(&headers, 1, 16),
        Err(Error::ResponseHeadersTooLarge)
    );
}
