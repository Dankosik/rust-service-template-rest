#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "bounded local TLS fixtures make setup failures test failures"
)]

use std::{collections::BTreeMap, sync::Mutex};
use std::{
    error::Error as _,
    fmt::Write as _,
    net::SocketAddr,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use http::{Request, Version, header};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
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

use hyper_util::client::legacy::connect::dns::Name;

use crate::{
    BuildError, Client, Error, Limits, Url, UrlTemplate, build_transport, observe, policy,
    tls::TlsMaterial,
};

const FIXTURE_HOST: &str = "authn.fixture.test";
const FIXTURE_URL: &str = "https://authn.fixture.test/fixture";

type SpanFields = BTreeMap<&'static str, String>;

#[derive(Clone, Default)]
struct SpanDiagnostics(Arc<Mutex<Vec<SpanFields>>>, Duration);

struct FieldVisitor(SpanFields);

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name(), format!("{value:?}"));
    }
}

impl tracing::Subscriber for SpanDiagnostics {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.name() == "outbound_http"
    }

    fn new_span(&self, attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        if !self.1.is_zero() {
            #[allow(
                clippy::disallowed_methods,
                reason = "the synchronous preparation fixture deliberately spends the admitted budget before dispatch"
            )]
            std::thread::sleep(self.1);
        }
        let mut fields = FieldVisitor(SpanFields::new());
        attributes.record(&mut fields);
        let mut spans = self.0.lock().expect("span diagnostic lock");
        spans.push(fields.0);
        tracing::span::Id::from_u64(spans.len() as u64)
    }

    fn record(&self, id: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        let mut fields = FieldVisitor(SpanFields::new());
        values.record(&mut fields);
        self.0
            .lock()
            .expect("span diagnostic lock")
            .get_mut(usize::try_from(id.into_u64() - 1).expect("span index"))
            .expect("outbound span exists")
            .extend(fields.0);
    }

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, _: &tracing::Event<'_>) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn observation_recorder() -> metrics_exporter_prometheus::PrometheusRecorder {
    PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(crate::REQUEST_DURATION_METRIC.to_owned()),
            crate::REQUEST_DURATION_BUCKETS,
        )
        .expect("observation buckets are valid")
        .build_recorder()
}

fn recorded_count(scrape: &str, required_labels: &[&str]) -> usize {
    scrape
        .lines()
        .filter(|line| {
            line.starts_with("http_client_request_duration_seconds_count")
                && required_labels.iter().all(|label| line.contains(label))
                && line.ends_with(" 1")
        })
        .count()
}

/// Resolves every name to the fixture addresses, in order. Clients built by
/// the public constructors in tests have none, so they cannot reach the
/// network.
#[derive(Clone, Default)]
pub(crate) struct FixtureResolver(Vec<SocketAddr>);

impl FixtureResolver {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl tower::Service<Name> for FixtureResolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = std::io::Error;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Name) -> Self::Future {
        std::future::ready(if self.0.is_empty() {
            Err(std::io::Error::other("no fixture listener"))
        } else {
            Ok(self.0.clone().into_iter())
        })
    }
}

fn limits() -> Limits {
    Limits {
        operation_timeout: Duration::from_secs(1),
        response_header_count: 8,
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
    fixture_client_for_host(FIXTURE_HOST, address, material, limits)
}

fn fixture_client_for_host(
    host: &str,
    address: SocketAddr,
    material: &TlsMaterial,
    limits: Limits,
) -> Client {
    fixture_client_resolving(host, vec![address], material, limits)
}

fn fixture_client_resolving(
    host: &str,
    addresses: Vec<SocketAddr>,
    material: &TlsMaterial,
    limits: Limits,
) -> Client {
    let mut roots = tokio_rustls::rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(material.root.clone()))
        .expect("fixture root certificate");
    let tls = tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("fixture TLS protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    let target = policy::admit_origin(&url(&format!("https://{host}/"))).expect("fixture origin");
    Client {
        server: observe::Server::new(&target),
        target,
        limits,
        propagate_trace_context: false,
        transport: build_transport(&limits, true, tls, FixtureResolver(addresses)),
    }
}

fn url(raw: &str) -> Url {
    Url::parse(raw).expect("fixture URL")
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(1)
}

fn request() -> Request<Bytes> {
    request_to(FIXTURE_URL)
}

fn request_to(uri: &str) -> Request<Bytes> {
    Request::get(uri)
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
        .execute(request(), deadline())
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
            .execute(request(), deadline())
            .await,
        Err(Error::Transport { .. })
    ));
    server.await.expect("untrusted fixture server succeeds");

    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let client = fixture_client_for_host("different.fixture.test", address, &material, limits());
    assert!(matches!(
        client
            .execute(
                request_to("https://different.fixture.test/fixture"),
                deadline()
            )
            .await,
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
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\no\r\n1\r\nk\r\n1\r\n!\r\n0\r\n\r\n"
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
            .execute(request(), deadline())
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
async fn fragmented_body_preserves_bytes_and_discards_trailers() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTrailer: X-Fixture\r\n\r\n\
          1\r\na\r\n2\r\nbc\r\n3\r\ndef\r\n5\r\nghijk\r\n8\r\nlmnopqrs\r\n7\r\ntuvwxyz\r\n\
          0\r\nX-Fixture: discarded\r\n\r\n",
    )
    .await;
    let response = fixture_client_with_limits(
        address,
        &material,
        Limits {
            response_body_bytes: 26,
            ..limits()
        },
    )
    .execute(request(), deadline())
    .await
    .expect("fragmented exact-ceiling response reaches EOF");
    assert_eq!(response.body().as_ref(), b"abcdefghijklmnopqrstuvwxyz");
    assert!(!response.headers().contains_key("x-fixture"));
    server.await.expect("fragmented fixture server succeeds");
}

