#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "bounded local TLS fixtures make setup failures test failures"
)]

#[path = "../../infra-egress-dns/tests/fixtures/tls.rs"]
mod tls;

use std::{
    error::Error as _,
    fmt::Write as _,
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use http::{Request, Version, header};
use infra_egress_dns::admit_answers;
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

use crate::{Client, Error, Limits, Operation, build_fixture_client, policy};
use tls::TlsMaterial;

const FIXTURE_HOST: &str = "authn.fixture.test";

#[derive(Clone)]
struct FixtureResolver {
    host: String,
    address: SocketAddr,
}

#[derive(Clone)]
struct RawAnswerResolver {
    answers: Vec<IpAddr>,
    admitted_fixture: SocketAddr,
}

impl Resolve for RawAnswerResolver {
    fn resolve(&self, _name: Name) -> Resolving {
        let answers = self.answers.clone();
        let admitted_fixture = self.admitted_fixture;
        Box::pin(async move {
            let admitted = admit_answers(answers)?;
            // The raw-answer facade exercises shared admission before mapping
            // a safe synthetic answer to the in-process TLS peer.
            Ok(Box::new(admitted.into_iter().map(move |_| admitted_fixture)) as Addrs)
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
        response_header_count: 8,
        response_header_bytes: 512,
        response_body_bytes: 128,
    }
}

fn fixture_client(address: SocketAddr, material: &TlsMaterial) -> Client {
    fixture_client_with_limits(address, material, limits())
}

fn fixture_client_with_limits(
    address: SocketAddr,
    material: &TlsMaterial,
    limits: Limits,
) -> Client {
    let base = policy::admit_base(&format!("https://{FIXTURE_HOST}/")).expect("fixture URL");
    let certificate =
        reqwest::Certificate::from_der(&material.root).expect("fixture root certificate");
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
        limits,
        transport,
        admission: Arc::new(tokio::sync::Semaphore::new(limits.max_active)),
        response_head_observed: None,
    }
}

fn denied_client(material: &TlsMaterial, address: SocketAddr) -> Client {
    let limits = limits();
    let base = policy::admit_base(&format!("https://{FIXTURE_HOST}/")).expect("fixture URL");
    let certificate = reqwest::Certificate::from_der(&material.root).expect("fixture root");
    let transport = build_fixture_client(
        RawAnswerResolver {
            answers: vec!["8.8.8.8".parse().expect("public answer"), address.ip()],
            admitted_fixture: address,
        },
        &limits,
        certificate,
    )
    .expect("fixture client");
    Client {
        base,
        limits,
        transport,
        admission: Arc::new(tokio::sync::Semaphore::new(1)),
        response_head_observed: None,
    }
}

fn operation() -> Operation {
    Operation {
        deadline: Instant::now() + Duration::from_secs(1),
        timeout: None,
        response_body_bytes: None,
    }
}

fn request() -> Request<Bytes> {
    Request::get("/fixture")
        .body(Bytes::new())
        .expect("fixture request")
}

fn fixture_acceptor(material: &TlsMaterial) -> TlsAcceptor {
    let config = ServerConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("fixture TLS protocol versions")
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from(material.cert.clone())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(material.key.clone())),
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

async fn tls_server(
    material: &TlsMaterial,
    response: &'static [u8],
) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let acceptor = fixture_acceptor(material);
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
    material: &TlsMaterial,
    response: &'static [u8],
) -> (SocketAddr, oneshot::Receiver<Vec<u8>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("capture fixture listener");
    let address = listener.local_addr().expect("capture fixture address");
    let acceptor = fixture_acceptor(material);
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

async fn tls_server_stall_after_headers(material: &TlsMaterial) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stall fixture listener");
    let address = listener.local_addr().expect("stall fixture address");
    let acceptor = fixture_acceptor(material);
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

async fn tls_server_reuses_one_connection(
    material: &TlsMaterial,
) -> (SocketAddr, JoinHandle<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("pool fixture listener");
    let address = listener.local_addr().expect("pool fixture address");
    let acceptor = fixture_acceptor(material);
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (socket, _) = listener.accept().await.expect("pooled request connects");
            let mut stream = acceptor.accept(socket).await.expect("pooled TLS handshake");
            read_request_headers(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .expect("first pooled response");
            stream.flush().await.expect("first pooled response flushes");
            read_request_headers(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .expect("second pooled response");
            stream.shutdown().await.expect("pooled connection closes");
            1
        })
        .await
        .expect("pool fixture completes within its budget")
    });
    (address, server)
}

