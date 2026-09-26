//! RFC 7662 opaque-token introspection.

use std::{
    fmt,
    future::Future,
    num::NonZeroUsize,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use moka::{Expiry, future::Cache};
use secrecy::ExposeSecret;
use sha2::{Digest, Sha256};
use tokio::{sync::Semaphore, time::Instant};

use crate::{
    BearerToken, Engine, Failure, PreparationError, Principal, VerificationError,
    VerificationReason, Verifier,
    claims::{ClaimPolicy, VerifiedIntrospection, validate_introspection_claims},
    provider::{ProviderClient, ProviderDeadline},
    record_verification,
};

/// Bootstrap input for RFC 7662 token introspection.
#[derive(Clone)]
pub struct IntrospectionOptions {
    pub issuer: crate::ProviderUrl,
    pub audiences: Vec<String>,
    pub endpoint: crate::ProviderUrl,
    pub client_id: String,
    pub client_secret: secrecy::SecretString,
    pub provider_concurrency: NonZeroUsize,
    pub cache: Option<IntrospectionCacheOptions>,
}

impl fmt::Debug for IntrospectionOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IntrospectionOptions([REDACTED])")
    }
}

/// Validated positive-result retention for one prepared introspection verifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntrospectionCacheOptions {
    capacity: usize,
    ttl: Duration,
}

impl IntrospectionCacheOptions {
    /// Selects a best-effort capacity of 1–1024 entries and a fixed TTL of 1–300 seconds.
    ///
    /// # Errors
    ///
    /// Returns [`PreparationError`] when either retention bound is invalid.
    pub fn new(capacity: usize, ttl: Duration) -> Result<Self, PreparationError> {
        if !(1..=1024).contains(&capacity)
            || !(Duration::from_secs(1)..=Duration::from_secs(300)).contains(&ttl)
        {
            return Err(PreparationError::new(
                crate::PreparationPhase::Options,
                crate::PreparationReason::Parse,
            ));
        }
        Ok(Self { capacity, ttl })
    }

    /// The best-effort maximum number of retained entries.
    #[must_use]
    pub const fn capacity(self) -> usize {
        self.capacity
    }

    /// The fixed maximum lifetime of a retained entry.
    #[must_use]
    pub const fn ttl(self) -> Duration {
        self.ttl
    }
}

/// Builds the opaque-token verifier without performing provider I/O.
///
/// # Errors
///
/// Returns [`PreparationError`] for invalid options or client preparation failure.
pub fn prepare_introspection(options: IntrospectionOptions) -> Result<Verifier, PreparationError> {
    prepare_with_provider(options, ProviderClient::new()?)
}

/// Prepares a verifier through fixture-only local TLS transport.
///
/// # Errors
///
/// Returns [`PreparationError`] when the options are invalid.
#[cfg(any(test, feature = "test-support"))]
pub fn prepare_introspection_with_fixture(
    options: IntrospectionOptions,
    fixture: crate::test_support::FixtureTransport,
) -> Result<Verifier, PreparationError> {
    prepare_with_provider(options, fixture.into_provider())
}

fn prepare_with_provider(
    options: IntrospectionOptions,
    provider: ProviderClient,
) -> Result<Verifier, PreparationError> {
    Ok(Verifier::new(IntrospectionVerifier::new(
        options, provider,
    )?))
}

/// A bounded client with optional positive retention for one immutable trust context.
#[derive(Clone)]
struct IntrospectionVerifier {
    endpoint: crate::ProviderUrl,
    client_id: String,
    client_secret: secrecy::SecretString,
    policy: Arc<ClaimPolicy>,
    provider: ProviderClient,
    permits: Arc<Semaphore>,
    cache: Option<IntrospectionCache>,
}

impl fmt::Debug for IntrospectionVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IntrospectionVerifier([REDACTED])")
    }
}

impl Engine for IntrospectionVerifier {
    fn verify<'a>(
        &'a self,
        token: &'a BearerToken<'_>,
    ) -> Pin<Box<dyn Future<Output = Result<Principal, Failure>> + Send + 'a>> {
        Box::pin(
            async move { record_verification("introspection", self.verify_evidence(token).await) },
        )
    }
}

