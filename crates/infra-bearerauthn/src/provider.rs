//! Trusted provider URL admission and pooled HTTPS transport.

use std::time::{Duration, Instant};

use reqwest::{header, redirect::Policy};
use shared_rules::{rfc9110, uri};
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
const REQUEST_DURATION_METRIC: &str = "authn_provider_request_duration_seconds";

/// Every histogram this crate records. A composition root hands it to the
/// Prometheus recorder; a metric left out renders the default buckets.
pub const HISTOGRAMS: &[(&str, &[f64])] = &[(
    REQUEST_DURATION_METRIC,
    &[
        0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
    ],
)];

/// Why a provider exchange produced no usable body. Closed, and free of
/// provider-supplied text, URLs and credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderFailure {
    Timeout,
    Connect,
    Status(u16),
    /// The response is not `application/json` where that is required.
    MediaType,
    TooLarge,
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
#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("IssuerUrl([REDACTED])")]
pub struct IssuerUrl(EndpointUrl);

impl IssuerUrl {
    /// Parses an issuer URL.
    ///
    /// # Errors
    ///
    /// Returns [`PreparationError`] when the URL violates the provider grammar.
    pub fn parse(raw: &str) -> Result<Self, PreparationError> {
        let url = admit(raw)?;
        if url.url.query().is_some() {
            return Err(invalid_url());
        }
        Ok(Self(url))
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

/// An admitted HTTPS endpoint. [`Self::as_str`] preserves the input spelling.
/// Provider requests use the parsed URL, which can normalize query escaping.
#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("EndpointUrl([REDACTED])")]
pub struct EndpointUrl {
    exact: String,
    url: Url,
}

impl EndpointUrl {
    /// Parses a provider endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`PreparationError`] for a non-HTTPS URL, missing host, userinfo,
    /// fragment, whitespace or control characters.
    pub fn parse(raw: &str) -> Result<Self, PreparationError> {
        admit(raw)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.exact
    }

    pub(crate) fn url(&self) -> &Url {
        &self.url
    }
}

/// [`Url::parse`] silently drops whitespace and an empty userinfo, so the raw
/// spelling is checked too.
fn admit(raw: &str) -> Result<EndpointUrl, PreparationError> {
    if uri::has_whitespace_or_control(raw) {
        return Err(invalid_url());
    }
    let url = Url::parse(raw).map_err(|_| invalid_url())?;
    if url.scheme() != "https"
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || uri::authority_has_userinfo(raw)
    {
        return Err(invalid_url());
    }
    Ok(EndpointUrl {
        exact: raw.to_owned(),
        url,
    })
}

const fn invalid_url() -> PreparationError {
    PreparationError::new(PreparationPhase::Options, PreparationReason::InvalidUrl)
}

/// The only outbound client authentication engines may use.
#[derive(Clone)]
pub(crate) struct ProviderClient {
    client: reqwest::Client,
}

impl ProviderClient {
    pub(crate) fn try_new() -> Result<Self, PreparationError> {
        Self::from_builder(client_builder()).map_err(|()| {
            PreparationError::new(PreparationPhase::Client, PreparationReason::ClientBuild)
        })
    }

    fn from_builder(builder: reqwest::ClientBuilder) -> Result<Self, ()> {
        metrics::describe_histogram!(
            REQUEST_DURATION_METRIC,
            metrics::Unit::Seconds,
            "Authentication provider exchange duration by operation and closed outcome"
        );
        builder
            .build()
            .map(|client| Self { client })
            .map_err(|_| ())
    }

    // template:begin oidc-jwt:authn-provider-get-json
    /// Fetches a provider document whatever media type it is served as: JWKS
    /// is `application/jwk-set+json` (RFC 7517) and providers label both
    /// documents inconsistently, while the JSON parse still rejects a non-JSON body.
    pub(crate) async fn get_document(
        &self,
        url: &Url,
        document: Document,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Vec<u8>, ProviderFailure> {
        Exchange::start(document.operation(), "GET", url)
            .run(
                self.client.get(url.clone()),
                ExchangeMode::Document { deadline },
            )
            .await
    }
    // template:end oidc-jwt:authn-provider-get-json

    // template:begin oidc-introspection:authn-provider-post-form-json
    /// Posts a form with client-secret Basic authentication, form-encoding each
    /// credential component first as RFC 6749 section 2.3.1 requires.
    pub(crate) async fn post_form_json(
        &self,
        url: &Url,
        client_id: &str,
        client_secret: &str,
        form_body: String,
        context: &operation_context::OperationContext,
    ) -> Result<Vec<u8>, ProviderFailure> {
        context.check().map_err(|_| ProviderFailure::Timeout)?;
        let client_id = form_encode(client_id);
        let client_secret = form_encode(client_secret);
        let request = self
            .client
            .post(url.clone())
            .basic_auth(&client_id, Some(&client_secret))
            .header(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/x-www-form-urlencoded"),
            )
            .body(form_body);
        context.check().map_err(|_| ProviderFailure::Timeout)?;
        let result = Exchange::start("introspection", "POST", url)
            .run(request, ExchangeMode::Introspection(context))
            .await;
        context.check().map_err(|_| ProviderFailure::Timeout)?;
        result
    }
    // template:end oidc-introspection:authn-provider-post-form-json
}

// template:begin oidc-introspection:authn-provider-form-encode
fn form_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
// template:end oidc-introspection:authn-provider-form-encode

// template:begin oidc-jwt:authn-provider-document
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

enum ExchangeMode<'a> {
    #[allow(
        dead_code,
        reason = "the introspection-only profile removes document requests"
    )]
    Document {
        deadline: Option<tokio::time::Instant>,
    },
    #[allow(
        dead_code,
        reason = "the JWT-only profile removes introspection requests"
    )]
    Introspection(&'a operation_context::OperationContext),
}

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

    async fn run(
        self,
        request: reqwest::RequestBuilder,
        mode: ExchangeMode<'_>,
    ) -> Result<Vec<u8>, ProviderFailure> {
        let require_json_content_type = matches!(mode, ExchangeMode::Introspection(_));
        let receive = receive(request, require_json_content_type).instrument(self.span.clone());
        let result = match mode {
            ExchangeMode::Introspection(context) => {
                tokio::select! {
                    biased;
                    _ = context.wait_stopped() => return Err(ProviderFailure::Timeout),
                    () = tokio::time::sleep(PROVIDER_TIMEOUT) => Err(ProviderFailure::Timeout),
                    result = receive => result,
                }
            }
            ExchangeMode::Document {
                deadline: Some(deadline),
            } => tokio::time::timeout_at(deadline, receive)
                .await
                .unwrap_or(Err(ProviderFailure::Timeout)),
            ExchangeMode::Document { deadline: None } => receive.await,
        };
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
        self.record(failure.map(ProviderFailure::label));
    }

    fn record(&self, failure: Option<&'static str>) {
        if let Some(failure) = failure {
            self.span.record("error.type", failure);
            self.span.record("otel.status_code", "ERROR");
        }
        let outcome = failure.unwrap_or("success");
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
            self.record(Some("cancelled"));
        }
    }
}

