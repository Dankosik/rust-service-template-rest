use std::{
    borrow::Borrow,
    hash::{Hash, Hasher},
    str::FromStr,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use enum_map::EnumMap;
// template:begin oidc-introspection:authn-provider-operation-context-import
use operation_context::{OperationContext, Stopped};
// template:end oidc-introspection:authn-provider-operation-context-import
use reqwest::{header, redirect::Policy};
use shared_rules::{rfc9110, uri};
use tracing::Instrument as _;
use url::Url;

use crate::{ClientBuildError, InvalidUrl};

const MAX_RESPONSE_BYTES: usize = 1_048_576;
pub(crate) const PROVIDER_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_DURATION_METRIC: &str = "authn_provider_request_duration_seconds";

pub const HISTOGRAMS: &[(&str, &[f64])] = &[(
    REQUEST_DURATION_METRIC,
    &[
        0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
    ],
)];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderFailure {
    Timeout,
    Connect,
    Status(u16),
    MediaType,
    TooLarge,
    Transfer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
pub(crate) enum ProviderReason {
    Timeout,
    Connect,
    Status4xx,
    Status5xx,
    StatusOther,
    MediaType,
    TooLarge,
    Transfer,
}

impl ProviderReason {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Timeout => "provider_timeout",
            Self::Connect => "provider_connect",
            Self::Status4xx => "provider_status_4xx",
            Self::Status5xx => "provider_status_5xx",
            Self::StatusOther => "provider_status_other",
            Self::MediaType => "provider_media_type",
            Self::TooLarge => "provider_too_large",
            Self::Transfer => "provider_transfer",
        }
    }
}

impl ProviderFailure {
    fn from_transport(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout
        } else if error.is_connect() {
            Self::Connect
        } else {
            Self::Transfer
        }
    }

    pub(crate) const fn reason(self) -> ProviderReason {
        match self {
            Self::Timeout => ProviderReason::Timeout,
            Self::Connect => ProviderReason::Connect,
            Self::Status(400..=499) => ProviderReason::Status4xx,
            Self::Status(500..=599) => ProviderReason::Status5xx,
            Self::Status(_) => ProviderReason::StatusOther,
            Self::MediaType => ProviderReason::MediaType,
            Self::TooLarge => ProviderReason::TooLarge,
            Self::Transfer => ProviderReason::Transfer,
        }
    }
}

#[derive(Clone, Copy, enum_map::Enum)]
enum Operation {
    // template:begin oidc-jwt:authn-provider-jwt-operations
    Discovery,
    Jwks,
    // template:end oidc-jwt:authn-provider-jwt-operations
    // template:begin oidc-introspection:authn-provider-introspection-operation
    Introspection,
    // template:end oidc-introspection:authn-provider-introspection-operation
}

impl Operation {
    const fn label(self) -> &'static str {
        match self {
            // template:begin oidc-jwt:authn-provider-jwt-operation-labels
            Self::Discovery => "discovery",
            Self::Jwks => "jwks",
            // template:end oidc-jwt:authn-provider-jwt-operation-labels
            // template:begin oidc-introspection:authn-provider-introspection-operation-label
            Self::Introspection => "introspection",
            // template:end oidc-introspection:authn-provider-introspection-operation-label
        }
    }
}

#[derive(Clone, Copy, enum_map::Enum)]
enum Outcome {
    Success,
    Failure(ProviderReason),
    // template:begin oidc-introspection:authn-provider-caller-deadline-outcome
    CallerDeadline,
    // template:end oidc-introspection:authn-provider-caller-deadline-outcome
    Cancelled,
}

impl Outcome {
    const fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure(reason) => reason.label(),
            // template:begin oidc-introspection:authn-provider-caller-deadline-label
            Self::CallerDeadline => "caller_deadline",
            // template:end oidc-introspection:authn-provider-caller-deadline-label
            Self::Cancelled => "cancelled",
        }
    }

    const fn error_label(self) -> Option<&'static str> {
        match self {
            Self::Success => None,
            _ => Some(self.label()),
        }
    }
}

type OperationHistograms = EnumMap<Operation, EnumMap<Outcome, OnceLock<metrics::Histogram>>>;

#[derive(Clone, Eq, Hash, PartialEq, derive_more::Debug)]
#[debug("IssuerUrl([REDACTED])")]
pub struct IssuerUrl(EndpointUrl);

