//! Trusted provider URL admission and pooled HTTPS transport.

use std::{fmt, sync::Arc, time::Duration};

use reqwest::{header, redirect::Policy};
use tokio::time::Instant;
#[cfg(any(test, feature = "test-support"))]
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{Failure, PreparationError, PreparationPhase, PreparationReason};

const MAX_RESPONSE_BYTES: usize = 1_048_576;
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(3);

/// One adapter-owned admitted provider destination.
#[derive(Clone, Eq, PartialEq)]
pub struct ProviderUrl {
    exact: String,
    url: Url,
}

impl ProviderUrl {
    /// Parses a strict issuer URL without a query or fragment.
    ///
    /// # Errors
    ///
    /// Returns [`PreparationError`] when the URL violates the provider grammar.
    pub fn parse(raw: &str) -> Result<Self, PreparationError> {
        Self::parse_url(raw, false)
    }

    /// Parses a provider endpoint, preserving its query ordering and escaping.
    ///
    /// # Errors
    ///
    /// Returns [`PreparationError`] for a non-HTTPS URL, missing host, userinfo,
    /// fragment, whitespace or control characters.
    pub fn parse_endpoint(raw: &str) -> Result<Self, PreparationError> {
        Self::parse_url(raw, true)
    }

    fn parse_url(raw: &str, allow_query: bool) -> Result<Self, PreparationError> {
        if raw
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(PreparationError::new(
                PreparationPhase::Options,
                PreparationReason::InvalidUrl,
            ));
        }
        let url = Url::parse(raw).map_err(|_| {
            PreparationError::new(PreparationPhase::Options, PreparationReason::InvalidUrl)
        })?;
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
            return Err(PreparationError::new(
                PreparationPhase::Options,
                PreparationReason::InvalidUrl,
            ));
        }
        Ok(Self {
            exact: raw.to_owned(),
            url,
        })
    }

    /// The unmodified accepted spelling, used for exact issuer identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.exact
    }

    pub(crate) fn url(&self) -> &Url {
        &self.url
    }
}

impl fmt::Debug for ProviderUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderUrl([REDACTED])")
    }
}

/// An absolute bound covering one provider exchange, including its body.
#[derive(Clone, Copy)]
pub(crate) struct ProviderDeadline {
    deadline: Instant,
}

impl ProviderDeadline {
    pub(crate) fn independent(now: Instant) -> Self {
        Self {
            deadline: now + PROVIDER_TIMEOUT,
        }
    }

    // template:begin oidc-jwt:authn-provider-jwt-deadlines
    pub(crate) fn startup(now: Instant, overall_deadline: Instant) -> Option<Self> {
        let deadline = (now + PROVIDER_TIMEOUT).min(overall_deadline);
        (deadline > now).then_some(Self { deadline })
    }
    // template:end oidc-jwt:authn-provider-jwt-deadlines
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
    pub(crate) async fn get_json(
        &self,
        url: &Url,
        deadline: ProviderDeadline,
    ) -> Result<Vec<u8>, Failure> {
        self.exchange(self.client.get(url.clone()), deadline, false)
            .await
    }
    // template:end oidc-jwt:authn-provider-get-json

    // template:begin oidc-introspection:authn-provider-post-form-json
    pub(crate) async fn post_form_json(
        &self,
        url: &Url,
        basic_authorization: &[u8],
        form_body: &[u8],
        deadline: ProviderDeadline,
    ) -> Result<Vec<u8>, Failure> {
        let mut authorization = header::HeaderValue::from_bytes(basic_authorization)
            .map_err(|_| Failure::Unavailable)?;
        authorization.set_sensitive(true);
        self.exchange(
            self.client
                .post(url.clone())
                .header(header::AUTHORIZATION, authorization)
                .header(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("application/x-www-form-urlencoded"),
                )
                .body(form_body.to_vec()),
            deadline,
            true,
        )
        .await
    }
    // template:end oidc-introspection:authn-provider-post-form-json