#[tokio::test]
async fn parser_header_count_overflow_is_a_transport_error() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let mut parser_limit = limits();
    parser_limit.response_header_count = 1;
    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-Overflow: one\r\n\r\n",
    )
    .await;
    assert!(matches!(
        fixture_client_with_limits(address, &material, parser_limit)
            .execute(request(), deadline())
            .await,
        Err(Error::Transport { .. })
    ));
    server.await.expect("parser header fixture joins");
}

#[tokio::test]
async fn response_head_beyond_the_parser_buffer_is_a_transport_error() {
    // Hyper's default HTTP/1 buffer ceiling is 8192 + 4096 * 100 bytes.
    const HEAD_BYTES: usize = 512 * 1024;
    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("head fixture listener");
    let address = listener.local_addr().expect("head fixture address");
    let acceptor = fixture_acceptor(&material);
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (socket, _) = listener.accept().await.expect("head fixture accepts");
            let mut stream = acceptor.accept(socket).await.expect("head TLS handshake");
            read_request_headers(&mut stream).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-Large: {}\r\n\r\n",
                "a".repeat(HEAD_BYTES)
            );
            // The client closes once its buffer is full; the write may fail.
            let _ = stream.write_all(head.as_bytes()).await;
        })
        .await
        .expect("head fixture completes within its budget");
    });
    assert!(matches!(
        fixture_client(address, &material)
            .execute(request(), deadline())
            .await,
        Err(Error::Transport { .. })
    ));
    server.await.expect("head fixture joins");
}

const REQUEST_SENTINELS: [&str; 6] = [
    "sentinel-path",
    "sentinel-query",
    "sentinel-header",
    "sentinel-body",
    "sentinel-value",
    "SENTINEL-METHOD",
];

async fn exercise_completed_attempts() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let client = fixture_client_with_limits(
        "127.0.0.1:0".parse().expect("unused address"),
        &material,
        Limits {
            response_body_bytes: 2,
            ..limits()
        },
    );
    let unpolled = client.execute(request(), deadline());
    drop(unpolled);

    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    let success = fixture_client(address, &material)
        .execute(request(), deadline())
        .await;
    assert!(success.is_ok());
    server.await.expect("success fixture joins");

    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    let not_found = fixture_client(address, &material)
        .execute(request(), deadline())
        .await
        .expect("HTTP error statuses remain responses");
    assert_eq!(not_found.status(), http::StatusCode::NOT_FOUND);
    server.await.expect("not-found fixture joins");

    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 600 Fixture Error\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    let custom_error = fixture_client(address, &material)
        .execute(request(), deadline())
        .await
        .expect("600 status remains a response");
    assert_eq!(custom_error.status().as_u16(), 600);
    server.await.expect("600-status fixture joins");

    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 999 Fixture Error\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    let custom_error = fixture_client(address, &material)
        .execute(request(), deadline())
        .await
        .expect("999 status remains a response");
    assert_eq!(custom_error.status().as_u16(), 999);
    server.await.expect("999-status fixture joins");

    // Refused before I/O, so it is not an attempt.
    assert!(matches!(
        client
            .execute(request_to("https://other.fixture.test/items"), deadline())
            .await,
        Err(Error::InvalidTarget)
    ));

    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 3\r\n\r\nno!",
    )
    .await;
    let mut request = Request::builder()
        .method(http::Method::from_bytes(b"SENTINEL-METHOD").expect("method"))
        .uri("https://authn.fixture.test/sentinel-path?token=sentinel-query")
        .header(header::AUTHORIZATION, "Bearer sentinel-header")
        .body(Bytes::from_static(b"sentinel-body"))
        .expect("sensitive fixture request");
    request
        .headers_mut()
        .insert("x-sentinel", "sentinel-value".parse().expect("header"));
    assert!(matches!(
        fixture_client_with_limits(
            address,
            &material,
            Limits {
                response_body_bytes: 2,
                ..limits()
            },
        )
        .execute(request, deadline())
        .await,
        Err(Error::ResponseBodyTooLarge)
    ));
    server.await.expect("body-limit fixture joins");
}

