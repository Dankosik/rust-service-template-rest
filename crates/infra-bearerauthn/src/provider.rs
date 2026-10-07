//! Trusted provider URL admission and pooled HTTPS transport.

use std::{
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use reqwest::{header, redirect::Policy};
#[cfg(
    // template:begin oidc-introspection:authn-provider-fixture-cancel-feature
    any(feature = "test-support",
    // template:end oidc-introspection:authn-provider-fixture-cancel-feature
    test
    // template:begin oidc-introspection:authn-provider-fixture-cancel-cfg-close
    )
    // template:end oidc-introspection:authn-provider-fixture-cancel-cfg-close
)]
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;
use url::Url;

#[cfg(
    // template:begin oidc-introspection:authn-provider-fixture-failure-feature
    any(feature = "test-support",
    // template:end oidc-introspection:authn-provider-fixture-failure-feature
    test
    // template:begin oidc-introspection:authn-provider-fixture-failure-cfg-close
    )
    // template:end oidc-introspection:authn-provider-fixture-failure-cfg-close
)]
use crate::Failure;
use crate::{PreparationError, PreparationPhase, PreparationReason};

const MAX_RESPONSE_BYTES: usize = 1_048_576;
/// Total budget of one provider exchange; reqwest applies it until the body ends.
pub(crate) const PROVIDER_TIMEOUT: Duration = Duration::from_secs(3);
/// How long one provider exchange took, by closed `operation` and `outcome`.
const REQUEST_DURATION_METRIC: &str = "authn_provider_request_duration_seconds";

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

    /// The closed metric, span and log label of this failure class.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Timeout => "provider_timeout",
            Self::Connect => "provider_connect",
            Self::Status(400..=499) => "provider_status_4xx",
            Self::Status(500..=599) => "provider_status_5xx",
            Self::Status(_) => "provider_status_other",
            Self::MediaType => "provider_media_type",
            Self::TooLarge => "provider_too_large",
            Self::Transfer => "provider_transfer",
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
    pub(crate) async fn get_json(
        &self,
        url: &Url,
        document: Document,
    ) -> Result<Vec<u8>, ProviderFailure> {
        Exchange::start(document.operation(), "GET", url)
            .run(self.client.get(url.clone()), false)
            .await
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
        context: &operation_context::OperationContext,
    ) -> Result<Vec<u8>, ProviderFailure> {
        context.check().map_err(|_| ProviderFailure::Timeout)?;
        let request = self
            .client
            .post(url.clone())
            .basic_auth(client_id, Some(client_secret))
            .header(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/x-www-form-urlencoded"),
            )
            .body(form_body);
        context.check().map_err(|_| ProviderFailure::Timeout)?;
        let request = request.timeout(context.remaining().unwrap_or(PROVIDER_TIMEOUT));
        let exchange = Exchange::start("introspection", "POST", url).run(request, true);
        let result = tokio::select! {
            biased;
            _ = context.wait_stopped() => return Err(ProviderFailure::Timeout),
            result = exchange => result,
        };
        context.check().map_err(|_| ProviderFailure::Timeout)?;
        result
    }
    // template:end oidc-introspection:authn-provider-post-form-json
}

// template:begin oidc-jwt:authn-provider-document
/// The provider document a GET requests.
#[derive(Clone, Copy)]
pub(crate) enum Document {
    Discovery,
    Jwks,
}

impl Document {
    const fn operation(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Jwks => "jwks",
        }
    }
}
// template:end oidc-jwt:authn-provider-document

/// One provider exchange: its client span and its duration sample. Only the
/// configured host and port are recorded, never a path, query or credential.
/// Dropped unfinished, it records `cancelled`.
struct Exchange {
    operation: &'static str,
    started: Instant,
    span: tracing::Span,
    finished: bool,
}