async fn receive(
    request: reqwest::RequestBuilder,
    require_json_content_type: bool,
) -> Result<Vec<u8>, ProviderFailure> {
    let transport = |error| ProviderFailure::from_transport(&error);
    let mut response = request.send().await.map_err(transport)?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(ProviderFailure::Status(response.status().as_u16()));
    }
    if require_json_content_type && !has_json_content_type(&response) {
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

/// The client policy every provider client shares, fixtures included. Trust
/// material comes only from the configured endpoint, so a redirect or an
/// ambient proxy cannot move a fetch to another host; no retries keep each
/// exchange inside its budget, and no `Referer` leaks request paths.
fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .tls_backend_rustls()
        .https_only(true)
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .referer(false)
        .connect_timeout(Duration::from_secs(2))
        .timeout(PROVIDER_TIMEOUT)
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
    let resolver = std::sync::Arc::new(FixtureResolver {
        host: fixture_host.to_owned(),
        address: fixture_addr,
        cancel,
    });
    ProviderClient::from_builder(
        client_builder()
            .dns_resolver(resolver)
            .tls_certs_only([root]),
    )
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

fn has_json_content_type(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(rfc9110::is_json_content_type)
}

/// A TLS acceptor for `host` and the root certificate that trusts it.
#[cfg(test)]
pub(crate) fn fixture_acceptor(host: &str) -> (tokio_rustls::TlsAcceptor, Vec<u8>) {
    use tokio_rustls::rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    };

    let material = crate::tls::TlsMaterial::new(host);
    let config = ServerConfig::builder_with_provider(std::sync::Arc::new(
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
        tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config)),
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
        EndpointUrl, Exchange, ExchangeMode, IssuerUrl, ProviderClient, ProviderFailure,
        client_builder, fixture_acceptor, new_fixture_client,
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
            "https://@provider.example",
            "https://provider.example/\u{a0}tenant",
        ] {
            assert!(IssuerUrl::parse(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn endpoint_spelling_is_preserved_and_provider_debug_is_redacted() {
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
            if request.array_windows().any(|part| part == b"\r\n\r\n") {
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
        let client = ProviderClient::from_builder(
            client_builder()
                .dns_resolver(resolver)
                .tls_certs_only([reqwest::Certificate::from_der(&root).unwrap()]),
        )
        .unwrap();
        // No explicit URL port: retain each resolver candidate's fixture port.
        let url = Url::parse(&format!("https://{FIXTURE_HOST}/keys")).unwrap();
        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_millis(2500),
            Exchange::start("discovery", "GET", &url).run(
                client.client.get(url.clone()),
                ExchangeMode::Document { deadline: None },
            ),
        )
        .await
        .expect("the connection budget must expire before the three-second total");
        assert_eq!(result, Err(ProviderFailure::Timeout));
        assert!(started.elapsed() >= Duration::from_secs(2));
        assert!(started.elapsed() < Duration::from_millis(2500));

        tokio::time::resume();
        let body = Exchange::start("discovery", "GET", &url)
            .run(
                client.client.get(url.clone()),
                ExchangeMode::Document { deadline: None },
            )
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
        let old_client = ProviderClient::from_builder(
            client_builder()
                .dns_resolver(resolver.clone())
                .tls_certs_only([reqwest::Certificate::from_der(&root_a).unwrap()]),
        )
        .unwrap();
        let url = Url::parse(&format!("https://{FIXTURE_HOST}/keys")).unwrap();
        assert_eq!(
            Exchange::start("jwks", "GET", &url)
                .run(
                    old_client.client.get(url.clone()),
                    ExchangeMode::Document { deadline: None }
                )
                .await
                .unwrap(),
            b"{}"
        );
        assert_eq!(
            Exchange::start("jwks", "GET", &url)
                .run(
                    old_client.client.get(url.clone()),
                    ExchangeMode::Document { deadline: None }
                )
                .await,
            Err(ProviderFailure::Connect)
        );

        let rebuilt_client = ProviderClient::from_builder(
            client_builder()
                .dns_resolver(resolver.clone())
                .tls_certs_only([reqwest::Certificate::from_der(&root_b).unwrap()]),
        )
        .unwrap();
        assert_eq!(
            Exchange::start("jwks", "GET", &url)
                .run(
                    rebuilt_client.client.get(url.clone()),
                    ExchangeMode::Document { deadline: None }
                )
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
                .get_document(endpoint.url(), Document::Jwks, None)
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
                .get_document(&fixture_url(address), Document::Jwks, None)
                .await,
            Err(ProviderFailure::TooLarge)
        );
        let _ = server.await;

        let response = b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n".to_vec();
        let (address, root, server) = tls_server(response, false).await;
        assert_eq!(
            fixture_client(address, &root)
                .get_document(&fixture_url(address), Document::Jwks, None)
                .await,
            Err(ProviderFailure::Status(404))
        );
        server.await.unwrap();

        let response = b"HTTP/1.1 200 OK\r\ncontent-length: 11\r\n\r\n{".to_vec();
        let (address, root, server) = tls_server(response, true).await;
        assert_eq!(
            fixture_client(address, &root)
                .get_document(&fixture_url(address), Document::Jwks, None)
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
                "a:b",
                "c d",
                "token=opaque".to_owned(),
                &operation_context::OperationContext::with_timeout(super::PROVIDER_TIMEOUT),
            )
            .await
            .unwrap();
        assert_eq!(body, br#"{"active":false}"#);
        let request = String::from_utf8(server.await.unwrap()).unwrap();
        assert!(request.starts_with("POST /introspect HTTP/1.1\r\n"));
        // Each component is form-encoded before the Basic scheme encodes the pair.
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
        let pending =
            tokio::spawn(exchange.run(request, ExchangeMode::Document { deadline: None }));
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
            .run(
                client.client.get(url.clone()),
                ExchangeMode::Document { deadline: None },
            )
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