#[test]
fn observation_records_polled_attempts_once_without_request_data() {
    let recorder = observation_recorder();
    let diagnostics = SpanDiagnostics::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    metrics::with_local_recorder(&recorder, || {
        tracing::subscriber::with_default(diagnostics.clone(), || {
            // Keep callsite interest independent of a sibling test's thread-local
            // subscriber; tracing-core 0.1.36 otherwise has a single-dispatcher fast path.
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            runtime.block_on(exercise_completed_attempts());
        });
    });

    let scrape = recorder.handle().render();
    assert_eq!(
        recorded_count(
            &scrape,
            &[
                "outbound_outcome=\"response\"",
                "http_request_method=\"GET\""
            ],
        ),
        4
    );
    for status in ["404", "600", "999"] {
        assert_eq!(
            recorded_count(
                &scrape,
                &[
                    "outbound_outcome=\"response\"",
                    &format!("error_type=\"{status}\""),
                    &format!("http_response_status_code=\"{status}\""),
                ],
            ),
            1,
            "{status} keeps matching status and error labels",
        );
    }
    assert_eq!(
        recorded_count(&scrape, &["error_type=\"invalid_target\""]),
        0
    );
    assert_eq!(
        recorded_count(
            &scrape,
            &[
                "outbound_outcome=\"error\"",
                "error_type=\"response_body_too_large\"",
                "http_request_method=\"_OTHER\"",
                "http_response_status_code=\"502\"",
            ],
        ),
        1
    );
    for secret in REQUEST_SENTINELS {
        assert!(!scrape.contains(secret), "metric disclosed request data");
    }
    assert!(
        !scrape.contains("url_template"),
        "a request without a template has no template label"
    );

    let spans = diagnostics.0.lock().expect("span diagnostic lock");
    assert_eq!(
        spans.len(),
        5,
        "unpolled futures and refused targets are not attempts"
    );
    let body_failure = spans
        .iter()
        .find(|fields| {
            fields
                .get("error.type")
                .is_some_and(|value| value.contains("response_body_too_large"))
        })
        .expect("body-limit attempt span");
    assert!(
        body_failure
            .get("http.response.status_code")
            .is_some_and(|value| value.contains("502"))
    );
    assert!(
        body_failure
            .get("http.request.method")
            .is_some_and(|value| value.contains("_OTHER"))
    );
    assert!(
        body_failure
            .get("otel.status_code")
            .is_some_and(|value| value.contains("ERROR"))
    );
    for fields in spans.iter() {
        let fields = format!("{fields:?}");
        for secret in REQUEST_SENTINELS {
            assert!(!fields.contains(secret), "span disclosed request data");
        }
    }
}

#[test]
fn url_template_names_the_operation_in_the_span_and_the_metric() {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use tracing_subscriber::layer::SubscriberExt as _;

    let recorder = observation_recorder();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    metrics::with_local_recorder(&recorder, || {
        tracing::subscriber::with_default(subscriber, || {
            // See `observation_records_polled_attempts_once_without_request_data`.
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            runtime.block_on(async {
                let material = TlsMaterial::new(FIXTURE_HOST);
                let (address, captured, server) = tls_server_capture(
                    &material,
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
                )
                .await;
                let mut request = request_to("https://authn.fixture.test/items/sentinel-path");
                request.extensions_mut().insert(UrlTemplate("/items/{id}"));
                fixture_client(address, &material)
                    .execute(request, deadline())
                    .await
                    .expect("HTTP error statuses remain responses");
                let wire = captured.await.expect("captured request");
                assert!(wire.starts_with(b"GET /items/sentinel-path HTTP/1.1\r\n"));
                server.await.expect("capture fixture server succeeds");
            });
        });
    });

    let scrape = recorder.handle().render();
    assert_eq!(
        recorded_count(
            &scrape,
            &[
                "url_template=\"/items/{id}\"",
                "http_request_method=\"GET\"",
                "http_response_status_code=\"404\"",
            ],
        ),
        1,
        "{scrape}"
    );
    assert!(
        !scrape.contains("sentinel-path"),
        "metric disclosed the path"
    );

    let spans = exporter.get_finished_spans().expect("exported spans");
    let [span] = spans.as_slice() else {
        panic!("one client span, got {spans:?}");
    };
    assert_eq!(span.name, "GET /items/{id}");
    assert!(span.attributes.iter().any(|attribute| {
        attribute.key.as_str() == "url.template" && attribute.value.as_str() == "/items/{id}"
    }));
    assert!(
        !format!("{span:?}").contains("sentinel-path"),
        "span disclosed the path"
    );
}