impl IntrospectionVerifier {
    fn new(
        options: IntrospectionOptions,
        provider: ProviderClient,
    ) -> Result<Self, PreparationError> {
        crate::ProviderUrl::parse(options.issuer.as_str())?;
        if options.audiences.is_empty() || options.audiences.iter().any(String::is_empty) {
            return Err(PreparationError::new(
                crate::PreparationPhase::Options,
                crate::PreparationReason::Parse,
            ));
        }
        crate::describe_verification();
        Ok(Self {
            endpoint: options.endpoint,
            client_id: options.client_id,
            client_secret: options.client_secret,
            policy: Arc::new(ClaimPolicy::new(
                options.issuer.as_str().to_owned(),
                options.audiences,
            )),
            provider,
            permits: Arc::new(Semaphore::new(options.provider_concurrency.get())),
            cache: options.cache.map(IntrospectionCache::new),
        })
    }

    async fn verify_evidence(
        &self,
        token: &BearerToken<'_>,
    ) -> Result<Principal, VerificationError> {
        let Some(cache) = &self.cache else {
            return self
                .fetch(token)
                .await
                .map(VerifiedIntrospection::into_principal);
        };
        let key: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        loop {
            let result = cache
                .entries
                .try_get_with(key, async {
                    let evidence = self.fetch(token).await.map_err(FillError::Verification)?;
                    CachedIntrospection::new(
                        evidence,
                        cache.options.ttl,
                        Instant::now(),
                        SystemTime::now(),
                    )
                    .map(Arc::new)
                })
                .await;
            match result {
                Ok(candidate) => {
                    if candidate.is_live(Instant::now(), SystemTime::now()) {
                        return Ok(candidate.evidence.clone().into_principal());
                    }
                    cache.entries.invalidate(&key).await;
                }
                Err(error) => {
                    return match error.as_ref() {
                        FillError::Verification(error) => Err(*error),
                        FillError::NotRetained(evidence) => {
                            Ok(evidence.as_ref().clone().into_principal())
                        }
                    };
                }
            }
        }
    }

    async fn fetch(
        &self,
        token: &BearerToken<'_>,
    ) -> Result<VerifiedIntrospection, VerificationError> {
        let _permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            VerificationError::new(Failure::Unavailable, VerificationReason::Capacity)
        })?;
        let body = form_body(token).map_err(provider_error)?;
        let authorization =
            basic_authorization(&self.client_id, self.client_secret.expose_secret());
        let response = self
            .provider
            .post_form_json(
                self.endpoint.url(),
                &authorization,
                body.as_bytes(),
                ProviderDeadline::independent(Instant::now()),
            )
            .await
            .map_err(provider_error)?;
        validate_introspection_claims(
            &response,
            &self.policy,
            now_epoch_seconds().map_err(provider_error)?,
        )
    }
}

const MAX_ENTRY_BYTES: usize = 64 * 1024;

enum FillError {
    Verification(VerificationError),
    NotRetained(Arc<VerifiedIntrospection>),
}

#[derive(Clone)]
struct CachedIntrospection {
    evidence: VerifiedIntrospection,
    expires_at: Instant,
    token_expires_at: SystemTime,
}

impl CachedIntrospection {
    fn new(
        evidence: VerifiedIntrospection,
        ttl: Duration,
        now: Instant,
        wall: SystemTime,
    ) -> Result<Self, FillError> {
        let expiry = UNIX_EPOCH
            .checked_add(Duration::from_secs(evidence.principal().expires_at()))
            .and_then(|token_expires_at| {
                let remaining = token_expires_at.duration_since(wall).ok()?;
                if remaining.is_zero() {
                    return None;
                }
                Some((now.checked_add(ttl.min(remaining))?, token_expires_at))
            });
        if let Some((expires_at, token_expires_at)) = expiry
            && evidence
                .principal()
                .retained_bytes()
                .is_some_and(|bytes| bytes <= MAX_ENTRY_BYTES)
        {
            Ok(Self {
                evidence,
                expires_at,
                token_expires_at,
            })
        } else {
            Err(FillError::NotRetained(Arc::new(evidence)))
        }
    }

