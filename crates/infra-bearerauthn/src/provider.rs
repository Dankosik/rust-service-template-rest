//! Fixed private transport for authentication providers.
//!
//! This is deliberately not a general outbound client.  It owns the small
//! policy surface shared by discovery, JWKS, and introspection exchanges.

mod dns;

use std::{sync::Arc, time::Duration};

use reqwest::{header, redirect::Policy};
// template:begin oidc-introspection:authn-provider-form-header-import
use reqwest::header::HeaderValue;
// template:end oidc-introspection:authn-provider-form-header-import
use tokio::time::Instant;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use url::{Host, Url};

use crate::Failure;

const MAX_RESPONSE_BYTES: usize = 1_048_576;
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(3);
const RESPONSE_RESERVE: Duration = Duration::from_millis(100);

pub(crate) fn parse_provider_url(raw: &str) -> Result<Url, Failure> {
    if raw != raw.trim() || raw.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(Failure::Unavailable);
    }
    let url = Url::parse(raw).map_err(|_| Failure::Unavailable)?;
    let authority = raw
        .split_once("://")
        .map(|(_, value)| value.split(['/', '?', '#']).next().unwrap_or_default())
        .unwrap_or_default();
    if url.scheme() != "https"
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
        || authority.contains('@')
    {
        return Err(Failure::Unavailable);
    }
    Ok(url)
}

pub(crate) fn reserve_request_deadline(now: Instant, request_deadline: Instant) -> Option<Instant> {
    let reserved_request_deadline = request_deadline.checked_sub(RESPONSE_RESERVE)?;
    let provider_deadline = (now + PROVIDER_TIMEOUT).min(reserved_request_deadline);
    (provider_deadline > now).then_some(provider_deadline)
}

/// The only outbound client authentication engines may use.
#[derive(Clone)]
pub(crate) struct ProviderClient {
    client: reqwest::Client,
}

impl ProviderClient {
    pub(crate) fn new(tracker: TaskTracker, cancel: CancellationToken) -> Result<Self, Failure> {
        let resolver = dns::Resolver::new(tracker, cancel)?;
        let client = build_client(resolver, None)?;

        Ok(Self { client })
    }

    // template:begin oidc-jwt:authn-provider-get-json
    pub(crate) async fn get_json(&self, url: &Url, deadline: Instant) -> Result<Vec<u8>, Failure> {
        admit_url(url)?;
        self.exchange(self.client.get(url.clone()), deadline).await
    }
    // template:end oidc-jwt:authn-provider-get-json