#[test]
fn observation_records_timeout_and_polled_drop_once() {
    let recorder = observation_recorder();
    let diagnostics = SpanDiagnostics::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    metrics::with_local_recorder(&recorder, || {
        tracing::subscriber::with_default(diagnostics.clone(), || {
            // Keep callsite interest independent of a sibling test's thread-local
            // subscriber; tracing-core 0.1.36 otherwise has a single-dispatcher fast path.
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            runtime.block_on(async {
                let material = TlsMaterial::new(FIXTURE_HOST);
                // The recorded 200 status proves the timeout struck after the
                // response headers, while the body was pending.
                let (address, server) = tls_server_stall_after_headers(&material).await;
                let timed = fixture_client(address, &material)
                    .execute(request(), deadline())
                    .await;
                assert!(matches!(timed, Err(Error::Timeout)));
                server.abort();
                assert!(
                    server
                        .await
                        .expect_err("timeout fixture aborts")
                        .is_cancelled()
                );

                let (address, server) = tls_server_stall_after_headers(&material).await;
                let client = fixture_client(address, &material);
                let mut exchange = Box::pin(client.execute(request(), deadline()));
                assert!(
                    tokio::time::timeout(Duration::from_millis(200), &mut exchange)
                        .await
                        .is_err(),
                    "stalled exchange completed unexpectedly"
                );
                drop(exchange);
                server.abort();
                assert!(
                    server
                        .await
                        .expect_err("drop fixture aborts")
                        .is_cancelled()
                );
            });
        });
    });

    let scrape = recorder.handle().render();
    assert_eq!(
        recorded_count(
            &scrape,
            &[
                "outbound_outcome=\"error\"",
                "error_type=\"timeout\"",
                "http_response_status_code=\"200\"",
            ],
        ),
        1
    );
    assert_eq!(
        recorded_count(&scrape, &["outbound_outcome=\"cancelled\""]),
        1
    );
    let spans = diagnostics.0.lock().expect("span diagnostic lock");
    assert_eq!(spans.len(), 2);
    assert!(spans.iter().any(|fields| {
        fields
            .get("outbound.outcome")
            .is_some_and(|value| value.contains("cancelled"))
            && !fields.contains_key("error.type")
    }));
}

/// The name of the span an event was logged in, and the event's fields.
type EventFields = (Option<&'static str>, SpanFields);

/// Events of this crate.
#[derive(Clone, Default)]
struct EventDiagnostics(Arc<Mutex<Vec<EventFields>>>);

impl<S> tracing_subscriber::Layer<S> for EventDiagnostics
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if !event.metadata().target().starts_with("infra_outbound_http") {
            return;
        }
        let mut fields = FieldVisitor(SpanFields::new());
        event.record(&mut fields);
        let span = context.event_span(event).map(|span| span.name());
        self.0
            .lock()
            .expect("event diagnostic lock")
            .push((span, fields.0));
    }
}