impl IssuerUrl {
    pub fn parse(raw: &str) -> Result<Self, InvalidUrl> {
        let endpoint = parse_endpoint_url(raw)?;
        if endpoint.url().query().is_some() {
            return Err(InvalidUrl);
        }
        Ok(Self(endpoint))
    }

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

impl Borrow<str> for IssuerUrl {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for IssuerUrl {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for IssuerUrl {
    type Err = InvalidUrl;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

#[derive(Clone, Eq, derive_more::Debug)]
#[debug("EndpointUrl([REDACTED])")]
pub struct EndpointUrl {
    exact: String,
    url: Url,
}

impl PartialEq for EndpointUrl {
    fn eq(&self, other: &Self) -> bool {
        self.exact == other.exact
    }
}

impl Hash for EndpointUrl {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.exact.hash(state);
    }
}

impl EndpointUrl {
    pub fn parse(raw: &str) -> Result<Self, InvalidUrl> {
        parse_endpoint_url(raw)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.exact
    }

    pub(crate) fn url(&self) -> &Url {
        &self.url
    }
}

impl Borrow<str> for EndpointUrl {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for EndpointUrl {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for EndpointUrl {
    type Err = InvalidUrl;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

fn parse_endpoint_url(raw: &str) -> Result<EndpointUrl, InvalidUrl> {
    if uri::has_whitespace_or_control(raw) {
        return Err(InvalidUrl);
    }
    let url = Url::parse(raw).map_err(|_| InvalidUrl)?;
    if url.scheme() != "https"
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || uri::authority_has_userinfo(raw)
    {
        return Err(InvalidUrl);
    }
    Ok(EndpointUrl {
        exact: raw.to_owned(),
        url,
    })
}

#[derive(Clone)]
pub(crate) struct ProviderClient {
    client: reqwest::Client,
    histograms: Arc<OperationHistograms>,
}

impl ProviderClient {
    pub(crate) fn try_new() -> Result<Self, ClientBuildError> {
        Self::from_builder(client_builder()).map_err(|_| ClientBuildError)
    }

    fn from_builder(builder: reqwest::ClientBuilder) -> Result<Self, reqwest::Error> {
        metrics::describe_histogram!(
            REQUEST_DURATION_METRIC,
            metrics::Unit::Seconds,
            "Authentication provider exchange duration by operation and closed outcome"
        );
        builder.build().map(|client| Self {
            client,
            histograms: Arc::new(EnumMap::default()),
        })
    }