#[tokio::test]
async fn standard_request_returns_standard_response_with_full_body() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 201 Created\r\nX-Fixture: yes\r\nContent-Length: 2\r\n\r\nok",
    )
    .await;
    let response = fixture_client(address, &material)
        .execute(request(), operation())
        .await
        .expect("trusted TLS response");
    assert_eq!(response.status(), http::StatusCode::CREATED);
    assert_eq!(response.version(), Version::HTTP_11);
    assert_eq!(response.headers()["x-fixture"], "yes");
    assert_eq!(response.body().as_ref(), b"ok");
    server.await.expect("fixture server succeeds");
}

#[tokio::test]
async fn tls_trust_and_hostname_failures_remain_transport_errors() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let mut untrusted = TlsMaterial::new(FIXTURE_HOST);
    untrusted.root = material.untrusted_root.clone();
    assert!(matches!(
        fixture_client(address, &untrusted)
            .execute(request(), operation())
            .await,
        Err(Error::Transport { .. })
    ));
    server.await.expect("untrusted fixture server succeeds");

    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let mut client = fixture_client(address, &material);
    client.base = policy::admit_base("https://different.fixture.test/").expect("different base");
    client.transport = build_fixture_client(
        FixtureResolver {
            host: "different.fixture.test".to_owned(),
            address,
        },
        &client.limits,
        reqwest::Certificate::from_der(&material.root).expect("fixture root"),
    )
    .expect("hostname fixture client");
    assert!(matches!(
        client.execute(request(), operation()).await,
        Err(Error::Transport { .. })
    ));
    server.await.expect("hostname fixture server succeeds");
}

#[tokio::test]
async fn framed_and_streamed_bodies_obey_the_response_ceiling() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let mut ceiling = limits();
    ceiling.response_body_bytes = 2;
    let cases = [
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".as_slice(),
            true,
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nno!\r\n0\r\n\r\n"
                .as_slice(),
            false,
        ),
        (
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nno!".as_slice(),
            false,
        ),
    ];
    for (wire, succeeds) in cases {
        let (address, server) = tls_server(&material, wire).await;
        let result = fixture_client_with_limits(address, &material, ceiling)
            .execute(request(), operation())
            .await;
        if succeeds {
            assert_eq!(
                result.expect("exact ceiling response").body().as_ref(),
                b"ok"
            );
        } else {
            assert!(matches!(result, Err(Error::ResponseBodyTooLarge)));
        }
        server.await.expect("body fixture server succeeds");
    }
}

#[tokio::test]
async fn advertised_overflow_and_missing_exact_cap_eof_do_not_return_a_body() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let mut ceiling = limits();
    ceiling.response_body_bytes = 2;
    let cases = [
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nno!".as_slice(),
            "advertised overflow",
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\no".as_slice(),
            "missing exact-cap EOF",
        ),
    ];
    for (wire, name) in cases {
        let (address, server) = tls_server(&material, wire).await;
        let error = fixture_client_with_limits(address, &material, ceiling)
            .execute(request(), operation())
            .await
            .expect_err(name);
        match name {
            "advertised overflow" => assert!(matches!(error, Error::ResponseBodyTooLarge)),
            "missing exact-cap EOF" => assert!(matches!(error, Error::Transport { .. })),
            _ => unreachable!("fixed test case name"),
        }
        server.await.expect("framing fixture server succeeds");
    }

    let (address, server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
    assert!(matches!(
        fixture_client_with_limits(address, &material, ceiling)
            .execute(
                request(),
                Operation {
                    deadline: Instant::now() + Duration::from_secs(1),
                    timeout: None,
                    response_body_bytes: Some(1),
                },
            )
            .await,
        Err(Error::ResponseBodyTooLarge)
    ));
    server.await.expect("narrowed body fixture server succeeds");
}

#[tokio::test]
async fn response_header_count_and_aggregate_bytes_have_distinct_bounds() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-Large: abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz\r\n\r\n";
    let mut byte_ceiling = limits();
    byte_ceiling.response_header_bytes = 32;
    let (address, server) = tls_server(&material, response).await;
    assert!(matches!(
        fixture_client_with_limits(address, &material, byte_ceiling)
            .execute(request(), operation())
            .await,
        Err(Error::ResponseHeadersTooLarge)
    ));
    server.await.expect("aggregate header fixture joins");

    let mut count_ceiling = limits();
    count_ceiling.response_header_count = 1;
    let (address, server) = tls_server(&material, response).await;
    assert!(matches!(
        fixture_client_with_limits(address, &material, count_ceiling)
            .execute(request(), operation())
            .await,
        Err(Error::Transport { .. })
    ));
    server.await.expect("parser header fixture joins");
}

