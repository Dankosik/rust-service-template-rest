//! Trusted provider URL admission and pooled HTTPS transport.

use std::{fmt, sync::Arc, time::Duration};

use reqwest::{header, redirect::Policy};
#[cfg(any(test, feature = "test-support"))]
use tokio_util::sync::CancellationToken;
use url::Url;

#[cfg(any(test, feature = "test-support"))]
use crate::Failure;
use crate::{PreparationError, PreparationPhase, PreparationReason};

const MAX_RESPONSE_BYTES: usize = 1_048_576;
/// Total budget of one provider exchange; reqwest applies it until the body ends.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(3);

/// Why a provider exchange produced no usable body. Closed, and free of
/// provider-supplied text, URLs and credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderFailure {
    /// The exchange exceeded its time budget.
    Timeout,
    /// DNS, TCP or TLS did not establish a connection.
    Connect,
    /// The provider answered with this status instead of 200.
    Status(u16),
    /// The response is not `application/json` where that is required.
    MediaType,
    /// The response exceeds the size ceiling.
    TooLarge,
    /// The request or the response body failed in transit.
    Transfer,
}

impl ProviderFailure {
    /// A timed-out connection attempt is both; the budget is the cause.
    fn from_transport(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout
        } else if error.is_connect() {
            Self::Connect
        } else {
            Self::Transfer
        }
    }
}

/// An exact issuer identity: an HTTPS URL without a query or fragment.
#[derive(Clone, Eq, PartialEq)]
pub struct IssuerUrl(EndpointUrl);

impl IssuerUrl {
    /// Parses a strict issuer URL without a query or fragment.
    ///
    /// # Errors
    ///
    /// Returns [`PreparationError`] when the URL violates the provider grammar.
    pub fn parse(raw: &str) -> Result<Self, PreparationError> {
        admit(raw, false).map(Self)
    }

    /// The unmodified accepted spelling, used for exact issuer identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    // template:begin oidc-jwt:authn-provider-issuer-url
    pub(crate) fn url(&self) -> &Url {
        self.0.url()
    }
    // template:end oidc-jwt:authn-provider-issuer-url
}

impl fmt::Debug for IssuerUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IssuerUrl([REDACTED])")
    }
}

/// One admitted provider endpoint whose query ordering and escaping are preserved.
#[derive(Clone, Eq, PartialEq)]
pub struct EndpointUrl {
    exact: String,
    url: Url,
}

impl EndpointUrl {
    /// Parses a provider endpoint, preserving its query ordering and escaping.
    ///
    /// # Errors
    ///
    /// Returns [`PreparationError`] for a non-HTTPS URL, missing host, userinfo,
    /// fragment, whitespace or control characters.
    pub fn parse(raw: &str) -> Result<Self, PreparationError> {
        admit(raw, true)
    }

    /// The unmodified accepted spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.exact
    }

    pub(crate) fn url(&self) -> &Url {
        &self.url
    }
}

impl fmt::Debug for EndpointUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EndpointUrl([REDACTED])")
    }
}

/// `Url::parse` silently drops whitespace and an empty userinfo, so the raw
/// spelling is checked too.
fn admit(raw: &str, allow_query: bool) -> Result<EndpointUrl, PreparationError> {
    let invalid =
        || PreparationError::new(PreparationPhase::Options, PreparationReason::InvalidUrl);
    if raw
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(invalid());
    }
    let url = Url::parse(raw).map_err(|_| invalid())?;
    let authority = raw
        .split_once("://")
        .map(|(_, value)| value.split(['/', '?', '#']).next().unwrap_or_default())
        .unwrap_or_default();
    if url.scheme() != "https"
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (!allow_query && url.query().is_some())
        || authority.contains('@')
    {
        return Err(invalid());
    }
    Ok(EndpointUrl {
        exact: raw.to_owned(),
        url,
    })
}

/// The only outbound client authentication engines may use.
#[derive(Clone)]
pub(crate) struct ProviderClient {
    client: reqwest::Client,
}

impl ProviderClient {
    pub(crate) fn new() -> Result<Self, PreparationError> {
        build_client(None, None)
            .map(|client| Self { client })
            .map_err(|()| {
                PreparationError::new(PreparationPhase::Client, PreparationReason::Client)
            })
    }

    // template:begin oidc-jwt:authn-provider-get-json
    pub(crate) async fn get_json(&self, url: &Url) -> Result<Vec<u8>, ProviderFailure> {
        self.exchange(self.client.get(url.clone()), false).await
    }
    // template:end oidc-jwt:authn-provider-get-json

    // template:begin oidc-introspection:authn-provider-post-form-json
    /// Posts a form with client-secret Basic authentication. The caller supplies
    /// credential components already form-encoded as RFC 6749 section 2.3.1 requires.
    pub(crate) async fn post_form_json(
        &self,
        url: &Url,
        client_id: &str,
        client_secret: &str,
        form_body: String,
    ) -> Result<Vec<u8>, ProviderFailure> {
        let request = self
            .client
            .post(url.clone())
            .basic_auth(client_id, Some(client_secret))
            .header(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/x-www-form-urlencoded"),
            )
            .body(form_body);
        self.exchange(request, true).await
    }
    // template:end oidc-introspection:authn-provider-post-form-json