    fn exchange(&self, operation: Operation, method: &'static str, url: &Url) -> Exchange<'_> {
        Exchange::start(&self.histograms, operation, method, url)
    }

    // template:begin oidc-jwt:authn-provider-get-json
    pub(crate) async fn get_document(
        &self,
        url: &Url,
        document: Document,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Vec<u8>, ProviderFailure> {
        let request = self.client.get(url.clone());
        self.exchange(document.operation(), "GET", url)
            .run(
                || Outcome::Cancelled,
                async move |span| {
                    let response_body = receive(request, false, span);
                    match deadline {
                        None => response_body.await,
                        Some(deadline) => tokio::time::timeout_at(deadline, response_body)
                            .await
                            .unwrap_or(Err(ProviderFailure::Timeout)),
                    }
                },
            )
            .await
    }
    // template:end oidc-jwt:authn-provider-get-json

    // template:begin oidc-introspection:authn-provider-post-form-json
    pub(crate) async fn post_introspection_form_json(
        &self,
        url: &Url,
        client_id: &str,
        client_secret: &str,
        form: &[(&str, &str)],
        caller_stop: StopProbe<'_>,
    ) -> Result<Vec<u8>, ProviderFailure> {
        let request = {
            let client_id = form_encode(client_id);
            let client_secret = zeroize::Zeroizing::new(form_encode(client_secret));
            self.client
                .post(url.clone())
                .basic_auth(&client_id, Some(client_secret.as_str()))
                .form(form)
        };
        self.exchange(Operation::Introspection, "POST", url)
            .run(
                move || {
                    caller_stop
                        .stopped()
                        .map_or(Outcome::Cancelled, stopped_outcome)
                },
                async move |span| receive(request, true, span).await,
            )
            .await
    }
    // template:end oidc-introspection:authn-provider-post-form-json
}

// template:begin oidc-introspection:authn-provider-form-encode
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len().saturating_mul(3));
    encoded.extend(url::form_urlencoded::byte_serialize(value.as_bytes()));
    encoded
}
// template:end oidc-introspection:authn-provider-form-encode

// template:begin oidc-jwt:authn-provider-document
#[derive(Clone, Copy)]
pub(crate) enum Document {
    Discovery,
    Jwks,
}

impl Document {
    const fn operation(self) -> Operation {
        match self {
            Self::Discovery => Operation::Discovery,
            Self::Jwks => Operation::Jwks,
        }
    }
}
// template:end oidc-jwt:authn-provider-document

// template:begin oidc-introspection:authn-provider-stop-probe
#[derive(Clone, Copy)]
pub(crate) enum StopProbe<'a> {
    Context(&'a OperationContext),
    Waiter(&'a OnceLock<Stopped>),
}

impl StopProbe<'_> {
    fn stopped(self) -> Option<Stopped> {
        match self {
            Self::Context(context) => context.stopped(),
            Self::Waiter(stop) => stop.get().copied(),
        }
    }
}

const fn stopped_outcome(stopped: Stopped) -> Outcome {
    match stopped {
        Stopped::Deadline => Outcome::CallerDeadline,
        Stopped::Cancelled => Outcome::Cancelled,
    }
}
// template:end oidc-introspection:authn-provider-stop-probe

struct Exchange<'a> {
    histograms: &'a OperationHistograms,
    operation: Operation,
    started: Instant,
    span: tracing::Span,
}

impl<'a> Exchange<'a> {
    fn start(
        histograms: &'a OperationHistograms,
        operation: Operation,
        method: &'static str,
        url: &Url,
    ) -> Self {
        let span = tracing::info_span!(
            "authn_provider",
            otel.name = method,
            otel.kind = "client",
            http.request.method = method,
            server.address = url.host_str().unwrap_or_default(),
            server.port = url.port_or_known_default().unwrap_or_default(),
            authn.operation = operation.label(),
            http.response.status_code = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        Self {
            histograms,
            operation,
            started: Instant::now(),
            span,
        }
    }

    async fn run(
        self,
        on_abandon: impl FnOnce() -> Outcome,
        work: impl AsyncFnOnce(&tracing::Span) -> Result<Vec<u8>, ProviderFailure>,
    ) -> Result<Vec<u8>, ProviderFailure> {
        let guard = std::mem::DropGuard::new(self, |exchange| exchange.record(on_abandon()));
        let result = work(&guard.span).await;
        let outcome = result
            .as_ref()
            .err()
            .copied()
            .map_or(Outcome::Success, |failure| {
                Outcome::Failure(failure.reason())
            });
        std::mem::DropGuard::dismiss(guard).record(outcome);
        result
    }

    fn record(&self, outcome: Outcome) {
        if let Some(error) = outcome.error_label() {
            self.span.record("error.type", error);
            self.span.record("otel.status_code", "ERROR");
        }
        self.histograms[self.operation][outcome]
            .get_or_init(|| {
                metrics::histogram!(
                    REQUEST_DURATION_METRIC,
                    "operation" => self.operation.label(),
                    "outcome" => outcome.label()
                )
            })
            .record(self.started.elapsed());
    }
}

async fn receive(
    request: reqwest::RequestBuilder,
    require_json_content_type: bool,
    span: &tracing::Span,
) -> Result<Vec<u8>, ProviderFailure> {
    async {
        let transport = |error| ProviderFailure::from_transport(&error);
        let mut response = request.send().await.map_err(transport)?;
        let status = response.status();
        span.record("http.response.status_code", status.as_u16());
        if status != reqwest::StatusCode::OK {
            return Err(ProviderFailure::Status(status.as_u16()));
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
    .instrument(span.clone())
    .await
}

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
    // template:begin oidc-introspection:authn-provider-fixture-feature
    any(feature = "test-support",
    // template:end oidc-introspection:authn-provider-fixture-feature
    test
    // template:begin oidc-introspection:authn-provider-fixture-cfg-close
    )
    // template:end oidc-introspection:authn-provider-fixture-cfg-close
)]
mod fixture {
    use tokio_util::sync::CancellationToken;

    use super::{ProviderClient, client_builder};
    use crate::ClientBuildError;

    pub(crate) fn new_fixture_client(
        fixture_host: &str,
        fixture_addr: std::net::SocketAddr,
        fixture_root_der: &[u8],
        cancel: CancellationToken,
    ) -> Result<ProviderClient, ClientBuildError> {
        if fixture_host.is_empty() || !fixture_addr.ip().is_loopback() {
            return Err(ClientBuildError);
        }
        let root =
            reqwest::Certificate::from_der(fixture_root_der).map_err(|_| ClientBuildError)?;
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
        .map_err(|_| ClientBuildError)
    }