    // template:begin oidc-introspection:authn-provider-post-form-json
    pub(crate) async fn post_form_json(
        &self,
        url: &Url,
        basic_authorization: &[u8],
        form_body: &[u8],
        deadline: Instant,
    ) -> Result<Vec<u8>, Failure> {
        admit_url(url)?;
        let authorization =
            HeaderValue::from_bytes(basic_authorization).map_err(|_| Failure::Unavailable)?;
        self.exchange(
            self.client
                .post(url.clone())
                .header(header::AUTHORIZATION, authorization)
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                )
                .body(form_body.to_vec()),
            deadline,
        )
        .await
    }
    // template:end oidc-introspection:authn-provider-post-form-json

    // template:begin oidc-jwt:authn-provider-raw-dns-test-constructor
    #[cfg(test)]
    fn with_raw_dns_answers(host: &str, answers: Vec<std::net::IpAddr>) -> Result<Self, Failure> {
        Ok(Self {
            client: build_client(dns::RawAnswerResolver::new(host, answers), None)?,
        })
    }
    // template:end oidc-jwt:authn-provider-raw-dns-test-constructor

    async fn exchange(
        &self,
        request: reqwest::RequestBuilder,
        deadline: Instant,
    ) -> Result<Vec<u8>, Failure> {
        // `timeout_at` may poll the request before observing an already-ready
        // timer. Refuse an expired reservation before request polling so a
        // scheduling delay cannot start provider I/O after its custody ends.
        if Instant::now() >= deadline {
            return Err(Failure::Unavailable);
        }
        let response = tokio::time::timeout_at(deadline, request.send())
            .await
            .map_err(|_| Failure::Unavailable)?
            .map_err(|_| Failure::Unavailable)?;

        if response.status() != reqwest::StatusCode::OK || !is_json_response(&response) {
            return Err(Failure::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(Failure::Unavailable);
        }

        let read = async move {
            let mut response = response;
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| Failure::Unavailable)? {
                let remaining = MAX_RESPONSE_BYTES.saturating_sub(body.len());
                if chunk.len() > remaining {
                    return Err(Failure::Unavailable);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        };
        tokio::time::timeout_at(deadline, read)
            .await
            .map_err(|_| Failure::Unavailable)?
    }
}

fn build_client<R>(
    resolver: R,
    root: Option<reqwest::Certificate>,
) -> Result<reqwest::Client, Failure>
where
    R: reqwest::dns::Resolve + 'static,
{
    let builder = reqwest::Client::builder()
        .tls_backend_rustls()
        .https_only(true)
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .referer(false)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .http1_only()
        .pool_max_idle_per_host(0)
        .http1_max_headers(100)
        .timeout(PROVIDER_TIMEOUT)
        .dns_resolver(Arc::new(resolver));
    let builder = match root {
        Some(root) => builder.add_root_certificate(root),
        None => builder,
    };
    builder.build().map_err(|_| Failure::Unavailable)
}

/// A fixture-only constructor for real TLS tests.  It has one hostname-to-
/// loopback mapping and retains the hostname for certificate/SNI verification.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn new_fixture_client(
    tracker: TaskTracker,
    cancel: CancellationToken,
    fixture_host: &str,
    fixture_addr: std::net::SocketAddr,
    fixture_root_der: &[u8],
) -> Result<ProviderClient, Failure> {
    if fixture_host.is_empty() || !fixture_addr.ip().is_loopback() {
        return Err(Failure::Unavailable);
    }
    let root =
        reqwest::Certificate::from_der(fixture_root_der).map_err(|_| Failure::Unavailable)?;
    let resolver = FixtureResolver {
        tracker,
        cancel,
        host: fixture_host.to_owned(),
        address: fixture_addr,
    };
    Ok(ProviderClient {
        client: build_client(resolver, Some(root))?,
    })
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
struct FixtureResolver {
    tracker: TaskTracker,
    cancel: CancellationToken,
    host: String,
    address: std::net::SocketAddr,
}

#[cfg(any(test, feature = "test-support"))]
impl reqwest::dns::Resolve for FixtureResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = self.host.clone();
        let address = self.address;
        let tracker = self.tracker.clone();
        let cancel = self.cancel.clone();
        Box::pin(async move {
            if cancel.is_cancelled() || !name.as_str().eq_ignore_ascii_case(&host) {
                return Err(std::io::Error::other("fixture DNS denied").into());
            }
            let _scope = tracker.token();
            Ok(Box::new(std::iter::once(address)) as reqwest::dns::Addrs)
        })
    }
}

fn admit_url(url: &Url) -> Result<(), Failure> {
    if url.scheme() != "https" {
        return Err(Failure::Unavailable);
    }
    match url.host() {
        Some(Host::Domain(_)) => Ok(()),
        Some(Host::Ipv4(address)) => dns::admit_address(address.into()),
        Some(Host::Ipv6(address)) => dns::admit_address(address.into()),
        None => Err(Failure::Unavailable),
    }
}

fn is_json_response(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_json_media_type)
}