/// One failure of each transport class, in the order `tls`, `connect`,
/// `protocol`, `transport`. Returns the deepest source of each error.
async fn exercise_transport_failures() -> Vec<String> {
    let sensitive_request = || {
        Request::get("https://authn.fixture.test/sentinel-path?token=sentinel-query")
            .header(header::AUTHORIZATION, "Bearer sentinel-header")
            .body(Bytes::from_static(b"sentinel-body"))
            .expect("sensitive fixture request")
    };
    let root_cause = |result: Result<http::Response<Bytes>, Error>| {
        let error = result.expect_err("transport failure");
        assert!(matches!(error, Error::Transport { .. }), "{error:?}");
        std::iter::successors(error.source(), |cause| (*cause).source())
            .last()
            .expect("transport source")
            .to_string()
    };
    let mut roots = Vec::new();
    let material = TlsMaterial::new(FIXTURE_HOST);

    let (address, server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let mut untrusted = TlsMaterial::new(FIXTURE_HOST);
    untrusted.root = material.untrusted_root.clone();
    let untrusted = fixture_client(address, &untrusted)
        .execute(sensitive_request(), deadline())
        .await;
    roots.push(root_cause(untrusted));
    server.await.expect("untrusted fixture joins");

    let unresolved = fixture_client_resolving(FIXTURE_HOST, vec![], &material, limits())
        .execute(sensitive_request(), deadline())
        .await;
    roots.push(root_cause(unresolved));

    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-Overflow: one\r\n\r\n",
    )
    .await;
    let overflow = fixture_client_with_limits(
        address,
        &material,
        Limits {
            response_header_count: 1,
            ..limits()
        },
    )
    .execute(sensitive_request(), deadline())
    .await;
    roots.push(root_cause(overflow));
    server.await.expect("parser fixture joins");

    let (address, server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\no").await;
    let truncated = fixture_client(address, &material)
        .execute(sensitive_request(), deadline())
        .await;
    roots.push(root_cause(truncated));
    server.await.expect("truncated fixture joins");
    roots
}

#[test]
fn transport_failures_are_classed_and_their_cause_is_logged_in_the_client_span() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let recorder = observation_recorder();
    let diagnostics = EventDiagnostics::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let roots = metrics::with_local_recorder(&recorder, || {
        let subscriber = tracing_subscriber::registry().with(diagnostics.clone());
        tracing::subscriber::with_default(subscriber, || {
            // See `observation_records_polled_attempts_once_without_request_data`.
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            runtime.block_on(exercise_transport_failures())
        })
    });

    let scrape = recorder.handle().render();
    for (class, after_headers) in [
        ("tls", false),
        ("connect", false),
        ("protocol", false),
        // The body broke after a complete response head.
        ("transport", true),
    ] {
        let class_label = format!("error_type=\"{class}\"");
        assert_eq!(
            recorded_count(&scrape, &["outbound_outcome=\"error\"", &class_label]),
            1,
            "one {class} attempt in {scrape}",
        );
        assert_eq!(
            scrape
                .lines()
                .any(|line| line.contains(&class_label)
                    && line.contains("http_response_status_code")),
            after_headers,
            "{class} status label",
        );
    }

    let events = diagnostics.0.lock().expect("event diagnostic lock");
    let causes: Vec<&str> = events
        .iter()
        .map(|(span, fields)| {
            assert_eq!(*span, Some("outbound_http"));
            assert_eq!(fields["message"], "outbound_http_transport_failed");
            fields["error"].as_str()
        })
        .collect();
    let [tls, connect, _protocol, _transport] = causes.as_slice() else {
        panic!("one event for each transport failure, got {causes:?}");
    };
    assert!(tls.contains("peer certificate"), "{tls}");
    assert!(connect.contains("no fixture listener"), "{connect}");
    for (cause, root) in causes.iter().zip(&roots) {
        assert!(cause.ends_with(root.as_str()), "{cause} lacks {root}");
    }
    for cause in &causes {
        for secret in REQUEST_SENTINELS {
            assert!(!cause.contains(secret), "{cause} disclosed request data");
        }
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
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\no\r\n1\r\nk\r\n"
                .as_slice(),
            "missing chunked EOF after exact-cap payload",
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\ninvalid-trailer\r\n\r\n"
                .as_slice(),
            "malformed trailer after exact-cap payload",
        ),
    ];
    for (wire, name) in cases {
        let (address, server) = tls_server(&material, wire).await;
        let error = fixture_client_with_limits(address, &material, ceiling)
            .execute(request(), deadline())
            .await
            .expect_err(name);
        match name {
            "advertised overflow" => assert!(matches!(error, Error::ResponseBodyTooLarge)),
            "missing exact-cap EOF"
            | "missing chunked EOF after exact-cap payload"
            | "malformed trailer after exact-cap payload" => {
                let Error::Transport { source } = error else {
                    panic!("{name} must retain a transport cause");
                };
                assert!(source.is::<hyper::Error>(), "{name} retains the body error");
            }
            _ => unreachable!("fixed test case name"),
        }
        server.await.expect("framing fixture server succeeds");
    }
}