    #[derive(Clone)]
    struct FixtureResolver {
        host: String,
        address: std::net::SocketAddr,
        cancel: CancellationToken,
    }

    impl reqwest::dns::Resolve for FixtureResolver {
        fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            let host_matches = name.as_str().eq_ignore_ascii_case(&self.host);
            let address = self.address;
            let cancel = self.cancel.clone();
            Box::pin(async move {
                if cancel.is_cancelled() || !host_matches {
                    return Err(std::io::Error::other("fixture DNS denied").into());
                }
                Ok(Box::new(std::iter::once(address)) as reqwest::dns::Addrs)
            })
        }
    }
}

#[cfg(
    // template:begin oidc-introspection:authn-provider-fixture-export-feature
    any(feature = "test-support",
    // template:end oidc-introspection:authn-provider-fixture-export-feature
    test
    // template:begin oidc-introspection:authn-provider-fixture-export-cfg-close
    )
    // template:end oidc-introspection:authn-provider-fixture-export-cfg-close
)]
pub(crate) use fixture::new_fixture_client;

fn has_json_content_type(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(rfc9110::is_json_content_type)
}

#[cfg(test)]
pub(crate) fn fixture_acceptor(host: &str) -> (tokio_rustls::TlsAcceptor, Vec<u8>) {
    use tokio_rustls::rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    };

    let material = tls_test_support::TlsMaterial::new(host);
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
        EndpointUrl, IssuerUrl, Operation, Outcome, ProviderClient, ProviderFailure,
        client_builder, fixture_acceptor, new_fixture_client, receive,
    };

    const FIXTURE_HOST: &str = "authn.fixture.test";

    fn test_operation() -> Operation {
        <Operation as enum_map::Enum>::from_usize(0)
    }

    async fn get_untyped(client: &ProviderClient, url: &Url) -> Result<Vec<u8>, ProviderFailure> {
        let request = client.client.get(url.clone());
        client
            .exchange(test_operation(), "GET", url)
            .run(
                || Outcome::Cancelled,
                async move |span| receive(request, false, span).await,
            )
            .await
    }