    async fn exchange(
        &self,
        request: reqwest::RequestBuilder,
        deadline: ProviderDeadline,
        require_json_media_type: bool,
    ) -> Result<Vec<u8>, Failure> {
        if Instant::now() >= deadline.deadline {
            return Err(Failure::Unavailable);
        }
        let response = tokio::time::timeout_at(deadline.deadline, request.send())
            .await
            .map_err(|_| Failure::Unavailable)?
            .map_err(|_| Failure::Unavailable)?;
        if response.status() != reqwest::StatusCode::OK
            || (require_json_media_type && !is_json_response(&response))
        {
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
                if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
                    return Err(Failure::Unavailable);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        };
        tokio::time::timeout_at(deadline.deadline, read)
            .await
            .map_err(|_| Failure::Unavailable)?
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
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
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

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
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

    use super::{Failure, ProviderClient, ProviderDeadline, ProviderUrl, new_fixture_client};

    const FIXTURE_HOST: &str = "authn.fixture.test";

    #[test]
    fn provider_url_retains_exact_spelling_and_rejects_unsafe_destinations() {
        let url = ProviderUrl::parse("https://provider.example/path").unwrap();
        assert_eq!(url.as_str(), "https://provider.example/path");
        for invalid in [
            "http://provider.example",
            "https://user@provider.example",
            "https://provider.example?x=y",
            "https://provider.example#fragment",
            " https://provider.example",
            "https://provider.example\n",
        ] {
            assert!(ProviderUrl::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn endpoint_queries_are_preserved_and_provider_debug_is_redacted() {
        let raw = "https://provider.example/keys?token=private%2Fvalue&x=1&x=2";
        let endpoint = ProviderUrl::parse_endpoint(raw).unwrap();
        assert_eq!(endpoint.as_str(), raw);
        assert_eq!(endpoint.url().as_str(), raw);
        assert!(ProviderUrl::parse(raw).is_err());
        assert!(!format!("{endpoint:?}").contains("private"));
        for invalid in [
            "http://provider.example?token=private",
            "https://user@provider.example?token=private",
            "https://provider.example?token=private#fragment",
            "https://provider.example?token=private\n",
        ] {
            let error = ProviderUrl::parse_endpoint(invalid).unwrap_err();
            assert!(!format!("{error:?}").contains("private"));
            assert!(!error.to_string().contains("private"));
        }
    }

    async fn tls_server(
        response: Vec<u8>,
        hold_open: bool,
    ) -> (std::net::SocketAddr, Vec<u8>, JoinHandle<Vec<u8>>) {
        let material = crate::test_support::tls_material(FIXTURE_HOST);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(material.certificate_der)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(material.private_key_der)),
        )
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
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
        (address, material.root_der, server)
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
            let endpoint = ProviderUrl::parse_endpoint(&format!(
                "https://{FIXTURE_HOST}:{}/keys?token=private%2Fvalue&x=1&x=2",
                address.port(),
            ))
            .unwrap();
            let body = fixture_client(address, &root)
                .get_json(
                    endpoint.url(),
                    ProviderDeadline::independent(Instant::now()),
                )
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
                .get_json(
                    &fixture_url(address),
                    ProviderDeadline::independent(Instant::now())
                )
                .await,
            Err(Failure::Unavailable)
        );
        let _ = server.await;

        let response = b"HTTP/1.1 200 OK\r\ncontent-length: 11\r\n\r\n{".to_vec();
        let (address, root, server) = tls_server(response, true).await;
        assert_eq!(
            fixture_client(address, &root)
                .get_json(
                    &fixture_url(address),
                    ProviderDeadline::independent(Instant::now()),
                )
                .await,
            Err(Failure::Unavailable),
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
                b"Basic Zml4dHVyZQ==",
                b"token=opaque",
                ProviderDeadline::independent(Instant::now()),
            )
            .await
            .unwrap();
        assert_eq!(body, br#"{"active":false}"#);
        assert!(
            String::from_utf8(server.await.unwrap())
                .unwrap()
                .starts_with("POST /introspect HTTP/1.1\r\n")
        );

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
                        b"Basic Zml4dHVyZQ==",
                        b"token=opaque",
                        ProviderDeadline::independent(Instant::now()),
                    )
                    .await,
                Err(Failure::Unavailable),
            );
            server.await.unwrap();
        }
    }
    // template:end oidc-introspection:authn-provider-post-form-tls-test
}