    async fn exchange(
        &self,
        request: reqwest::RequestBuilder,
        require_json_media_type: bool,
    ) -> Result<Vec<u8>, ProviderFailure> {
        let transport = |error| ProviderFailure::from_transport(&error);
        let mut response = request.send().await.map_err(transport)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(ProviderFailure::Status(response.status().as_u16()));
        }
        if require_json_media_type && !is_json_response(&response) {
            return Err(ProviderFailure::MediaType);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(ProviderFailure::TooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport)? {
            if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
                return Err(ProviderFailure::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

fn build_client(
    root: Option<reqwest::Certificate>,
    resolver: Option<Arc<dyn reqwest::dns::Resolve>>,
) -> Result<reqwest::Client, ()> {
    let builder = reqwest::Client::builder()
        .tls_backend_rustls()
        .https_only(true)
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .referer(false)
        .timeout(PROVIDER_TIMEOUT);
    let builder = if let Some(resolver) = resolver {
        builder.dns_resolver(resolver)
    } else {
        builder
    };
    let builder = if let Some(root) = root {
        builder.tls_certs_only([root])
    } else {
        builder
    };
    builder.build().map_err(|_| ())
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn new_fixture_client(
    fixture_host: &str,
    fixture_addr: std::net::SocketAddr,
    fixture_root_der: &[u8],
    cancel: CancellationToken,
) -> Result<ProviderClient, Failure> {
    if fixture_host.is_empty() || !fixture_addr.ip().is_loopback() {
        return Err(Failure::Unavailable);
    }
    let root =
        reqwest::Certificate::from_der(fixture_root_der).map_err(|_| Failure::Unavailable)?;
    let resolver = Arc::new(FixtureResolver {
        host: fixture_host.to_owned(),
        address: fixture_addr,
        cancel,
    });
    build_client(Some(root), Some(resolver))
        .map(|client| ProviderClient { client })
        .map_err(|()| Failure::Unavailable)
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
struct FixtureResolver {
    host: String,
    address: std::net::SocketAddr,
    cancel: CancellationToken,
}

#[cfg(any(test, feature = "test-support"))]
impl reqwest::dns::Resolve for FixtureResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = self.host.clone();
        let address = self.address;
        let cancel = self.cancel.clone();
        Box::pin(async move {
            if cancel.is_cancelled() || !name.as_str().eq_ignore_ascii_case(&host) {
                return Err(std::io::Error::other("fixture DNS denied").into());
            }
            Ok(Box::new(std::iter::once(address)) as reqwest::dns::Addrs)
        })
    }
}

fn is_json_response(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|type_| type_.trim().eq_ignore_ascii_case("application/json"))
        })
}

/// A TLS acceptor for `host` and the root certificate that trusts it.
#[cfg(test)]
pub(crate) fn fixture_acceptor(host: &str) -> (tokio_rustls::TlsAcceptor, Vec<u8>) {
    use tokio_rustls::rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    };