#[tokio::test]
async fn denied_answer_set_refuses_before_any_connection() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("no-connect listener");
    let result = denied_client(&material, listener.local_addr().expect("listener address"))
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
async fn expired_or_wider_operation_limits_refuse_before_network_work() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("no-connect listener");
    let client = fixture_client(listener.local_addr().expect("listener address"), &material);
    let expired = client
        .execute(
            request(),
            Operation {
                deadline: Instant::now() - Duration::from_millis(1),
                timeout: None,
                response_body_bytes: None,
            },
        )
        .await;
    assert!(matches!(expired, Err(Error::Timeout { source: None })));
    let wider = client
        .execute(
            request(),
            Operation {
                deadline: Instant::now() + Duration::from_secs(1),
                timeout: Some(Duration::from_secs(2)),
                response_body_bytes: None,
            },
        )
        .await;
    assert!(matches!(wider, Err(Error::InvalidConfiguration)));
    let zero_timeout = client
        .execute(
            request(),
            Operation {
                deadline: Instant::now() + Duration::from_secs(1),
                timeout: Some(Duration::ZERO),
                response_body_bytes: None,
            },
        )
        .await;
    assert!(matches!(zero_timeout, Err(Error::InvalidConfiguration)));
    let zero_body = client
        .execute(
            request(),
            Operation {
                deadline: Instant::now() + Duration::from_secs(1),
                timeout: None,
                response_body_bytes: Some(0),
            },
        )
        .await;
    assert!(matches!(zero_body, Err(Error::InvalidConfiguration)));
    let wider_body = client
        .execute(
            request(),
            Operation {
                deadline: Instant::now() + Duration::from_secs(1),
                timeout: None,
                response_body_bytes: Some(limits().response_body_bytes + 1),
            },
        )
        .await;
    assert!(matches!(wider_body, Err(Error::InvalidConfiguration)));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn request_is_not_replayed_after_the_peer_received_it() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("replay listener");
    let address = listener.local_addr().expect("replay address");
    let acceptor = fixture_acceptor(&material);
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
        fixture_client(address, &material)
            .execute(request(), operation())
            .await,
        Err(Error::Transport { .. })
    ));
    assert!(
        !server.await.expect("replay fixture joins"),
        "request was replayed"
    );
}

#[tokio::test]
async fn completed_exchange_reuses_an_idle_https_connection() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) = tls_server_reuses_one_connection(&material).await;
    let client = fixture_client(address, &material);
    client
        .execute(request(), operation())
        .await
        .expect("first pooled response");
    let second = client
        .execute(request(), operation())
        .await
        .expect("second pooled response");
    assert_eq!(second.status(), http::StatusCode::NO_CONTENT);
    assert_eq!(server.await.expect("pool fixture joins"), 1);
}

#[tokio::test]
async fn header_stripping_and_fixed_authority_apply_on_the_wire() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, captured, server) =
        tls_server_capture(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let mut request = Request::get("//other.fixture.test/items%23safe?q=%23")
        .body(Bytes::new())
        .expect("origin-form request");
    for (name, value) in [
        ("traceparent", "00-abc-def-01"),
        ("tracestate", "vendor=value"),
        ("baggage", "key=value"),
        ("x-request-id", "correlation"),
        ("accept-encoding", "gzip"),
    ] {
        request
            .headers_mut()
            .insert(name, value.parse().expect("header value"));
    }
    fixture_client(address, &material)
        .execute(request, operation())
        .await
        .expect("captured request response");
    let wire = String::from_utf8(captured.await.expect("captured request")).expect("ASCII request");
    assert!(wire.starts_with("GET //other.fixture.test/items%23safe?q=%23 HTTP/1.1\r\n"));
    for removed in [
        "traceparent:",
        "tracestate:",
        "baggage:",
        "x-request-id:",
        "accept-encoding:",
    ] {
        assert!(
            !wire.to_ascii_lowercase().contains(removed),
            "{removed} leaked to the wire"
        );
    }
    server.await.expect("capture fixture server succeeds");
}