    fn is_live(&self, now: Instant, wall: SystemTime) -> bool {
        now < self.expires_at
            && wall < self.token_expires_at
            && wall
                .duration_since(UNIX_EPOCH)
                .is_ok_and(|elapsed| self.evidence.validate_time(elapsed.as_secs()).is_ok())
    }
}

impl fmt::Debug for CachedIntrospection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CachedIntrospection([REDACTED])")
    }
}

struct CompletionExpiry;

impl Expiry<[u8; 32], Arc<CachedIntrospection>> for CompletionExpiry {
    fn expire_after_create(
        &self,
        _: &[u8; 32],
        value: &Arc<CachedIntrospection>,
        _: std::time::Instant,
    ) -> Option<Duration> {
        Some(value.expires_at.saturating_duration_since(Instant::now()))
    }

    fn expire_after_update(
        &self,
        _: &[u8; 32],
        value: &Arc<CachedIntrospection>,
        _: std::time::Instant,
        _: Option<Duration>,
    ) -> Option<Duration> {
        Some(value.expires_at.saturating_duration_since(Instant::now()))
    }
    // Moka's default read callback preserves the remaining duration.
}

#[derive(Clone)]
struct IntrospectionCache {
    entries: Cache<[u8; 32], Arc<CachedIntrospection>>,
    options: IntrospectionCacheOptions,
}

impl IntrospectionCache {
    fn new(options: IntrospectionCacheOptions) -> Self {
        Self {
            entries: Cache::builder()
                .max_capacity(options.capacity as u64)
                .expire_after(CompletionExpiry)
                .build(),
            options,
        }
    }
}

impl fmt::Debug for IntrospectionCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IntrospectionCache([REDACTED])")
    }
}

fn provider_error(failure: Failure) -> VerificationError {
    VerificationError::new(failure, VerificationReason::Provider)
}

fn now_epoch_seconds() -> Result<u64, Failure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| Failure::Unavailable)
}

fn form_body(token: &BearerToken<'_>) -> Result<String, Failure> {
    let value = std::str::from_utf8(token.as_bytes()).map_err(|_| Failure::Invalid)?;
    Ok(url::form_urlencoded::Serializer::new(String::new())
        .append_pair("token", value)
        .append_pair("token_type_hint", "access_token")
        .finish())
}

fn basic_authorization(client_id: &str, client_secret: &str) -> Vec<u8> {
    let credential = format!(
        "{}:{}",
        url::form_urlencoded::byte_serialize(client_id.as_bytes()).collect::<String>(),
        url::form_urlencoded::byte_serialize(client_secret.as_bytes()).collect::<String>()
    );
    let encoded = STANDARD.encode(credential);
    let mut authorization = b"Basic ".to_vec();
    authorization.extend_from_slice(encoded.as_bytes());
    authorization
}