impl Exchange {
    fn start(operation: &'static str, method: &'static str, url: &Url) -> Self {
        let span = tracing::info_span!(
            "authn_provider",
            otel.name = method,
            otel.kind = "client",
            http.request.method = method,
            server.address = url.host_str().unwrap_or_default(),
            server.port = url.port_or_known_default().unwrap_or_default(),
            authn.operation = operation,
            http.response.status_code = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        Self {
            operation,
            started: Instant::now(),
            span,
            finished: false,
        }
    }

    /// Sends the request inside the span and records how the exchange ended.
    async fn run(
        self,
        request: reqwest::RequestBuilder,
        require_json_media_type: bool,
    ) -> Result<Vec<u8>, ProviderFailure> {
        let result = receive(request, require_json_media_type)
            .instrument(self.span.clone())
            .await;
        self.finish(result.as_ref().err().copied());
        result
    }

    fn finish(mut self, failure: Option<ProviderFailure>) {
        self.finished = true;
        let status = match failure {
            None => Some(reqwest::StatusCode::OK.as_u16()),
            Some(ProviderFailure::Status(status)) => Some(status),
            Some(_) => None,
        };
        self.span.record("http.response.status_code", status);
        self.record(
            failure.map_or("success", ProviderFailure::label),
            failure.is_some(),
        );
    }

    fn record(&self, outcome: &'static str, failed: bool) {
        if failed {
            self.span.record("error.type", outcome);
            self.span.record("otel.status_code", "ERROR");
        }
        metrics::histogram!(
            REQUEST_DURATION_METRIC,
            "operation" => self.operation,
            "outcome" => outcome
        )
        .record(self.started.elapsed().as_secs_f64());
    }
}

impl Drop for Exchange {
    fn drop(&mut self) {
        if !self.finished {
            self.record("cancelled", true);
        }
    }
}

async fn receive(
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

fn build_client(
    root: Option<reqwest::Certificate>,
    resolver: Option<Arc<dyn reqwest::dns::Resolve>>,
) -> Result<reqwest::Client, ()> {
    metrics::describe_histogram!(
        REQUEST_DURATION_METRIC,
        metrics::Unit::Seconds,
        "Authentication provider exchange duration by operation and closed outcome"
    );
    let builder = reqwest::Client::builder()
        .tls_backend_rustls()
        .https_only(true)
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .referer(false)
        .connect_timeout(Duration::from_secs(2))
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

#[cfg(
    // template:begin oidc-introspection:authn-provider-fixture-client-feature
    any(feature = "test-support",
    // template:end oidc-introspection:authn-provider-fixture-client-feature
    test
    // template:begin oidc-introspection:authn-provider-fixture-client-cfg-close
    )
    // template:end oidc-introspection:authn-provider-fixture-client-cfg-close
)]
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

#[cfg(
    // template:begin oidc-introspection:authn-provider-fixture-resolver-feature
    any(feature = "test-support",
    // template:end oidc-introspection:authn-provider-fixture-resolver-feature
    test
    // template:begin oidc-introspection:authn-provider-fixture-resolver-cfg-close
    )
    // template:end oidc-introspection:authn-provider-fixture-resolver-cfg-close
)]
#[derive(Clone)]
struct FixtureResolver {
    host: String,
    address: std::net::SocketAddr,
    cancel: CancellationToken,
}