    let material = crate::tls::TlsMaterial::new(host);
    let config = ServerConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from(material.cert)],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(material.key)),
    )
    .unwrap();
    (
        tokio_rustls::TlsAcceptor::from(Arc::new(config)),
        material.root,
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };
    use tokio_util::sync::CancellationToken;
    use url::Url;

    use super::{
        EndpointUrl, IssuerUrl, ProviderClient, ProviderFailure, fixture_acceptor,
        new_fixture_client,
    };

    const FIXTURE_HOST: &str = "authn.fixture.test";

    #[test]
    fn provider_url_retains_exact_spelling_and_rejects_unsafe_destinations() {
        let url = IssuerUrl::parse("https://provider.example/path").unwrap();
        assert_eq!(url.as_str(), "https://provider.example/path");
        for invalid in [
            "http://provider.example",
            "https://user@provider.example",
            "https://provider.example?x=y",
            "https://provider.example#fragment",
            " https://provider.example",
            "https://provider.example\n",
        ] {
            assert!(IssuerUrl::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn endpoint_queries_are_preserved_and_provider_debug_is_redacted() {
        let raw = "https://provider.example/keys?token=private%2Fvalue&x=1&x=2";
        let endpoint = EndpointUrl::parse(raw).unwrap();
        assert_eq!(endpoint.as_str(), raw);
        assert_eq!(endpoint.url().as_str(), raw);
        assert!(IssuerUrl::parse(raw).is_err());
        assert!(!format!("{endpoint:?}").contains("private"));
        for invalid in [
            "http://provider.example?token=private",
            "https://user@provider.example?token=private",
            "https://provider.example?token=private#fragment",
            "https://provider.example?token=private\n",
        ] {
            let error = EndpointUrl::parse(invalid).unwrap_err();
            assert!(!format!("{error:?}").contains("private"));
            assert!(!error.to_string().contains("private"));
        }
    }

    async fn tls_server(
        response: Vec<u8>,
        hold_open: bool,
    ) -> (std::net::SocketAddr, Vec<u8>, JoinHandle<Vec<u8>>) {
        let (acceptor, root) = fixture_acceptor(FIXTURE_HOST);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = acceptor.accept(stream).await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 4096];
                loop {
                    let read = stream.read(&mut chunk).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|part| part == b"\r\n\r\n") {
                        break;
                    }
                }
                if !response.is_empty() {
                    stream.write_all(&response).await.unwrap();
                    stream.flush().await.unwrap();
                }
                if hold_open {
                    let _ = stream.read(&mut chunk).await;
                }
                request
            })
            .await
            .unwrap()
        });
        (address, root, server)
    }

    fn fixture_client(address: std::net::SocketAddr, root: &[u8]) -> ProviderClient {
        new_fixture_client(FIXTURE_HOST, address, root, CancellationToken::new()).unwrap()
    }

    fn fixture_url(address: std::net::SocketAddr) -> Url {
        Url::parse(&format!(
            "https://{FIXTURE_HOST}:{}/introspect",
            address.port()
        ))
        .unwrap()
    }

    // template:begin oidc-jwt:authn-provider-tls-bounds-test
    #[tokio::test]
    async fn jwt_fixture_transport_uses_trusted_private_tls_and_enforces_body_and_time_bounds() {
        for content_type in [
            "",
            "content-type: text/plain\r\n",
            "content-type: application/json\r\n",
        ] {
            let response = format!(
                "HTTP/1.1 200 OK\r\n{content_type}content-length: 11\r\n\r\n{{\"keys\":[]}}"
            )
            .into_bytes();
            let (address, root, server) = tls_server(response, false).await;
            let endpoint = EndpointUrl::parse(&format!(
                "https://{FIXTURE_HOST}:{}/keys?token=private%2Fvalue&x=1&x=2",
                address.port(),
            ))
            .unwrap();
            let body = fixture_client(address, &root)
                .get_json(endpoint.url())
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                serde_json::json!({"keys":[]})
            );
            assert!(
                String::from_utf8(server.await.unwrap())
                    .unwrap()
                    .starts_with("GET /keys?token=private%2Fvalue&x=1&x=2 HTTP/1.1\r\n")
            );
        }

        let mut oversized = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n100000\r\n".to_vec();
        oversized.extend(std::iter::repeat_n(b'x', 1_048_576));
        oversized.extend_from_slice(b"\r\n1\r\ny\r\n0\r\n\r\n");
        let (address, root, server) = tls_server(oversized, false).await;
        assert_eq!(
            fixture_client(address, &root)
                .get_json(&fixture_url(address))
                .await,
            Err(ProviderFailure::TooLarge)
        );
        let _ = server.await;

        let response = b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n".to_vec();
        let (address, root, server) = tls_server(response, false).await;
        assert_eq!(
            fixture_client(address, &root)
                .get_json(&fixture_url(address))
                .await,
            Err(ProviderFailure::Status(404))
        );
        server.await.unwrap();

        let response = b"HTTP/1.1 200 OK\r\ncontent-length: 11\r\n\r\n{".to_vec();
        let (address, root, server) = tls_server(response, true).await;
        assert_eq!(
            fixture_client(address, &root)
                .get_json(&fixture_url(address))
                .await,
            Err(ProviderFailure::Timeout),
        );
        server.await.unwrap();
    }
    // template:end oidc-jwt:authn-provider-tls-bounds-test

    // template:begin oidc-introspection:authn-provider-post-form-tls-test
    #[tokio::test]
    async fn introspection_fixture_transport_uses_trusted_private_tls_for_form_posts() {
        let response = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 16\r\n\r\n{\"active\":false}".to_vec();
        let (address, root, server) = tls_server(response, false).await;
        let body = fixture_client(address, &root)
            .post_form_json(
                &fixture_url(address),
                "a%3Ab",
                "c+d",
                "token=opaque".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(body, br#"{"active":false}"#);
        let request = String::from_utf8(server.await.unwrap()).unwrap();
        assert!(request.starts_with("POST /introspect HTTP/1.1\r\n"));
        // reqwest's Basic scheme carries the pre-encoded components verbatim.
        assert!(request.contains("authorization: Basic YSUzQWI6Yytk\r\n"));

        for content_type in ["", "content-type: text/plain\r\n"] {
            let response = format!(
                "HTTP/1.1 200 OK\r\n{content_type}content-length: 16\r\n\r\n{{\"active\":false}}"
            )
            .into_bytes();
            let (address, root, server) = tls_server(response, false).await;
            assert_eq!(
                fixture_client(address, &root)
                    .post_form_json(
                        &fixture_url(address),
                        "fixture",
                        "secret",
                        "token=opaque".to_owned(),
                    )
                    .await,
                Err(ProviderFailure::MediaType),
            );
            server.await.unwrap();
        }
    }
    // template:end oidc-introspection:authn-provider-post-form-tls-test
}