#[cfg(test)]
mod tests {
    use std::{
        future::{Future, poll_fn},
        num::NonZeroUsize,
        pin::Pin,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        task::Poll,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::Semaphore,
        task::{JoinHandle, JoinSet},
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

    use super::{
        CachedIntrospection, FillError, IntrospectionCacheOptions, IntrospectionOptions,
        IntrospectionVerifier, basic_authorization, form_body, prepare_with_provider,
    };
    use crate::{
        Engine, Failure, ProviderUrl,
        claims::{ClaimPolicy, validate_introspection_claims},
        parse_bearer,
        provider::{ProviderClient, new_fixture_client},
        tls::TlsMaterial,
    };

    const FIXTURE_HOST: &str = "provider.test";

    struct Fixture {
        endpoint: ProviderUrl,
        provider: ProviderClient,
        calls: Arc<AtomicUsize>,
        received: Arc<Semaphore>,
        response: Arc<Mutex<Vec<u8>>>,
        gate: Arc<Mutex<Option<Arc<Semaphore>>>>,
        cancel: CancellationToken,
        task: JoinHandle<()>,
    }

    impl Fixture {
        async fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let material = TlsMaterial::new(FIXTURE_HOST);
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
            let acceptor = TlsAcceptor::from(Arc::new(config));
            let calls = Arc::new(AtomicUsize::new(0));
            let received = Arc::new(Semaphore::new(0));
            let response = Arc::new(Mutex::new(Vec::<u8>::new()));
            let gate = Arc::new(Mutex::new(None::<Arc<Semaphore>>));
            let cancel = CancellationToken::new();
            let task = tokio::spawn({
                let cancel = cancel.clone();
                let calls = calls.clone();
                let received = received.clone();
                let response = response.clone();
                let gate = gate.clone();
                async move {
                    let mut connections = JoinSet::new();
                    loop {
                        let stream = tokio::select! {
                            () = cancel.cancelled() => break,
                            Some(result) = connections.join_next(), if !connections.is_empty() => { result.unwrap(); continue; },
                            accepted = listener.accept() => accepted.unwrap().0,
                        };
                        let acceptor = acceptor.clone();
                        let calls = calls.clone();
                        let received = received.clone();
                        let response = response.clone();
                        let gate = gate.clone();
                        connections.spawn(async move {
                            tokio::time::timeout(Duration::from_secs(5), async {
                                let mut stream = acceptor.accept(stream).await.unwrap();
                                let mut request = Vec::new();
                                let mut chunk = [0_u8; 4096];
                                loop {
                                    let read = stream.read(&mut chunk).await.unwrap();
                                    assert_ne!(read, 0);
                                    request.extend_from_slice(&chunk[..read]);
                                    assert!(request.len() <= 128 * 1024);
                                    if request.windows(4).any(|part| part == b"\r\n\r\n") {
                                        break;
                                    }
                                }
                                let bytes = response.lock().unwrap().clone();
                                let gate = gate.lock().unwrap().clone();
                                calls.fetch_add(1, Ordering::SeqCst);
                                received.add_permits(1);
                                if let Some(gate) = gate {
                                    gate.acquire().await.unwrap().forget();
                                }
                                // A cancelled caller can close its TLS stream before the response.
                                let _ = stream.write_all(&bytes).await;
                                let _ = stream.shutdown().await;
                            })
                            .await
                            .unwrap();
                        });
                    }
                    connections.abort_all();
                    while let Some(result) = connections.join_next().await {
                        if let Err(error) = result {
                            assert!(error.is_cancelled());
                        }
                    }
                }
            });
            let fixture = Self {
                endpoint: ProviderUrl::parse_endpoint(&format!(
                    "https://{FIXTURE_HOST}:{}/introspect",
                    address.port()
                ))
                .unwrap(),
                provider: new_fixture_client(
                    FIXTURE_HOST,
                    address,
                    &material.root,
                    CancellationToken::new(),
                )
                .unwrap(),
                calls,
                received,
                response,
                gate,
                cancel,
                task,
            };
            fixture.respond("200 OK", &active_response("subject", epoch_now() + 600));
            fixture
        }

        fn respond(&self, status: &str, body: &[u8]) {
            let mut response = format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).into_bytes();
            response.extend_from_slice(body);
            *self.response.lock().unwrap() = response;
        }

        fn block_responses(&self) -> Arc<Semaphore> {
            let gate = Arc::new(Semaphore::new(0));
            *self.gate.lock().unwrap() = Some(gate.clone());
            gate
        }

        async fn received(&self) {
            tokio::time::timeout(Duration::from_secs(5), self.received.acquire())
                .await
                .unwrap()
                .unwrap()
                .forget();
        }

        fn options(&self, cache: Option<IntrospectionCacheOptions>) -> IntrospectionOptions {
            IntrospectionOptions {
                issuer: ProviderUrl::parse("https://issuer.example").unwrap(),
                audiences: vec!["api".to_owned()],
                endpoint: self.endpoint.clone(),
                client_id: "fixture-client".to_owned(),
                client_secret: secrecy::SecretString::from("fixture-secret"),
                provider_concurrency: NonZeroUsize::new(1).unwrap(),
                cache,
            }
        }