#[tokio::test]
async fn future_drop_releases_admission_after_response_headers() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) = tls_server_stall_after_headers(&material).await;
    let head_observed = Arc::new(tokio::sync::Notify::new());
    let mut client = fixture_client(address, &material);
    client.response_head_observed = Some(head_observed.clone());
    let exchange_client = client.clone();
    let exchange =
        tokio::spawn(async move { exchange_client.execute(request(), operation()).await });
    tokio::time::timeout(Duration::from_secs(2), head_observed.notified())
        .await
        .expect("client admits response headers before pending body read");
    assert!(matches!(
        client.execute(request(), operation()).await,
        Err(Error::AtCapacity)
    ));
    exchange.abort();
    assert!(
        exchange
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
async fn narrower_timeout_covers_stalled_body_and_releases_admission() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) = tls_server_stall_after_headers(&material).await;
    let head_observed = Arc::new(tokio::sync::Notify::new());
    let mut client = fixture_client(address, &material);
    client.response_head_observed = Some(head_observed.clone());
    let exchange_client = client.clone();
    let exchange = tokio::spawn(async move {
        exchange_client
            .execute(
                request(),
                Operation {
                    deadline: Instant::now() + Duration::from_secs(1),
                    timeout: Some(Duration::from_millis(50)),
                    response_body_bytes: None,
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), head_observed.notified())
        .await
        .expect("client admits response headers before stalled body");
    assert!(matches!(
        exchange.await.expect("timed exchange joins"),
        Err(Error::Timeout { source: None })
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

#[test]
fn base_url_names_one_public_https_origin() {
    for base in [
        "https://provider.example",
        "https://provider.example/",
        "https://provider.example:8443",
        "https://8.8.8.8/",
    ] {
        assert!(policy::admit_base(base).is_ok(), "{base} must be admitted");
    }
    // A literal host never reaches the resolver, so this is its only admission.
    for base in [
        "https://127.0.0.1/",
        "https://[::ffff:127.0.0.1]/",
        "https://169.254.169.254/",
        "https://[fd00:ec2::254]/",
    ] {
        assert!(
            matches!(Client::new(base, limits()), Err(Error::Denied)),
            "{base} must be denied"
        );
    }
    // The request path replaces any base path, so a base path is refused
    // instead of being silently ignored.
    for base in [
        "http://provider.example/",
        "https://user:secret@provider.example/",
        "https://provider.example/?q=1",
        "https://provider.example/#fragment",
        " https://provider.example/",
        "https://provider.example/\u{0085}",
        "https://provider.example/v1",
        "https://provider.example/v1/",
    ] {
        assert!(
            matches!(
                Client::new(base, limits()),
                Err(Error::InvalidConfiguration)
            ),
            "{base} must be refused"
        );
    }
}

#[test]
fn uri_admission_uses_the_represented_origin_form() {
    let base = policy::admit_base("https://authn.fixture.test").expect("base URL");
    let target = policy::admit_target(
        &base,
        &"//other.fixture.test/items%23safe?q=%23"
            .parse()
            .expect("origin-form URI"),
    )
    .expect("double-slash path is not an authority");
    assert_eq!(
        target.as_str(),
        "https://authn.fixture.test//other.fixture.test/items%23safe?q=%23"
    );

    let parsed_fragment: http::Uri = "/items#fragment".parse().expect("dynamic URI");
    assert_eq!(parsed_fragment.path(), "/items");
    assert!(policy::admit_target(&base, &parsed_fragment).is_ok());
    for invalid in ["https://other.fixture.test/items", "items", "*"] {
        let uri = invalid.parse().expect("HTTP URI grammar");
        assert!(matches!(
            policy::admit_target(&base, &uri),
            Err(Error::InvalidTarget)
        ));
    }
}

#[test]
fn fixed_authority_policy_rejects_host_and_invalid_limits() {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::HOST,
        "other.fixture.test".parse().expect("host header"),
    );
    assert!(matches!(
        policy::admit_request_headers(headers),
        Err(Error::Denied)
    ));
    let mutations: [fn(&mut Limits); 5] = [
        |limits: &mut Limits| limits.max_active = 0,
        |limits: &mut Limits| limits.operation_timeout = Duration::ZERO,
        |limits: &mut Limits| limits.response_header_count = 0,
        |limits: &mut Limits| limits.response_header_bytes = 0,
        |limits: &mut Limits| limits.response_body_bytes = 0,
    ];
    for mutate in mutations {
        let mut invalid = limits();
        mutate(&mut invalid);
        assert!(matches!(
            policy::validate_limits(&invalid),
            Err(Error::InvalidConfiguration)
        ));
    }
}

#[tokio::test]
async fn transport_source_redacts_request_and_response_content() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 200 OK\r\nContent-Length: 23\r\n\r\nresponse-sentinel-body",
    )
    .await;
    let mut request = Request::get("/sentinel-path?token=request-query-sentinel")
        .body(Bytes::from_static(b"request-sentinel-body"))
        .expect("request with sentinel target");
    request.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer request-header-sentinel"
            .parse()
            .expect("credential header"),
    );
    let error = fixture_client(address, &material)
        .execute(request, operation())
        .await
        .expect_err("truncated response fails");
    let mut diagnostics = format!("{error:?} {error}");
    let mut source = error.source();
    while let Some(current) = source {
        write!(diagnostics, " {current:?} {current}").expect("format diagnostics");
        source = current.source();
    }
    for sentinel in [
        "sentinel-path",
        "request-query-sentinel",
        "request-header-sentinel",
        "request-sentinel-body",
        "response-sentinel-body",
    ] {
        assert!(
            !diagnostics.contains(sentinel),
            "transport diagnostics leaked {sentinel}"
        );
    }
    server.await.expect("truncated fixture server succeeds");
}