    #[test]
    fn provider_url_retains_exact_spelling_and_rejects_unsafe_destinations() {
        let url: IssuerUrl = "https://provider.example/path".parse().unwrap();
        assert_eq!(url.as_str(), "https://provider.example/path");
        assert_eq!(url.as_ref(), "https://provider.example/path");
        assert!(
            std::collections::HashSet::from([url.clone()])
                .contains("https://provider.example/path")
        );
        assert_eq!(
            IssuerUrl::parse("https://provider.example/path").unwrap(),
            url
        );
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
            assert_eq!(
                IssuerUrl::parse(invalid),
                Err(crate::InvalidUrl),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn endpoint_spelling_is_preserved_and_provider_debug_is_redacted() {
        let raw = "https://provider.example/keys?token=private%2Fvalue&x=1&x=2";
        let endpoint: EndpointUrl = raw.parse().unwrap();
        assert_eq!(endpoint.as_str(), raw);
        assert_eq!(endpoint.as_ref(), raw);
        assert!(std::collections::HashSet::from([endpoint.clone()]).contains(raw));
        assert_eq!(endpoint.url().as_str(), raw);
        assert_eq!(IssuerUrl::parse(raw), Err(crate::InvalidUrl));
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

    fn http_ok(content_type: &str, body: &str) -> Vec<u8> {
        let content_type = if content_type.is_empty() {
            String::new()
        } else {
            format!("content-type: {content_type}\r\n")
        };
        format!(
            "HTTP/1.1 200 OK\r\n{content_type}content-length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
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
        respond(stream, acceptor, http_ok("", "{}"), false).await
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_dns_expires_at_connect_budget_then_same_client_uses_later_candidate() {
        let response = http_ok("", "{}");
        let (address, root, server) = tls_server(response, false).await;
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
        let url = Url::parse(&format!("https://{FIXTURE_HOST}/keys")).unwrap();
        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(Duration::from_millis(2500), get_untyped(&client, &url))
            .await
            .expect("the connection budget must expire before the three-second total");
        assert_eq!(result, Err(ProviderFailure::Timeout));
        assert!(started.elapsed() >= Duration::from_secs(2));
        assert!(started.elapsed() < Duration::from_millis(2500));

        tokio::time::resume();
        let body = get_untyped(&client, &url).await.unwrap();
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
        assert_eq!(get_untyped(&old_client, &url).await.unwrap(), b"{}");
        assert_eq!(
            get_untyped(&old_client, &url).await,
            Err(ProviderFailure::Connect)
        );

        let rebuilt_client = ProviderClient::from_builder(
            client_builder()
                .dns_resolver(resolver.clone())
                .tls_certs_only([reqwest::Certificate::from_der(&root_b).unwrap()]),
        )
        .unwrap();
        assert_eq!(get_untyped(&rebuilt_client, &url).await.unwrap(), b"{}");
        let (first, replacement) = server.await.unwrap();
        assert!(first.starts_with(b"GET /keys HTTP/1.1\r\n"));
        assert!(replacement.starts_with(b"GET /keys HTTP/1.1\r\n"));
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_provider_redirect_is_reported_instead_of_followed() {
        let response = format!(
            "HTTP/1.1 302 Found\r\nlocation: https://{FIXTURE_HOST}/elsewhere\r\ncontent-length: 0\r\n\r\n"
        )
        .into_bytes();
        let (address, root, server) = tls_server(response, false).await;
        let url = fixture_url(address);
        let client = fixture_client(address, &root);
        let result = get_untyped(&client, &url).await;
        assert_eq!(result, Err(ProviderFailure::Status(302)));
        assert!(
            server
                .await
                .unwrap()
                .starts_with(b"GET /introspect HTTP/1.1\r\n")
        );
    }

    // template:begin oidc-jwt:authn-provider-tls-bounds-test
    #[tokio::test]
    async fn jwt_fixture_transport_uses_trusted_private_tls_and_enforces_body_and_time_bounds() {
        for content_type in ["", "text/plain", "application/json"] {
            let response = http_ok(content_type, r#"{"keys":[]}"#);
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
        use operation_context::OperationContext;
        use tracing::instrument::WithSubscriber as _;

        use super::StopProbe;
        use crate::test_metrics::Diagnostics;

        let context = OperationContext::unbounded();
        let response = http_ok("application/json", r#"{"active":false}"#);
        let (address, root, server) = tls_server(response, false).await;
        let body = fixture_client(address, &root)
            .post_introspection_form_json(
                &fixture_url(address),
                "a:b",
                "c d",
                &[("token", "opaque")],
                StopProbe::Context(&context),
            )
            .await
            .unwrap();
        assert_eq!(body, br#"{"active":false}"#);
        let request = String::from_utf8(server.await.unwrap()).unwrap();
        assert!(request.starts_with("POST /introspect HTTP/1.1\r\n"));
        assert!(request.contains("authorization: Basic YSUzQWI6Yytk\r\n"));

        let diagnostics = Diagnostics::default();
        let _registration_anchor =
            tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let dispatch = diagnostics.dispatch();
        for content_type in ["", "text/plain"] {
            let response = http_ok(content_type, r#"{"active":false}"#);
            let (address, root, server) = tls_server(response, false).await;
            assert_eq!(
                fixture_client(address, &root)
                    .post_introspection_form_json(
                        &fixture_url(address),
                        "fixture",
                        "secret",
                        &[("token", "opaque")],
                        StopProbe::Context(&context),
                    )
                    .with_subscriber(dispatch.clone())
                    .await,
                Err(ProviderFailure::MediaType),
            );
            server.await.unwrap();
        }
        let spans = diagnostics.take_spans();
        let provider_spans = spans
            .iter()
            .filter(|span| span.starts_with("span=authn_provider;"))
            .collect::<Vec<_>>();
        assert_eq!(provider_spans.len(), 2);
        for span in provider_spans {
            assert!(span.contains("http.response.status_code=200;"), "{span}");
            assert!(
                span.contains("error.type=\"provider_media_type\";"),
                "{span}"
            );
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
                while stalled.read(&mut buffer).await.unwrap() != 0 {}
                let (stream, _) = listener.accept().await.unwrap();
                respond(stream, acceptor, http_ok("", "{}"), false).await
            })
            .await
            .unwrap()
        });
        let client = fixture_client(address, &root);
        let url = fixture_url(address);
        let pending = tokio::spawn({
            let client = client.clone();
            let url = url.clone();
            async move { get_untyped(&client, &url).await }
        });
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

        let body = get_untyped(&client, &url).await.unwrap();
        assert_eq!(body, b"{}");
        assert!(
            server
                .await
                .unwrap()
                .starts_with(b"GET /introspect HTTP/1.1\r\n")
        );
    }
}