#[tokio::test]
async fn expired_deadline_refuses_before_network_work() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("no-connect listener");
    let client = fixture_client(listener.local_addr().expect("listener address"), &material);
    let expired = client
        .execute(request(), Instant::now() - Duration::from_millis(1))
        .await;
    assert!(matches!(expired, Err(Error::Timeout)));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn stopped_request_context_refuses_both_entry_points_before_dispatch() {
    use operation_context::OperationContext;

    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let client = fixture_client(listener.local_addr().expect("address"), &material);
    for expired in [false, true] {
        let context = OperationContext::with_timeout(if expired {
            Duration::ZERO
        } else {
            Duration::from_secs(1)
        });
        if !expired {
            context.cancel();
        }
        let mut supplied = request();
        supplied.extensions_mut().insert(context.clone());
        assert!(matches!(
            client.execute(supplied, deadline()).await,
            Err(Error::Timeout)
        ));
        let mut supplied = request();
        supplied.extensions_mut().insert(context.clone());
        assert!(matches!(
            client
                .execute_with_context(supplied, &OperationContext::unbounded())
                .await,
            Err(Error::Timeout)
        ));
        assert!(matches!(
            client.execute_with_context(request(), &context).await,
            Err(Error::Timeout)
        ));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[test]
fn synchronous_preparation_cannot_restart_the_local_cutoff() {
    let diagnostics = SpanDiagnostics(Arc::default(), Duration::from_millis(30));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    tracing::subscriber::with_default(diagnostics.clone(), || {
        let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        runtime.block_on(async {
            let material = TlsMaterial::new(FIXTURE_HOST);
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
            let mut ceiling = limits();
            ceiling.operation_timeout = Duration::from_millis(10);
            let client = fixture_client_with_limits(
                listener.local_addr().expect("address"),
                &material,
                ceiling,
            );
            assert!(matches!(
                client.execute(request(), deadline()).await,
                Err(Error::Timeout)
            ));
            assert!(
                !diagnostics.0.lock().expect("spans").is_empty(),
                "preparation must cross the cutoff"
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(20), listener.accept())
                    .await
                    .is_err(),
                "expired preparation must not dispatch"
            );
        });
    });
}

#[tokio::test]
async fn context_cancellation_ends_an_incomplete_buffered_body() {
    use operation_context::OperationContext;

    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let acceptor = fixture_acceptor(&material);
    let (received, dispatched) = oneshot::channel();
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (socket, _) = listener.accept().await.expect("accept");
            let mut stream = acceptor.accept(socket).await.expect("TLS");
            read_request_headers(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\no")
                .await
                .expect("partial body");
            stream.flush().await.expect("flush");
            received.send(()).expect("client waiting");
            std::future::pending::<()>().await;
        })
        .await
        .expect("fixture bounded");
    });
    let client = fixture_client(address, &material);
    let context = OperationContext::with_timeout(Duration::from_secs(2));
    let exchange = client.execute_with_context(request(), &context);
    tokio::pin!(exchange);
    tokio::select! {
        result = &mut exchange => panic!("incomplete body completed: {result:?}"),
        result = dispatched => result.expect("request observed"),
    }
    context.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(200), exchange)
            .await
            .expect("cancellation must end waiting before the local ceiling"),
        Err(Error::Timeout)
    ));
    server.abort();
    assert!(server.await.expect_err("fixture cancelled").is_cancelled());
}

/// A listener that completes no further handshakes: its accept queue is
/// full, so the kernel drops new SYNs and a connect to it stays pending.
async fn unresponsive_listener() -> (SocketAddr, tokio::net::TcpListener, Vec<TcpStream>) {
    let socket = tokio::net::TcpSocket::new_v4().expect("unresponsive socket");
    socket
        .bind("127.0.0.1:0".parse().expect("loopback address"))
        .expect("unresponsive bind");
    let listener = socket.listen(1).expect("unresponsive listen");
    let address = listener.local_addr().expect("unresponsive address");
    let mut queued = Vec::new();
    for _ in 0..64 {
        match tokio::time::timeout(Duration::from_millis(200), TcpStream::connect(address)).await {
            Ok(stream) => queued.push(stream.expect("queued connection")),
            Err(_) => return (address, listener, queued),
        }
    }
    panic!("the accept queue never filled");
}

#[tokio::test]
async fn unresponsive_address_leaves_time_for_the_next_one() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (unresponsive, _listener, _queued) = unresponsive_listener().await;
    let (address, server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let mut limits = limits();
    limits.operation_timeout = Duration::from_secs(2);
    let response =
        fixture_client_resolving(FIXTURE_HOST, vec![unresponsive, address], &material, limits)
            .execute(request(), Instant::now() + Duration::from_secs(2))
            .await
            .expect("the second address answers");
    assert_eq!(response.status(), 200);
    server.await.expect("fallback fixture server succeeds");
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
            .execute(request(), deadline())
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
        .execute(request(), deadline())
        .await
        .expect("first pooled response");
    let second = client
        .execute(request(), deadline())
        .await
        .expect("second pooled response");
    assert_eq!(second.status(), http::StatusCode::NO_CONTENT);
    assert_eq!(server.await.expect("pool fixture joins"), 1);
}