        fn verifier(&self, cache: Option<IntrospectionCacheOptions>) -> IntrospectionVerifier {
            IntrospectionVerifier::new(self.options(cache), self.provider.clone()).unwrap()
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        async fn finish(self) {
            self.cancel.cancel();
            tokio::time::timeout(Duration::from_secs(5), self.task)
                .await
                .unwrap()
                .unwrap();
        }
    }

    fn epoch_now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
    fn active_response(subject: &str, expiry: u64) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":expiry,"sub":subject,"scope":"read write"})).unwrap()
    }
    fn cache_options(capacity: usize) -> IntrospectionCacheOptions {
        IntrospectionCacheOptions::new(capacity, Duration::from_secs(30)).unwrap()
    }
    async fn verify(
        verifier: &IntrospectionVerifier,
        value: &[u8],
    ) -> Result<crate::Principal, Failure> {
        let token = parse_bearer([value]).unwrap();
        verifier.verify(&token).await
    }
    async fn poll_pending<T>(mut future: Pin<&mut impl Future<Output = T>>) {
        assert!(
            poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx)))
                .await
                .is_pending()
        );
    }
    async fn advance_cache_time(duration: Duration) {
        tokio::time::pause();
        tokio::time::advance(duration).await;
        tokio::time::resume();
    }

    #[test]
    fn client_secret_basic_encodes_each_component_before_base64() {
        assert_eq!(basic_authorization("a:b", "c d"), b"Basic YSUzQWI6Yytk");
    }

    #[test]
    fn form_body_keeps_the_token_out_of_the_url() {
        let token = parse_bearer([b"Bearer a+b/=".as_slice()]).unwrap();
        assert_eq!(
            form_body(&token).unwrap(),
            "token=a%2Bb%2F%3D&token_type_hint=access_token"
        );
    }

    #[tokio::test]
    async fn caching_is_opt_in_shared_only_by_clones_and_exact_token() {
        let fixture = Fixture::new().await;
        let uncached = fixture.verifier(None);
        verify(&uncached, b"Bearer first").await.unwrap();
        verify(&uncached, b"Bearer first").await.unwrap();
        assert_eq!(fixture.calls(), 2);
        let cached = fixture.verifier(Some(cache_options(2)));
        let principal = verify(&cached, b"Bearer first").await.unwrap();
        assert_eq!(principal.scopes(), ["read", "write"]);
        let permit = cached.permits.clone().acquire_owned().await.unwrap();
        assert_eq!(
            verify(&cached.clone(), b"Bearer first").await.unwrap(),
            principal
        );
        assert_eq!(fixture.calls(), 3);
        assert_eq!(
            verify(&cached, b"Bearer second").await,
            Err(Failure::Unavailable)
        );
        drop(permit);
        verify(&cached, b"Bearer second").await.unwrap();
        assert_eq!(fixture.calls(), 4);
        let independent = fixture.verifier(Some(cache_options(2)));
        fixture.respond("200 OK", &active_response("new-subject", epoch_now() + 600));
        assert_eq!(
            verify(&independent, b"Bearer first")
                .await
                .unwrap()
                .subject(),
            Some("new-subject")
        );
        assert_eq!(
            verify(&cached, b"Bearer first").await.unwrap().subject(),
            Some("subject")
        );
        assert_eq!(fixture.calls(), 5);
        let mut options = fixture.options(Some(cache_options(2)));
        options.audiences = vec!["other".to_owned()];
        let changed_context = prepare_with_provider(options, fixture.provider.clone()).unwrap();
        let token = parse_bearer([b"Bearer first".as_slice()]).unwrap();
        assert_eq!(changed_context.verify(&token).await, Err(Failure::Invalid));
        assert_eq!(fixture.calls(), 6);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn lifetime_does_not_slide_and_expired_entries_never_mask_provider_failure() {
        let fixture = Fixture::new().await;
        let verifier = fixture.verifier(Some(cache_options(1)));
        verify(&verifier, b"Bearer first").await.unwrap();
        advance_cache_time(Duration::from_secs(15)).await;
        fixture.respond("503 Service Unavailable", b"{}");
        verify(&verifier, b"Bearer first").await.unwrap();
        assert_eq!(fixture.calls(), 1);
        advance_cache_time(Duration::from_secs(15)).await;
        for _ in 0..2 {
            assert_eq!(
                verify(&verifier, b"Bearer first").await,
                Err(Failure::Unavailable)
            );
        }
        assert_eq!(fixture.calls(), 3);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn unsuccessful_and_already_expired_evidence_is_never_retained() {
        let fixture = Fixture::new().await;
        let verifier = fixture.verifier(Some(cache_options(4)));
        for (response, expected) in [
            (br#"{"active":false}"#.to_vec(), Some(Failure::Invalid)),
            (br#"{"active":true}"#.to_vec(), Some(Failure::Invalid)),
            (b"{".to_vec(), Some(Failure::Unavailable)),
            (active_response("subject", epoch_now() - 1), None),
        ] {
            fixture.respond("200 OK", &response);
            let before = fixture.calls();
            for _ in 0..2 {
                assert_eq!(verify(&verifier, b"Bearer first").await.err(), expected);
            }
            assert_eq!(fixture.calls(), before + 2);
        }
        fixture.finish().await;
    }

    #[tokio::test]
    async fn concurrent_misses_share_provider_capacity_including_nonretained_success_and_errors() {
        let fixture = Fixture::new().await;
        let oversized = serde_json::to_vec(&serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":epoch_now()+600,"sub":"subject","custom":"x".repeat(64*1024)})).unwrap();
        for (response, expected, retained) in [
            (active_response("subject", epoch_now() + 600), None, true),
            (oversized, None, false),
            (active_response("subject", epoch_now() - 1), None, false),
            (
                br#"{"active":false}"#.to_vec(),
                Some(Failure::Invalid),
                false,
            ),
        ] {
            fixture.respond("200 OK", &response);
            let verifier = fixture.verifier(Some(cache_options(1)));
            let gate = fixture.block_responses();
            let before = fixture.calls();
            let mut first = Box::pin(verify(&verifier, b"Bearer shared"));
            tokio::select! { () = fixture.received() => {}, result = &mut first => panic!("response must be gated: {result:?}"), }
            let mut second = Box::pin(verify(&verifier, b"Bearer shared"));
            poll_pending(second.as_mut()).await;
            assert_eq!(
                verify(&verifier, b"Bearer distinct").await,
                Err(Failure::Unavailable)
            );
            gate.add_permits(1);
            let (first, second) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(first, second)
            })
            .await
            .unwrap();
            assert_eq!(first.as_ref().err().copied(), expected);
            assert_eq!(first, second);
            assert_eq!(fixture.calls(), before + 1);
            *fixture.gate.lock().unwrap() = None;
            assert_eq!(verify(&verifier, b"Bearer shared").await.err(), expected);
            assert_eq!(fixture.calls(), before + if retained { 1 } else { 2 });
            // Drain the optional second exchange's arrival before the next case.
            if !retained {
                fixture.received().await;
            }
            if expected.is_some() {
                fixture.respond("200 OK", &active_response("recovered", epoch_now() + 600));
                assert_eq!(
                    verify(&verifier, b"Bearer shared").await.unwrap().subject(),
                    Some("recovered")
                );
                assert_eq!(fixture.calls(), before + 3);
                fixture.received().await;
            }
        }
        fixture.finish().await;
    }

    #[tokio::test]
    async fn cancelling_initializer_does_not_strand_surviving_requests() {
        let fixture = Fixture::new().await;
        let verifier = fixture.verifier(Some(cache_options(2)));
        let gate = fixture.block_responses();
        let mut leader = Box::pin(verify(&verifier, b"Bearer shared"));
        tokio::select! { () = fixture.received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
        let mut survivor = Box::pin(verify(&verifier, b"Bearer shared"));
        poll_pending(survivor.as_mut()).await;
        drop(leader);
        tokio::select! { () = fixture.received() => {}, result = &mut survivor => panic!("replacement response must be gated: {result:?}"), }
        assert_eq!(fixture.calls(), 2);
        gate.add_permits(2);
        tokio::time::timeout(Duration::from_secs(5), survivor)
            .await
            .unwrap()
            .unwrap();
        let permit = verifier.permits.clone().acquire_owned().await.unwrap();
        verify(&verifier, b"Bearer shared").await.unwrap();
        assert_eq!(fixture.calls(), 2);
        drop(permit);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn a_cancelled_waiter_preserves_the_original_fill_and_its_live_hit() {
        let fixture = Fixture::new().await;
        let verifier = fixture.verifier(Some(cache_options(1)));
        let gate = fixture.block_responses();
        let mut leader = Box::pin(verify(&verifier, b"Bearer shared"));
        tokio::select! { () = fixture.received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
        let mut waiter = Box::pin(verify(&verifier, b"Bearer shared"));
        poll_pending(waiter.as_mut()).await;
        drop(waiter);
        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), leader)
            .await
            .unwrap()
            .unwrap();
        let permit = verifier.permits.clone().acquire_owned().await.unwrap();
        verify(&verifier, b"Bearer shared").await.unwrap();
        assert_eq!(fixture.calls(), 1);
        drop(permit);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn capacity_admission_never_refuses_successful_provider_evidence() {
        let fixture = Fixture::new().await;
        let verifier = fixture.verifier(Some(cache_options(1)));
        for token in [
            b"Bearer first".as_slice(),
            b"Bearer second",
            b"Bearer third",
        ] {
            assert_eq!(
                verify(&verifier, token).await.unwrap().subject(),
                Some("subject")
            );
        }
        assert_eq!(fixture.calls(), 3);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn endpoint_query_permission_cannot_weaken_issuer_preparation() {
        let fixture = Fixture::new().await;
        let mut options = fixture.options(None);
        options.issuer =
            ProviderUrl::parse_endpoint("https://issuer.example?tenant=secret").unwrap();
        assert!(prepare_with_provider(options, fixture.provider.clone()).is_err());
        assert_eq!(fixture.calls(), 0);
        fixture.finish().await;
    }

    #[test]
    fn cache_options_have_one_validated_construction_boundary() {
        for (capacity, ttl) in [
            (0, Duration::from_secs(30)),
            (1025, Duration::from_secs(30)),
            (1, Duration::from_millis(999)),
            (1, Duration::from_millis(300_001)),
        ] {
            assert_eq!(
                IntrospectionCacheOptions::new(capacity, ttl)
                    .unwrap_err()
                    .phase(),
                crate::PreparationPhase::Options
            );
        }
        for (capacity, ttl) in [
            (1, Duration::from_secs(1)),
            (1024, Duration::from_secs(300)),
        ] {
            let options = IntrospectionCacheOptions::new(capacity, ttl).unwrap();
            assert_eq!(options.capacity(), capacity);
            assert_eq!(options.ttl(), ttl);
        }
    }

    #[test]
    fn cache_expiry_rechecks_subseconds_wall_clock_and_not_before() {
        let policy = ClaimPolicy::new("https://issuer.example".to_owned(), vec!["api".to_owned()]);
        let evidence = validate_introspection_claims(br#"{"active":true,"iss":"https://issuer.example","aud":"api","exp":131,"nbf":120,"sub":"subject"}"#, &policy, 100).unwrap();
        let now = Instant::now();
        let wall = UNIX_EPOCH + Duration::from_millis(100_500);
        let entry = CachedIntrospection::new(evidence.clone(), Duration::from_secs(60), now, wall)
            .unwrap_or_else(|_| panic!("valid cache evidence"));
        assert!(entry.is_live(now, wall));
        assert!(!entry.is_live(now + Duration::from_millis(30_500), wall));
        assert!(!entry.is_live(now, UNIX_EPOCH + Duration::from_secs(131)));
        assert!(!entry.is_live(now, UNIX_EPOCH + Duration::from_secs(89)));
        assert!(!entry.is_live(now, UNIX_EPOCH - Duration::from_secs(1)));
        assert!(matches!(
            CachedIntrospection::new(
                evidence,
                Duration::from_secs(60),
                now,
                UNIX_EPOCH + Duration::from_secs(131)
            ),
            Err(FillError::NotRetained(_))
        ));
        assert!(!format!("{entry:?}").contains("subject"));
    }
}