fn is_json_media_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};
    // template:begin oidc-jwt:authn-provider-raw-dns-test-imports
    use std::net::{IpAddr, Ipv4Addr};
    // template:end oidc-jwt:authn-provider-raw-dns-test-imports

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
        time::Instant,
    };
    // template:begin oidc-jwt:authn-provider-cancellation-server-import
    use tokio::sync::oneshot;
    // template:end oidc-jwt:authn-provider-cancellation-server-import
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{
            ServerConfig,
            pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        },
    };
    use url::Url;

    use super::{
        Failure, PROVIDER_TIMEOUT, ProviderClient, is_json_media_type, new_fixture_client,
    };
    use tokio_util::{sync::CancellationToken, task::TaskTracker};

    const FIXTURE_HOST: &str = "authn.fixture.test";
    const CERT_DER: &[u8] = include_bytes!("../tests/fixtures/authn-fixture-cert.der");
    const KEY_DER: &[u8] = include_bytes!("../tests/fixtures/authn-fixture-key.der");
    const ROOT_DER: &[u8] = include_bytes!("../tests/fixtures/authn-fixture-root.der");
    // template:begin oidc-jwt:authn-provider-untrusted-root-fixture
    const UNTRUSTED_ROOT_DER: &[u8] =
        include_bytes!("../tests/fixtures/authn-fixture-untrusted-root.der");
    // template:end oidc-jwt:authn-provider-untrusted-root-fixture

    async fn tls_server(response: Vec<u8>) -> (std::net::SocketAddr, JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(CERT_DER.to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY_DER.to_vec())),
        )
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = acceptor
                .accept(stream)
                .await
                .expect("trusted fixture TLS handshake must succeed");
            let mut request = vec![0; 4096];
            let read = stream.read(&mut request).await.unwrap_or_default();
            request.truncate(read);
            if !response.is_empty() {
                stream.write_all(&response).await.unwrap();
                stream.flush().await.unwrap();
            }
            request
        });
        (address, server)
    }

    // template:begin oidc-jwt:authn-provider-redirect-server
    async fn redirect_server() -> (std::net::SocketAddr, JoinHandle<(Vec<u8>, bool)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(CERT_DER.to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY_DER.to_vec())),
        )
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let Ok(mut stream) = acceptor.accept(stream).await else {
                return (Vec::new(), false);
            };
            let mut request = vec![0; 4096];
            let read = stream.read(&mut request).await.unwrap_or_default();
            request.truncate(read);
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nlocation: /introspect\r\ncontent-length: 0\r\n\r\n",
                )
                .await
                .unwrap();
            stream.flush().await.unwrap();
            let repeated = tokio::time::timeout(Duration::from_millis(25), listener.accept())
                .await
                .is_ok();
            (request, repeated)
        });
        (address, server)
    }
    // template:end oidc-jwt:authn-provider-redirect-server

    // template:begin oidc-introspection:authn-provider-dropped-post-server
    async fn dropped_post_server() -> (std::net::SocketAddr, JoinHandle<(Vec<u8>, bool)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(CERT_DER.to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY_DER.to_vec())),
        )
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let Ok(mut stream) = acceptor.accept(stream).await else {
                return (Vec::new(), false);
            };
            let mut request = vec![0; 4096];
            let read = stream.read(&mut request).await.unwrap_or_default();
            request.truncate(read);
            drop(stream);
            let repeated = tokio::time::timeout(Duration::from_millis(25), listener.accept())
                .await
                .is_ok();
            (request, repeated)
        });
        (address, server)
    }
    // template:end oidc-introspection:authn-provider-dropped-post-server

    // template:begin oidc-jwt:authn-provider-cancellation-server
    async fn cancellation_server() -> (
        std::net::SocketAddr,
        oneshot::Receiver<()>,
        JoinHandle<bool>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(CERT_DER.to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY_DER.to_vec())),
        )
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let (received, observed_request) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let Ok(mut stream) = acceptor.accept(stream).await else {
                return false;
            };
            let mut request = vec![0; 4096];
            let read = stream.read(&mut request).await.unwrap_or_default();
            if read == 0 {
                return false;
            }
            let _ = received.send(());
            let mut after_request = [0_u8; 1];
            matches!(
                tokio::time::timeout(Duration::from_secs(1), stream.read(&mut after_request)).await,
                Ok(Ok(0))
            )
        });
        (address, observed_request, server)
    }
    // template:end oidc-jwt:authn-provider-cancellation-server

    fn fixture_client(address: std::net::SocketAddr, host: &str, root: &[u8]) -> ProviderClient {
        new_fixture_client(
            TaskTracker::new(),
            CancellationToken::new(),
            host,
            address,
            root,
        )
        .unwrap()
    }

    fn fixture_url(host: &str, address: std::net::SocketAddr) -> Url {
        Url::parse(&format!("https://{host}:{}/introspect", address.port())).unwrap()
    }

    #[test]
    fn accepts_json_media_type_with_case_and_parameters() {
        assert!(is_json_media_type("Application/JSON; charset=utf-8"));
    }

    #[test]
    fn rejects_non_json_media_type() {
        assert!(!is_json_media_type("text/plain"));
    }

    // template:begin oidc-introspection:authn-provider-post-form-tls-test
    #[tokio::test]
    async fn fixture_client_preserves_tls_name_root_and_form_post_controls() {
        let response = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 16\r\n\r\n{\"active\":false}".to_vec();
        let (address, server) = tls_server(response).await;
        let provider = fixture_client(address, FIXTURE_HOST, ROOT_DER);

        let body = provider
            .post_form_json(
                &fixture_url(FIXTURE_HOST, address),
                b"Basic Zml4dHVyZQ==",
                b"token=opaque&token_type_hint=access_token",
                Instant::now() + PROVIDER_TIMEOUT,
            )
            .await;
        let request = String::from_utf8(server.await.unwrap()).unwrap();
        let body = body.unwrap();

        assert_eq!(body, br#"{"active":false}"#);
        assert!(request.starts_with("POST /introspect HTTP/1.1\r\n"));
        assert!(request.contains("authorization: Basic Zml4dHVyZQ==\r\n"));
        assert!(request.contains("token=opaque&token_type_hint=access_token"));
    }

    // template:end oidc-introspection:authn-provider-post-form-tls-test

    // template:begin oidc-jwt:authn-provider-tls-name-root-test
    #[tokio::test]
    async fn fixture_client_rejects_wrong_name_and_untrusted_root() {
        let response =
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}"
                .to_vec();
        let (wrong_name_address, wrong_name_server) = tls_server(response.clone()).await;
        let wrong_name = fixture_client(wrong_name_address, "wrong.fixture.test", ROOT_DER);
        assert_eq!(
            wrong_name
                .get_json(
                    &fixture_url("wrong.fixture.test", wrong_name_address),
                    Instant::now() + PROVIDER_TIMEOUT,
                )
                .await,
            Err(Failure::Unavailable)
        );
        let _ = wrong_name_server.await;

        let (untrusted_address, untrusted_server) = tls_server(response).await;
        let untrusted = fixture_client(untrusted_address, FIXTURE_HOST, UNTRUSTED_ROOT_DER);
        assert_eq!(
            untrusted
                .get_json(
                    &fixture_url(FIXTURE_HOST, untrusted_address),
                    Instant::now() + PROVIDER_TIMEOUT,
                )
                .await,
            Err(Failure::Unavailable)
        );
        let _ = untrusted_server.await;
    }
    // template:end oidc-jwt:authn-provider-tls-name-root-test

    // template:begin oidc-jwt:authn-provider-length-deadline-test
    #[tokio::test]
    async fn fixture_client_denies_oversized_response_and_deadline() {
        let oversized =
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 1048577\r\n\r\n"
                .to_vec();
        let (oversized_address, oversized_server) = tls_server(oversized).await;
        let provider = fixture_client(oversized_address, FIXTURE_HOST, ROOT_DER);
        assert_eq!(
            provider
                .get_json(
                    &fixture_url(FIXTURE_HOST, oversized_address),
                    Instant::now() + PROVIDER_TIMEOUT,
                )
                .await,
            Err(Failure::Unavailable)
        );
        let _ = oversized_server.await;

        let (slow_address, slow_server) = tls_server(Vec::new()).await;
        let slow = fixture_client(slow_address, FIXTURE_HOST, ROOT_DER);
        assert_eq!(
            slow.get_json(
                &fixture_url(FIXTURE_HOST, slow_address),
                Instant::now() + Duration::from_millis(1),
            )
            .await,
            Err(Failure::Unavailable)
        );
        let _ = slow_server.await;
    }
    // template:end oidc-jwt:authn-provider-length-deadline-test

    // template:begin oidc-jwt:authn-provider-streaming-bound-test
    #[tokio::test]
    async fn fixture_client_enforces_streaming_body_bound_without_content_length() {
        let mut response = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n100000\r\n".to_vec();
        response.extend(std::iter::repeat_n(b'x', 1_048_576));
        response.extend_from_slice(b"\r\n1\r\ny\r\n0\r\n\r\n");
        let (address, server) = tls_server(response).await;
        let provider = fixture_client(address, FIXTURE_HOST, ROOT_DER);

        assert_eq!(
            provider
                .get_json(
                    &fixture_url(FIXTURE_HOST, address),
                    Instant::now() + PROVIDER_TIMEOUT,
                )
                .await,
            Err(Failure::Unavailable)
        );
        let _ = server.await;
    }
    // template:end oidc-jwt:authn-provider-streaming-bound-test

    // template:begin oidc-introspection:authn-provider-dropped-post-test
    #[tokio::test]
    async fn dropped_post_does_not_repeat_the_exchange() {
        let (address, server) = dropped_post_server().await;
        let provider = fixture_client(address, FIXTURE_HOST, ROOT_DER);
        assert_eq!(
            provider
                .post_form_json(
                    &fixture_url(FIXTURE_HOST, address),
                    b"Basic Zml4dHVyZQ==",
                    b"token=opaque&token_type_hint=access_token",
                    Instant::now() + PROVIDER_TIMEOUT,
                )
                .await,
            Err(Failure::Unavailable)
        );
        let (request, repeated) = server.await.unwrap();
        assert!(request.starts_with(b"POST /introspect HTTP/1.1\r\n"));
        assert!(!repeated, "dropped POST must not be retried");
    }
    // template:end oidc-introspection:authn-provider-dropped-post-test

    // template:begin oidc-jwt:authn-provider-drop-request-test
    #[tokio::test]
    async fn dropping_request_future_closes_the_actual_provider_connection() {
        let (address, received, server) = cancellation_server().await;
        let provider = fixture_client(address, FIXTURE_HOST, ROOT_DER);
        let request = tokio::spawn(async move {
            provider
                .get_json(
                    &fixture_url(FIXTURE_HOST, address),
                    Instant::now() + PROVIDER_TIMEOUT,
                )
                .await
        });
        received
            .await
            .expect("fixture receives request before cancellation");
        request.abort();
        let _ = request.await;
        assert!(
            server.await.unwrap(),
            "dropped request must close its connection"
        );
    }
    // template:end oidc-jwt:authn-provider-drop-request-test

    // template:begin oidc-jwt:authn-provider-expired-deadline-test
    #[tokio::test]
    async fn expired_reservation_refuses_before_request_polling() {
        let provider = ProviderClient {
            client: reqwest::Client::builder().build().unwrap(),
        };
        assert_eq!(
            provider
                .get_json(
                    &Url::parse("https://provider.example/never-contacted").unwrap(),
                    Instant::now() - Duration::from_millis(1),
                )
                .await,
            Err(Failure::Unavailable)
        );
    }
    // template:end oidc-jwt:authn-provider-expired-deadline-test

    // template:begin oidc-jwt:authn-provider-literal-redirect-test
    #[tokio::test]
    async fn production_literal_loopback_and_redirect_never_reach_or_repeat_a_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let production = ProviderClient::new(TaskTracker::new(), CancellationToken::new()).unwrap();
        assert_eq!(
            production
                .get_json(
                    &Url::parse(&format!("https://127.0.0.1:{}/denied", address.port())).unwrap(),
                    Instant::now() + PROVIDER_TIMEOUT,
                )
                .await,
            Err(Failure::Unavailable)
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), listener.accept())
                .await
                .is_err()
        );

        let (redirect_address, redirect_server) = redirect_server().await;
        let fixture = fixture_client(redirect_address, FIXTURE_HOST, ROOT_DER);
        assert_eq!(
            fixture
                .get_json(
                    &fixture_url(FIXTURE_HOST, redirect_address),
                    Instant::now() + PROVIDER_TIMEOUT,
                )
                .await,
            Err(Failure::Unavailable)
        );
        let (request, repeated) = redirect_server.await.unwrap();
        assert!(request.starts_with("GET /introspect HTTP/1.1\r\n".as_bytes()));
        assert!(!repeated);
    }
    // template:end oidc-jwt:authn-provider-literal-redirect-test

    // template:begin oidc-jwt:authn-provider-raw-dns-test
    #[tokio::test]
    async fn private_mapped_and_mixed_dns_answers_never_reach_a_listener() {
        let private_answers = [
            vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            vec!["::ffff:127.0.0.1".parse().expect("mapped test address")],
            vec![
                IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
            ],
        ];
        for answers in private_answers {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let provider = ProviderClient::with_raw_dns_answers(FIXTURE_HOST, answers).unwrap();
            assert_eq!(
                provider
                    .get_json(
                        &fixture_url(FIXTURE_HOST, address),
                        Instant::now() + PROVIDER_TIMEOUT,
                    )
                    .await,
                Err(Failure::Unavailable)
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(25), listener.accept())
                    .await
                    .is_err(),
                "denied DNS answers must not reach the listener"
            );
        }
    }
    // template:end oidc-jwt:authn-provider-raw-dns-test
}