#[tokio::test]
async fn target_and_caller_headers_reach_the_wire_unchanged() {
    // Framing headers are the exception: see
    // `buffered_body_length_replaces_caller_framing_on_the_wire`.
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, captured, server) =
        tls_server_capture(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let mut request =
        request_to("https://authn.fixture.test//other.fixture.test/items%23safe?q=%23");
    for (name, value) in [
        ("traceparent", "00-abc-def-01"),
        ("accept-encoding", "gzip"),
    ] {
        request
            .headers_mut()
            .insert(name, value.parse().expect("header value"));
    }
    fixture_client(address, &material)
        .execute(request, deadline())
        .await
        .expect("captured request response");
    let wire = String::from_utf8(captured.await.expect("captured request")).expect("ASCII request");
    assert!(wire.starts_with("GET //other.fixture.test/items%23safe?q=%23 HTTP/1.1\r\n"));
    let wire = wire.to_ascii_lowercase();
    assert!(wire.contains("host: authn.fixture.test\r\n"));
    assert!(wire.contains("traceparent: 00-abc-def-01"));
    assert!(wire.contains("accept-encoding: gzip"));
    server.await.expect("capture fixture server succeeds");
}

#[tokio::test]
async fn buffered_body_length_replaces_caller_framing_on_the_wire() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, captured, server) =
        tls_server_capture(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let request = Request::post(FIXTURE_URL)
        .version(Version::HTTP_2)
        .header(header::CONTENT_LENGTH, "2")
        .header(header::TRANSFER_ENCODING, "chunked")
        .body(Bytes::from_static(b"hello"))
        .expect("misframed request");
    fixture_client(address, &material)
        .execute(request, deadline())
        .await
        .expect("captured request response");
    let wire = String::from_utf8(captured.await.expect("captured request")).expect("ASCII request");
    assert!(wire.starts_with("POST /fixture HTTP/1.1\r\n"), "{wire}");
    let wire = wire.to_ascii_lowercase();
    assert!(wire.contains("content-length: 5\r\n"), "{wire}");
    assert_eq!(wire.matches("content-length:").count(), 1);
    assert!(!wire.contains("transfer-encoding"), "{wire}");
    server.await.expect("capture fixture server succeeds");
}

#[tokio::test]
async fn opted_in_client_sends_the_context_of_its_named_client_span() {
    use opentelemetry::trace::{SpanKind, TracerProvider as _};
    use opentelemetry_sdk::{
        propagation::TraceContextPropagator,
        trace::{InMemorySpanExporter, SdkTracerProvider},
    };
    use tracing_subscriber::layer::SubscriberExt as _;

    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _guard = tracing::subscriber::set_default(subscriber);
    // See `observation_records_polled_attempts_once_without_request_data`.
    let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());

    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, captured, server) =
        tls_server_capture(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let mut request = request();
    request.headers_mut().insert(
        "traceparent",
        "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
            .parse()
            .expect("header value"),
    );
    fixture_client(address, &material)
        .with_trace_context()
        .execute(request, deadline())
        .await
        .expect("captured request response");
    let wire = String::from_utf8(captured.await.expect("captured request")).expect("ASCII request");
    server.await.expect("capture fixture server succeeds");

    let spans = exporter.get_finished_spans().expect("exported spans");
    let [span] = spans.as_slice() else {
        panic!("one client span, got {spans:?}");
    };
    assert_eq!(span.name, "GET");
    assert_eq!(span.span_kind, SpanKind::Client);
    let context = &span.span_context;
    let expected = format!(
        "traceparent: 00-{}-{}-01\r\n",
        context.trace_id(),
        context.span_id()
    );
    let wire = wire.to_ascii_lowercase();
    assert!(wire.contains(&expected), "{wire} lacks {expected}");
    assert_eq!(wire.matches("traceparent:").count(), 1);
}

#[test]
fn client_binds_one_https_origin() {
    for origin in [
        "https://provider.example",
        "https://provider.example:8443",
        "https://provider.example/v1/token?ignored=1",
        "https://8.8.8.8/",
        "https://127.0.0.1/",
        "https://[::ffff:127.0.0.1]/",
        "https://169.254.169.254/",
        "https://[fd00:ec2::254]/",
    ] {
        assert!(
            Client::new(&url(origin), limits()).is_ok(),
            "{origin} must be admitted"
        );
    }
    for origin in [
        "http://provider.example/",
        "https://user:secret@provider.example/",
        "https://user@provider.example/",
        "data:text/plain,provider",
    ] {
        assert!(
            matches!(
                Client::new(&url(origin), limits()),
                Err(BuildError::InvalidConfiguration)
            ),
            "{origin} must be refused"
        );
    }
}

