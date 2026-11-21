#[cfg(feature = "test-support")]
use crate::{Client, InvalidOrigin, Limits, Origin, TlsError, policy, transport};

#[cfg(feature = "test-support")]
#[derive(Debug, thiserror::Error)]
pub enum TestBuildError {
    #[error(transparent)]
    InvalidOrigin(#[from] InvalidOrigin),
    #[error(transparent)]
    Tls(#[from] TlsError),
}

#[cfg(feature = "test-support")]
#[must_use]
pub fn tls_error(source: Box<dyn std::error::Error + Send + Sync>) -> TlsError {
    TlsError::new(source)
}

#[cfg(feature = "test-support")]
pub fn client_http(origin: &str, limits: Limits) -> Result<Client, TestBuildError> {
    Client::try_with_default_transport(policy::admit_test_http_origin(origin)?, limits)
        .map_err(TestBuildError::from)
}

#[cfg(feature = "test-support")]
pub fn client_https(
    origin: Origin,
    limits: Limits,
    root_der: &[u8],
    address: std::net::SocketAddr,
) -> Result<Client, TestBuildError> {
    if !address.ip().is_loopback() {
        return Err(TestBuildError::InvalidOrigin(InvalidOrigin));
    }
    let tls = transport::tls_config_with_root(root_der)?;
    Ok(Client::with_transport(
        policy::TargetOrigin::Https(origin),
        limits,
        tls,
        transport::Resolver::Fixture(transport::FixtureResolver::with_addresses(vec![address])),
    ))
}

#[cfg(test)]
pub(crate) use unit::*;

#[cfg(test)]
mod unit {
    use std::{net::SocketAddr, time::Duration};

    use bytes::Bytes;
    use http::Request;
    use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
    use operation_context::OperationContext;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
        task::JoinHandle,
    };
    use tokio_rustls::{TlsAcceptor, server::TlsStream};

    use tls_test_support::TlsMaterial;

    pub(crate) use crate::transport::FixtureResolver;
    use crate::{
        Client, Limits, Origin,
        observe::{REQUEST_DURATION_BUCKETS, REQUEST_DURATION_METRIC},
        transport::{Resolver, tls_config_with_root},
    };

    pub(crate) const FIXTURE_HOST: &str = "provider.fixture.test";
    pub(crate) const FIXTURE_URL: &str = "https://provider.fixture.test/fixture";

    pub(crate) fn observation_recorder() -> metrics_exporter_prometheus::PrometheusRecorder {
        PrometheusBuilder::new()
            .set_buckets_for_metric(
                Matcher::Full(REQUEST_DURATION_METRIC.to_owned()),
                REQUEST_DURATION_BUCKETS,
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
            })
            .map(|line| {
                let (_, count) = line.rsplit_once(' ').expect("histogram sample value");
                count.parse::<usize>().expect("integer histogram count")
            })
            .sum()
    }

    pub(crate) fn limits() -> Limits {
        Limits::new(Duration::from_secs(1), 8, 128)
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
        let tls = tls_config_with_root(&material.root).expect("fixture root certificate");
        let origin = Origin::parse_https(&format!("https://{host}")).expect("fixture origin");
        Client::with_transport(
            crate::policy::TargetOrigin::Https(origin),
            limits,
            tls,
            Resolver::Fixture(resolver),
        )
    }

    pub(crate) fn default_context() -> OperationContext {
        OperationContext::with_timeout(Duration::from_secs(1))
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
        material.acceptor()
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
}