#[cfg(
    // template:begin oidc-introspection:authn-provider-fixture-resolve-feature
    any(feature = "test-support",
    // template:end oidc-introspection:authn-provider-fixture-resolve-feature
    test
    // template:begin oidc-introspection:authn-provider-fixture-resolve-cfg-close
    )
    // template:end oidc-introspection:authn-provider-fixture-resolve-cfg-close
)]
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
    use std::{
        net::SocketAddr,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };
    use tokio_util::sync::CancellationToken;
    use url::Url;

    // template:begin oidc-jwt:authn-provider-document-test-import
    use super::Document;
    // template:end oidc-jwt:authn-provider-document-test-import
    use super::{
        EndpointUrl, Exchange, IssuerUrl, ProviderClient, ProviderFailure, build_client,
        fixture_acceptor, new_fixture_client,
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
                respond(stream, acceptor, response, hold_open).await
            })
            .await
            .unwrap()
        });
        (address, root, server)
    }

    async fn respond(
        stream: tokio::net::TcpStream,
        acceptor: tokio_rustls::TlsAcceptor,
        response: Vec<u8>,
        hold_open: bool,
    ) -> Vec<u8> {
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

    struct RecoveringResolver {
        stall_once: AtomicBool,
        addresses: Vec<SocketAddr>,
    }

    impl reqwest::dns::Resolve for RecoveringResolver {
        fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            assert_eq!(name.as_str(), FIXTURE_HOST);
            let stall = self.stall_once.swap(false, Ordering::SeqCst);
            let addresses = self.addresses.clone();
            Box::pin(async move {
                if stall {
                    std::future::pending::<()>().await;
                }
                Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
            })
        }
    }

    struct RotatingResolver {
        calls: AtomicUsize,
        first: SocketAddr,
        replacement: SocketAddr,
    }

    impl reqwest::dns::Resolve for RotatingResolver {
        fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            assert_eq!(name.as_str(), FIXTURE_HOST);
            let address = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.first
            } else {
                self.replacement
            };
            Box::pin(async move { Ok(Box::new(std::iter::once(address)) as reqwest::dns::Addrs) })
        }
    }

    async fn respond_once(
        stream: tokio::net::TcpStream,
        acceptor: tokio_rustls::TlsAcceptor,
    ) -> Vec<u8> {
        respond(
            stream,
            acceptor,
            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}".to_vec(),
            false,
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_dns_expires_at_connect_budget_then_same_client_uses_later_candidate() {
        let response = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}".to_vec();
        let (address, root, server) = tls_server(response, false).await;
        // Reserve a refused endpoint without releasing its port to another test.
        let refused = tokio::net::TcpSocket::new_v4().unwrap();
        refused.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let resolver = Arc::new(RecoveringResolver {
            stall_once: AtomicBool::new(true),
            addresses: vec![refused.local_addr().unwrap(), address],
        });
        let client = ProviderClient {
            client: build_client(
                Some(reqwest::Certificate::from_der(&root).unwrap()),
                Some(resolver),
            )
            .unwrap(),
        };
        // No explicit URL port: retain each resolver candidate's fixture port.
        let url = Url::parse(&format!("https://{FIXTURE_HOST}/keys")).unwrap();
        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_millis(2500),
            Exchange::start("discovery", "GET", &url).run(client.client.get(url.clone()), false),
        )
        .await
        .expect("the connection budget must expire before the three-second total");
        assert_eq!(result, Err(ProviderFailure::Timeout));
        assert!(started.elapsed() >= Duration::from_secs(2));
        assert!(started.elapsed() < Duration::from_millis(2500));

        tokio::time::resume();
        let body = Exchange::start("discovery", "GET", &url)
            .run(client.client.get(url.clone()), false)
            .await
            .unwrap();
        assert_eq!(body, b"{}");
        assert!(server.await.unwrap().starts_with(b"GET /keys HTTP/1.1\r\n"));
    }

    #[tokio::test]
    async fn new_dial_observes_ip_and_trust_replacement_after_client_reconstruction() {
        let listener_a = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener_b = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address_a = listener_a.local_addr().unwrap();
        let address_b = listener_b.local_addr().unwrap();
        let (acceptor_a, root_a) = fixture_acceptor(FIXTURE_HOST);
        let (acceptor_b, root_b) = fixture_acceptor(FIXTURE_HOST);
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), async move {
                let (stream, _) = listener_a.accept().await.unwrap();
                let first = respond_once(stream, acceptor_a).await;

                let (stream, _) = listener_b.accept().await.unwrap();
                assert!(acceptor_b.clone().accept(stream).await.is_err());

                let (stream, _) = listener_b.accept().await.unwrap();
                let replacement = respond_once(stream, acceptor_b).await;
                (first, replacement)
            })
            .await
            .unwrap()
        });
        let resolver = Arc::new(RotatingResolver {
            calls: AtomicUsize::new(0),
            first: address_a,
            replacement: address_b,
        });
        let old_client = ProviderClient {
            client: build_client(
                Some(reqwest::Certificate::from_der(&root_a).unwrap()),
                Some(resolver.clone()),
            )
            .unwrap(),
        };
        let url = Url::parse(&format!("https://{FIXTURE_HOST}/keys")).unwrap();
        assert_eq!(
            Exchange::start("jwks", "GET", &url)
                .run(old_client.client.get(url.clone()), false)
                .await
                .unwrap(),
            b"{}"
        );
        assert_eq!(
            Exchange::start("jwks", "GET", &url)
                .run(old_client.client.get(url.clone()), false)
                .await,
            Err(ProviderFailure::Connect)
        );

        let rebuilt_client = ProviderClient {
            client: build_client(
                Some(reqwest::Certificate::from_der(&root_b).unwrap()),
                Some(resolver.clone()),
            )
            .unwrap(),
        };
        assert_eq!(
            Exchange::start("jwks", "GET", &url)
                .run(rebuilt_client.client.get(url.clone()), false)
                .await
                .unwrap(),
            b"{}"
        );
        let (first, replacement) = server.await.unwrap();
        assert!(first.starts_with(b"GET /keys HTTP/1.1\r\n"));
        assert!(replacement.starts_with(b"GET /keys HTTP/1.1\r\n"));
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 3);
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
                .get_json(endpoint.url(), Document::Jwks)
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
                .get_json(&fixture_url(address), Document::Jwks)
                .await,
            Err(ProviderFailure::TooLarge)
        );
        let _ = server.await;

        let response = b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n".to_vec();
        let (address, root, server) = tls_server(response, false).await;
        assert_eq!(
            fixture_client(address, &root)
                .get_json(&fixture_url(address), Document::Jwks)
                .await,
            Err(ProviderFailure::Status(404))
        );
        server.await.unwrap();

        let response = b"HTTP/1.1 200 OK\r\ncontent-length: 11\r\n\r\n{".to_vec();
        let (address, root, server) = tls_server(response, true).await;
        assert_eq!(
            fixture_client(address, &root)
                .get_json(&fixture_url(address), Document::Jwks)
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
                &operation_context::OperationContext::with_timeout(super::PROVIDER_TIMEOUT),
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
                        &operation_context::OperationContext::with_timeout(super::PROVIDER_TIMEOUT),
                    )
                    .await,
                Err(ProviderFailure::MediaType),
            );
            server.await.unwrap();
        }
    }
    // template:end oidc-introspection:authn-provider-post-form-tls-test

    #[tokio::test]
    async fn stalled_tls_expires_at_connect_budget_and_same_client_redials() {
        let (acceptor, root) = fixture_acceptor(FIXTURE_HOST);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (hello, received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), async move {
                let (mut stalled, _) = listener.accept().await.unwrap();
                let mut buffer = [0_u8; 4096];
                assert!(stalled.read(&mut buffer).await.unwrap() > 0);
                hello.send(()).unwrap();
                // Never answer the ClientHello; observe the client release this socket.
                while stalled.read(&mut buffer).await.unwrap() != 0 {}
                let (stream, _) = listener.accept().await.unwrap();
                respond(
                    stream,
                    acceptor,
                    b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}".to_vec(),
                    false,
                )
                .await
            })
            .await
            .unwrap()
        });
        let client = fixture_client(address, &root);
        let url = fixture_url(address);
        let request = client.client.get(url.clone());
        let exchange = Exchange::start("discovery", "GET", &url);
        let pending = tokio::spawn(exchange.run(request, false));
        tokio::time::timeout(Duration::from_secs(1), received)
            .await
            .unwrap()
            .unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(2)).await;
        let result = tokio::time::timeout(Duration::from_millis(100), pending)
            .await
            .expect("TLS must spend the connect sub-budget, not the total budget")
            .unwrap();
        assert_eq!(result, Err(ProviderFailure::Timeout));
        tokio::time::resume();

        let body = Exchange::start("discovery", "GET", &url)
            .run(client.client.get(url.clone()), false)
            .await
            .unwrap();
        assert_eq!(body, b"{}");
        assert!(
            server
                .await
                .unwrap()
                .starts_with(b"GET /introspect HTTP/1.1\r\n")
        );
    }
}