#[test]
fn requests_must_name_the_configured_origin() {
    let origin = policy::admit_origin(&url("https://authn.fixture.test")).expect("origin");
    for admitted in [
        "https://authn.fixture.test/items",
        "https://authn.fixture.test:443/items?q=1",
        "https://AUTHN.fixture.test/items",
    ] {
        assert!(
            policy::admit_request(&origin, request_to(admitted)).is_ok(),
            "{admitted} must be admitted"
        );
    }
    for refused in [
        "https://other.fixture.test/items",
        "https://authn.fixture.test:8443/items",
        "http://authn.fixture.test/items",
        "https://user@authn.fixture.test/items",
        "/items",
        "*",
    ] {
        assert!(
            matches!(
                policy::admit_request(&origin, request_to(refused)),
                Err(Error::InvalidTarget)
            ),
            "{refused} must be refused"
        );
    }
    let mut with_host = request();
    with_host.headers_mut().insert(
        header::HOST,
        "other.fixture.test".parse().expect("host header"),
    );
    assert!(matches!(
        policy::admit_request(&origin, with_host),
        Err(Error::InvalidTarget)
    ));
}

#[test]
fn admission_preserves_the_request_and_sets_the_transport_properties() {
    let origin = policy::admit_origin(&url("https://authn.fixture.test")).expect("origin");
    let request = Request::builder()
        .method(http::Method::PATCH)
        .uri("https://AUTHN.fixture.test:443/items")
        .version(Version::HTTP_2)
        .header("x-request-part", "preserved")
        .header(header::CONTENT_LENGTH, "4")
        .header(header::TRANSFER_ENCODING, "chunked")
        .body(Bytes::from_static(b"preserved"))
        .expect("request parts");
    let admitted = policy::admit_request(&origin, request).expect("admitted request");
    assert_eq!(admitted.method(), http::Method::PATCH);
    assert_eq!(admitted.version(), Version::HTTP_11);
    assert_eq!(admitted.headers()[header::CONTENT_LENGTH], "9");
    assert!(!admitted.headers().contains_key(header::TRANSFER_ENCODING));
    assert_eq!(admitted.uri(), "https://AUTHN.fixture.test:443/items");
    assert_eq!(admitted.headers()[header::HOST], "authn.fixture.test");
    assert_eq!(admitted.headers()[header::ACCEPT], "*/*");
    assert_eq!(admitted.headers()["x-request-part"], "preserved");
    assert_eq!(admitted.body().as_ref(), b"preserved");

    let port = policy::admit_origin(&url("https://[::1]:8443")).expect("IPv6 origin");
    let admitted = policy::admit_request(&port, request_to("https://[::1]:8443/items"))
        .expect("admitted IPv6 request");
    assert_eq!(admitted.headers()[header::HOST], "[::1]:8443");
    assert!(!admitted.headers().contains_key(header::CONTENT_LENGTH));

    // A provider may answer 411 to a content method without a length, and a
    // length on a request without content would be read from the next one.
    for (method, sent, expected) in [
        (http::Method::POST, None, Some("0")),
        (http::Method::POST, Some("7"), Some("0")),
        (http::Method::DELETE, Some("7"), None),
    ] {
        let mut request = Request::builder()
            .method(method.clone())
            .uri("https://authn.fixture.test/items")
            .body(Bytes::new())
            .expect("empty request");
        if let Some(sent) = sent {
            request
                .headers_mut()
                .insert(header::CONTENT_LENGTH, sent.parse().expect("length"));
        }
        let admitted = policy::admit_request(&origin, request).expect("admitted request");
        assert_eq!(
            admitted
                .headers()
                .get(header::CONTENT_LENGTH)
                .map(|value| value.to_str().expect("ASCII length")),
            expected,
            "{method} sent {sent:?}"
        );
    }
}

#[test]
fn limits_must_be_positive_and_bounded() {
    let mutations: [fn(&mut Limits); 4] = [
        |limits: &mut Limits| limits.operation_timeout = Duration::ZERO,
        |limits: &mut Limits| limits.response_header_count = 0,
        |limits: &mut Limits| limits.response_header_count = 32_769,
        |limits: &mut Limits| limits.response_body_bytes = 0,
    ];
    for mutate in mutations {
        let mut invalid = limits();
        mutate(&mut invalid);
        assert!(matches!(
            policy::validate_limits(&invalid),
            Err(BuildError::InvalidConfiguration)
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
    let mut request =
        Request::get("https://authn.fixture.test/sentinel-path?token=request-query-sentinel")
            .body(Bytes::from_static(b"request-sentinel-body"))
            .expect("request with sentinel target");
    request.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer request-header-sentinel"
            .parse()
            .expect("credential header"),
    );
    let error = fixture_client(address, &material)
        .execute(request, deadline())
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
