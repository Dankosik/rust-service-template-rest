use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use http::Request;
use hyper_util::client::legacy::connect::dns::Name;
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
    server::TlsStream,
};

use tls_test_support::TlsMaterial;

use crate::{Client, Limits, Url, policy};

pub(crate) const FIXTURE_HOST: &str = "provider.fixture.test";
pub(crate) const FIXTURE_URL: &str = "https://provider.fixture.test/fixture";

pub(crate) type SpanFields = BTreeMap<&'static str, String>;

#[derive(Clone, Default)]
pub(crate) struct SpanDiagnostics(pub(crate) Arc<Mutex<Vec<SpanFields>>>);

pub(crate) struct FieldVisitor(pub(crate) SpanFields);

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

pub(crate) fn observation_recorder() -> metrics_exporter_prometheus::PrometheusRecorder {
    PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(crate::REQUEST_DURATION_METRIC.to_owned()),
            crate::REQUEST_DURATION_BUCKETS,
        )
        .expect("observation buckets are valid")
        .build_recorder()
}

pub(crate) fn recorded_count(scrape: &str, required_labels: &[&str]) -> usize {
    scrape
        .lines()
        .filter(|line| {
            line.starts_with("http_client_request_duration_seconds_count")
                && required_labels.iter().all(|label| line.contains(label))
                && line.ends_with(" 1")
        })
        .count()
}

#[derive(Clone, Default)]
pub(crate) struct FixtureResolver(Arc<Mutex<Vec<SocketAddr>>>);

impl FixtureResolver {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_addresses(addresses: Vec<SocketAddr>) -> Self {
        Self(Arc::new(Mutex::new(addresses)))
    }

    pub(crate) fn replace(&self, addresses: Vec<SocketAddr>) {
        *self.0.lock().expect("fixture resolver addresses") = addresses;
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
        let addresses = self.0.lock().expect("fixture resolver addresses").clone();
        std::future::ready(if addresses.is_empty() {
            Err(std::io::Error::other("no fixture listener"))
        } else {
            Ok(addresses.into_iter())
        })
    }
}

pub(crate) fn limits() -> Limits {
    Limits {
        operation_timeout: Duration::from_secs(1),
        response_header_count: 8,
        response_body_bytes: 128,
    }
}

pub(crate) fn fixture_client(address: SocketAddr, material: &TlsMaterial) -> Client {
    fixture_client_with_limits(address, material, limits())
}

pub(crate) fn fixture_client_with_limits(
    address: SocketAddr,
    material: &TlsMaterial,
    limits: Limits,
) -> Client {
    fixture_client_for_host(FIXTURE_HOST, address, material, limits)
}

pub(crate) fn fixture_client_for_host(
    host: &str,
    address: SocketAddr,
    material: &TlsMaterial,
    limits: Limits,
) -> Client {
    fixture_client_resolving(host, vec![address], material, limits)
}

pub(crate) fn fixture_client_resolving(
    host: &str,
    addresses: Vec<SocketAddr>,
    material: &TlsMaterial,
    limits: Limits,
) -> Client {
    fixture_client_with_resolver(
        host,
        FixtureResolver::with_addresses(addresses),
        material,
        limits,
    )
}

pub(crate) fn fixture_client_with_resolver(
    host: &str,
    resolver: FixtureResolver,
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
    let origin = policy::admit_origin(&url(&format!("https://{host}/"))).expect("fixture origin");
    Client::with_transport(origin, limits, tls, resolver)
}

pub(crate) fn url(raw: &str) -> Url {
    Url::parse(raw).expect("fixture URL")
}

pub(crate) fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(1)
}

pub(crate) fn request() -> Request<Bytes> {
    request_to(FIXTURE_URL)
}

pub(crate) fn request_to(uri: &str) -> Request<Bytes> {
    Request::get(uri)
        .body(Bytes::new())
        .expect("fixture request")
}

pub(crate) fn fixture_acceptor(material: &TlsMaterial) -> TlsAcceptor {
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

pub(crate) async fn read_request_headers<S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
) -> Vec<u8> {
    let mut headers = Vec::new();
    let mut chunk = [0_u8; 256];
    while !headers.array_windows().any(|bytes| bytes == b"\r\n\r\n") {
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

async fn spawn_tls_fixture<T, F, Fut>(
    material: &TlsMaterial,
    handler: F,
) -> (SocketAddr, JoinHandle<T>)
where
    T: Default + Send + 'static,
    F: FnOnce(TlsStream<TcpStream>) -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let acceptor = fixture_acceptor(material);
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (socket, _) = listener.accept().await.expect("fixture accepts connection");
            match acceptor.accept(socket).await {
                Ok(stream) => handler(stream).await,
                Err(_) => T::default(),
            }
        })
        .await
        .expect("TLS fixture completes within its budget")
    });
    (address, server)
}

async fn respond_and_close(mut stream: TlsStream<TcpStream>, response: &[u8]) {
    stream
        .write_all(response)
        .await
        .expect("fixture writes response");
    stream.shutdown().await.expect("fixture closes response");
}

pub(crate) async fn tls_server(
    material: &TlsMaterial,
    response: &'static [u8],
) -> (SocketAddr, JoinHandle<()>) {
    spawn_tls_fixture(material, move |mut stream| async move {
        read_request_headers(&mut stream).await;
        respond_and_close(stream, response).await;
    })
    .await
}

pub(crate) async fn tls_server_capture(
    material: &TlsMaterial,
    response: &'static [u8],
) -> (SocketAddr, oneshot::Receiver<String>, JoinHandle<()>) {
    let (captured_send, captured) = oneshot::channel();
    let (address, server) = spawn_tls_fixture(material, move |mut stream| async move {
        let request = read_request_headers(&mut stream).await;
        captured_send
            .send(String::from_utf8(request).expect("ASCII request"))
            .expect("capture receiver stays available");
        respond_and_close(stream, response).await;
    })
    .await;
    (address, captured, server)
}

pub(crate) async fn tls_server_stall_after_headers(
    material: &TlsMaterial,
) -> (SocketAddr, JoinHandle<()>) {
    spawn_tls_fixture(material, |mut stream| async move {
        read_request_headers(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n")
            .await
            .expect("stall fixture writes headers");
        stream.flush().await.expect("stall fixture flushes headers");
        std::future::pending::<()>().await;
    })
    .await
}

pub(crate) async fn tls_server_reuses_one_connection(
    material: &TlsMaterial,
) -> (SocketAddr, JoinHandle<usize>) {
    spawn_tls_fixture(material, |mut stream| async move {
        read_request_headers(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .expect("first pooled response");
        stream.flush().await.expect("first pooled response flushes");
        read_request_headers(&mut stream).await;
        respond_and_close(
            stream,
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        1
    })
    .await
}
