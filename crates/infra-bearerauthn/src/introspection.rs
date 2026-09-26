//! RFC 7662 opaque-token introspection.

use std::{
    borrow::Borrow,
    collections::{HashMap, hash_map::Entry},
    fmt,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use secrecy::{ExposeSecret, SecretString};
use tokio::{sync::Semaphore, time::Instant};

use crate::{
    BearerToken, Failure, IntrospectionCacheOptions, IntrospectionOptions, PreparationError,
    Principal, VerificationError, VerificationReason, Verifier,
    claims::{ClaimPolicy, VerifiedIntrospection, validate_introspection_claims},
    provider::{ProviderClient, ProviderDeadline, reserve_request_deadline},
    record_verification,
};

/// Builds the opaque-token verifier without performing provider I/O.
///
/// # Errors
///
/// Returns [`PreparationError`] for invalid options or client preparation failure.
pub fn prepare_introspection(options: IntrospectionOptions) -> Result<Verifier, PreparationError> {
    let provider = ProviderClient::new().map_err(|error| {
        error.with_context(
            &options.issuer,
            &options.audiences,
            Some(options.endpoint.as_str()),
        )
    })?;
    prepare_with_provider(options, provider)
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
    if options.audiences.is_empty()
        || options.audiences.iter().any(String::is_empty)
        || options.cache.is_some_and(|cache| {
            !(1..=1024).contains(&cache.capacity)
                || !(Duration::from_secs(1)..=Duration::from_secs(300)).contains(&cache.ttl)
        })
    {
        return Err(PreparationError::new(
            crate::PreparationPhase::Options,
            crate::PreparationReason::Parse,
        )
        .with_context(
            &options.issuer,
            &options.audiences,
            Some(options.endpoint.as_str()),
        ));
    }
    Ok(Verifier::Introspection(IntrospectionVerifier {
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
    }))
}

/// A bounded client with optional positive retention for one immutable trust context.
#[derive(Clone)]
pub struct IntrospectionVerifier {
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

impl IntrospectionVerifier {
    pub(crate) async fn verify(
        &self,
        token: &BearerToken<'_>,
        deadline: Instant,
    ) -> Result<Principal, Failure> {
        record_verification("introspection", self.verify_evidence(token, deadline).await)
    }

    async fn verify_evidence(
        &self,
        token: &BearerToken<'_>,
        deadline: Instant,
    ) -> Result<Principal, VerificationError> {
        check_request_reserve(Instant::now(), deadline)?;
        let token_value =
            std::str::from_utf8(token.as_bytes()).map_err(|_| provider_error(Failure::Invalid))?;
        if let Some(cache) = &self.cache
            && let Some(candidate) = cache.lookup(token_value, Instant::now(), SystemTime::now())
            && let Some(principal) = candidate.admit(deadline, Instant::now(), SystemTime::now())?
        {
            return Ok(principal);
        }
        let provider_deadline = ProviderDeadline::request(Instant::now(), deadline)
            .ok_or_else(|| provider_error(Failure::Timeout))?;
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
                provider_deadline,
            )
            .await
            .map_err(provider_error)?;
        let evidence = validate_introspection_claims(
            &response,
            &self.policy,
            now_epoch_seconds().map_err(provider_error)?,
        )?;
        if let Some(cache) = &self.cache {
            cache.complete(token_value, evidence, deadline)
        } else {
            check_request_reserve(Instant::now(), deadline)?;
            Ok(evidence.into_principal())
        }
    }
}

const MAX_ENTRY_BYTES: usize = 64 * 1024;

struct TokenKey(SecretString);

impl Borrow<str> for TokenKey {
    fn borrow(&self) -> &str {
        self.0.expose_secret()
    }
}

impl PartialEq for TokenKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.expose_secret() == other.0.expose_secret()
    }
}

impl Eq for TokenKey {}

impl Hash for TokenKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.expose_secret().hash(state);
    }
}

impl fmt::Debug for TokenKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TokenKey([REDACTED])")
    }
}

#[derive(Clone)]
struct CachedIntrospection {
    evidence: VerifiedIntrospection,
    expires_at: Instant,
    token_expires_at: SystemTime,
}

impl CachedIntrospection {
    fn new(
        evidence: &VerifiedIntrospection,
        ttl: Duration,
        now: Instant,
        wall: SystemTime,
    ) -> Option<Self> {
        wall.duration_since(UNIX_EPOCH).ok()?;
        let token_expires_at =
            UNIX_EPOCH.checked_add(Duration::from_secs(evidence.principal().expires_at()))?;
        let remaining = token_expires_at.duration_since(wall).ok()?;
        if remaining.is_zero() {
            return None;
        }
        let expires_at = now.checked_add(ttl.min(remaining))?;
        Some(Self {
            evidence: evidence.clone(),
            expires_at,
            token_expires_at,
        })
    }

    fn is_live(&self, now: Instant, wall: SystemTime) -> bool {
        now < self.expires_at && wall < self.token_expires_at
    }

    fn admit(
        self,
        deadline: Instant,
        now: Instant,
        wall: SystemTime,
    ) -> Result<Option<Principal>, VerificationError> {
        check_request_reserve(now, deadline)?;
        let wall_seconds = wall
            .duration_since(UNIX_EPOCH)
            .map_err(|_| provider_error(Failure::Unavailable))?
            .as_secs();
        if !self.is_live(now, wall) {
            return Ok(None);
        }
        self.evidence.validate_time(wall_seconds)?;
        Ok(Some(self.evidence.into_principal()))
    }
}

impl fmt::Debug for CachedIntrospection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CachedIntrospection([REDACTED])")
    }
}

#[derive(Clone)]
struct IntrospectionCache {
    entries: Arc<Mutex<HashMap<TokenKey, CachedIntrospection>>>,
    options: IntrospectionCacheOptions,
}

impl IntrospectionCache {
    fn new(options: IntrospectionCacheOptions) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::with_capacity(options.capacity))),
            options,
        }
    }

    fn lookup(&self, token: &str, now: Instant, wall: SystemTime) -> Option<CachedIntrospection> {
        let mut entries = self.entries.try_lock().ok()?;
        if entries.get(token)?.is_live(now, wall) {
            entries.get(token).cloned()
        } else {
            entries.remove(token);
            None
        }
    }

    fn complete(
        &self,
        token: &str,
        evidence: VerifiedIntrospection,
        deadline: Instant,
    ) -> Result<Principal, VerificationError> {
        // Build and size the retained copy before locking. The completion clocks
        // remain fixed even if cloning or cache admission takes time.
        let candidate = CachedIntrospection::new(
            &evidence,
            self.options.ttl,
            Instant::now(),
            SystemTime::now(),
        )
        .filter(|candidate| {
            candidate
                .evidence
                .principal()
                .retained_bytes()
                .and_then(|bytes| bytes.checked_add(token.len()))
                .is_some_and(|bytes| bytes <= MAX_ENTRY_BYTES)
        });
        let mut entries = self.entries.try_lock().ok();
        let mut inserted = false;
        if let (Some(entries), Some(candidate)) = (entries.as_mut(), candidate) {
            let now = Instant::now();
            let wall = SystemTime::now();
            entries.retain(|_, entry| entry.is_live(now, wall));
            if entries.len() < self.options.capacity && candidate.is_live(now, wall) {
                // A concurrent miss must not replace a live entry or renew it.
                if let Entry::Vacant(entry) = entries.entry(TokenKey(SecretString::from(token))) {
                    entry.insert(candidate);
                    inserted = true;
                }
            }
        }
        if let Err(error) = check_request_reserve(Instant::now(), deadline) {
            if inserted && let Some(entries) = &mut entries {
                entries.remove(token);
            }
            return Err(error);
        }
        Ok(evidence.into_principal())
    }
}

impl fmt::Debug for IntrospectionCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IntrospectionCache([REDACTED])")
    }
}

fn check_request_reserve(now: Instant, deadline: Instant) -> Result<(), VerificationError> {
    reserve_request_deadline(now, deadline)
        .map(|_| ())
        .ok_or_else(|| provider_error(Failure::Timeout))
}

fn provider_error(failure: Failure) -> VerificationError {
    VerificationError::new(
        failure,
        if failure == Failure::Timeout {
            VerificationReason::RequestTimeout
        } else {
            VerificationReason::Provider
        },
    )
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
    let mut credential = form_component(client_id);
    credential.push(':');
    credential.push_str(&form_component(client_secret));
    let encoded = STANDARD.encode(credential);
    let mut authorization = b"Basic ".to_vec();
    authorization.extend_from_slice(encoded.as_bytes());
    authorization
}

fn form_component(value: &str) -> String {
    let encoded = url::form_urlencoded::Serializer::new(String::new())
        .append_pair(value, "")
        .finish();
    encoded[..encoded.len().saturating_sub(1)].to_owned()
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroUsize,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::Semaphore,
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

    use super::{
        CachedIntrospection, ClaimPolicy, IntrospectionCache, IntrospectionVerifier,
        basic_authorization, form_body, prepare_with_provider,
    };
    use crate::tls::TlsMaterial;
    use crate::{
        Failure, IntrospectionCacheOptions, IntrospectionOptions, ProviderUrl, VerificationReason,
        Verifier,
        claims::validate_introspection_claims,
        parse_bearer,
        provider::{ProviderClient, new_fixture_client},
    };

    const FIXTURE_HOST: &str = "authn.fixture.test";

    struct Fixture {
        endpoint: ProviderUrl,
        provider: ProviderClient,
        calls: Arc<AtomicUsize>,
        response: Arc<Mutex<Vec<u8>>>,
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
                vec![CertificateDer::from(material.cert.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(material.key.clone())),
            )
            .unwrap();
            let acceptor = TlsAcceptor::from(Arc::new(config));
            let calls = Arc::new(AtomicUsize::new(0));
            let response = Arc::new(Mutex::new(Vec::new()));
            let cancel = CancellationToken::new();
            let task = tokio::spawn({
                let cancel = cancel.clone();
                let calls = calls.clone();
                let response = response.clone();
                async move {
                    loop {
                        let stream = tokio::select! {
                            () = cancel.cancelled() => break,
                            accepted = listener.accept() => accepted.unwrap().0,
                        };
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
                            calls.fetch_add(1, Ordering::SeqCst);
                            let bytes = response.lock().unwrap().clone();
                            stream.write_all(&bytes).await.unwrap();
                            stream.shutdown().await.unwrap();
                        })
                        .await
                        .unwrap();
                    }
                }
            });
            let fixture = Self {
                endpoint: ProviderUrl::parse(&format!(
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
                response,
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
            let Verifier::Introspection(verifier) =
                prepare_with_provider(self.options(cache), self.provider.clone()).unwrap()
            else {
                panic!("introspection verifier required");
            };
            verifier
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
        IntrospectionCacheOptions {
            capacity,
            ttl: Duration::from_secs(30),
        }
    }

    async fn verify(
        verifier: &IntrospectionVerifier,
        value: &[u8],
    ) -> Result<crate::Principal, Failure> {
        let token = parse_bearer([value], 32 * 1024).unwrap();
        verifier
            .verify(&token, Instant::now() + Duration::from_secs(4))
            .await
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
        let token = parse_bearer([b"Bearer a+b/=".as_slice()], 32 * 1024).unwrap();
        assert_eq!(
            form_body(&token).unwrap(),
            "token=a%2Bb%2F%3D&token_type_hint=access_token"
        );
    }

    #[tokio::test]
    async fn exhausted_configured_capacity_rejects_before_provider_io() {
        let verifier = IntrospectionVerifier {
            endpoint: ProviderUrl::parse("https://127.0.0.1/introspect").unwrap(),
            client_id: "client".to_owned(),
            client_secret: secrecy::SecretString::from("secret"),
            policy: Arc::new(ClaimPolicy::new(
                "https://issuer.example".to_owned(),
                vec!["api".to_owned()],
            )),
            provider: ProviderClient::new().unwrap(),
            permits: Arc::new(Semaphore::new(0)),
            cache: None,
        };
        let token = parse_bearer([b"Bearer opaque".as_slice()], 32 * 1024).unwrap();
        assert_eq!(
            verifier
                .verify(&token, Instant::now() + std::time::Duration::from_secs(1))
                .await,
            Err(Failure::Unavailable)
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
        // A different token cannot use the hit to bypass provider admission.
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
        let token = parse_bearer([b"Bearer first".as_slice()], 32 * 1024).unwrap();
        assert_eq!(
            changed_context
                .verify(&token, Instant::now() + Duration::from_secs(4))
                .await,
            Err(Failure::Invalid)
        );
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
        assert_eq!(
            verify(&verifier, b"Bearer first").await,
            Err(Failure::Unavailable)
        );
        assert_eq!(fixture.calls(), 2);
        assert_eq!(
            verify(&verifier, b"Bearer first").await,
            Err(Failure::Unavailable)
        );
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
    #[allow(
        clippy::await_holding_lock,
        reason = "hold the cache lock deliberately to prove nonblocking provider fallback"
    )]
    async fn full_busy_poisoned_and_oversized_cache_bypasses_preserve_verification() {
        let fixture = Fixture::new().await;
        let verifier = fixture.verifier(Some(cache_options(1)));
        verify(&verifier, b"Bearer first").await.unwrap();
        verify(&verifier, b"Bearer second").await.unwrap();
        verify(&verifier, b"Bearer second").await.unwrap();
        verify(&verifier, b"Bearer first").await.unwrap();
        assert_eq!(fixture.calls(), 3);
        {
            let _guard = verifier.cache.as_ref().unwrap().entries.lock().unwrap();
            verify(&verifier, b"Bearer first").await.unwrap();
            assert_eq!(fixture.calls(), 4);
        }
        let entries = verifier.cache.as_ref().unwrap().entries.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = entries.lock().unwrap();
                panic!("fixture poisons cache state");
            })
            .join()
            .is_err()
        );
        verify(&verifier, b"Bearer first").await.unwrap();
        verify(&verifier, b"Bearer first").await.unwrap();
        assert_eq!(fixture.calls(), 6);

        let oversized = fixture.verifier(Some(cache_options(1)));
        fixture.respond(
            "200 OK",
            &active_response(&"x".repeat(64 * 1024), epoch_now() + 600),
        );
        assert_eq!(
            verify(&oversized, b"Bearer first")
                .await
                .unwrap()
                .subject()
                .unwrap()
                .len(),
            64 * 1024
        );
        verify(&oversized, b"Bearer first").await.unwrap();
        assert_eq!(fixture.calls(), 8);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn direct_callers_cannot_prepare_unbounded_cache_options() {
        let fixture = Fixture::new().await;
        for options in [
            IntrospectionCacheOptions {
                capacity: 0,
                ttl: Duration::from_secs(30),
            },
            IntrospectionCacheOptions {
                capacity: 1025,
                ttl: Duration::from_secs(30),
            },
            IntrospectionCacheOptions {
                capacity: 1,
                ttl: Duration::from_millis(999),
            },
            IntrospectionCacheOptions {
                capacity: 1,
                ttl: Duration::from_millis(300_001),
            },
        ] {
            let error =
                prepare_with_provider(fixture.options(Some(options)), fixture.provider.clone())
                    .unwrap_err();
            assert_eq!(error.phase(), crate::PreparationPhase::Options);
        }
        for options in [
            IntrospectionCacheOptions {
                capacity: 1,
                ttl: Duration::from_secs(1),
            },
            IntrospectionCacheOptions {
                capacity: 1024,
                ttl: Duration::from_secs(300),
            },
        ] {
            assert!(
                prepare_with_provider(fixture.options(Some(options)), fixture.provider.clone())
                    .is_ok()
            );
        }
        assert_eq!(fixture.calls(), 0);
        fixture.finish().await;
    }

    #[test]
    fn cached_evidence_rechecks_strict_clocks_request_reserve_and_not_before() {
        let policy = ClaimPolicy::new("https://issuer.example".to_owned(), vec!["api".to_owned()]);
        let response = br#"{"active":true,"iss":"https://issuer.example","aud":"api","exp":131,"nbf":120,"sub":"subject"}"#;
        let evidence = validate_introspection_claims(response, &policy, 100).unwrap();
        let now = Instant::now();
        let wall = UNIX_EPOCH + Duration::from_millis(100_500);
        let entry =
            CachedIntrospection::new(&evidence, Duration::from_secs(60), now, wall).unwrap();
        let deadline = now + Duration::from_secs(120);
        assert!(entry.clone().admit(deadline, now, wall).unwrap().is_some());
        // Subsecond remaining token lifetime bounds monotonic retention too.
        assert!(
            entry
                .clone()
                .admit(deadline, now + Duration::from_millis(30_500), wall)
                .unwrap()
                .is_none()
        );
        assert!(
            entry
                .clone()
                .admit(deadline, now, UNIX_EPOCH + Duration::from_secs(131))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            entry
                .clone()
                .admit(now + Duration::from_millis(100), now, wall)
                .unwrap_err()
                .failure,
            Failure::Timeout
        );
        assert_eq!(
            entry
                .clone()
                .admit(deadline, now, UNIX_EPOCH + Duration::from_secs(89))
                .unwrap_err()
                .reason,
            VerificationReason::NotYetValid
        );
        assert_eq!(
            entry
                .clone()
                .admit(deadline, now, UNIX_EPOCH - Duration::from_secs(1))
                .unwrap_err()
                .failure,
            Failure::Unavailable
        );
        assert!(
            CachedIntrospection::new(
                &evidence,
                Duration::from_secs(60),
                now,
                UNIX_EPOCH + Duration::from_secs(131)
            )
            .is_none()
        );
        let cache = IntrospectionCache::new(cache_options(1));
        assert!(!format!("{entry:?} {cache:?}").contains("subject"));
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_completion_cannot_renew_a_live_entry_and_timed_out_results_are_removed() {
        let policy = ClaimPolicy::new("https://issuer.example".to_owned(), vec!["api".to_owned()]);
        let evidence = validate_introspection_claims(
            &active_response("subject", epoch_now() + 600),
            &policy,
            epoch_now(),
        )
        .unwrap();
        let cache = IntrospectionCache::new(cache_options(1));
        let started = Instant::now();
        cache
            .complete(
                "first",
                evidence.clone(),
                started + Duration::from_secs(120),
            )
            .unwrap();
        tokio::time::advance(Duration::from_secs(15)).await;
        cache
            .complete(
                "first",
                evidence.clone(),
                started + Duration::from_secs(120),
            )
            .unwrap();
        tokio::time::advance(Duration::from_secs(15)).await;
        assert!(
            cache
                .lookup("first", Instant::now(), SystemTime::now())
                .is_none()
        );
        assert_eq!(
            cache
                .complete(
                    "second",
                    evidence,
                    Instant::now() + Duration::from_millis(100)
                )
                .unwrap_err()
                .failure,
            Failure::Timeout
        );
        assert!(
            cache
                .lookup("second", Instant::now(), SystemTime::now())
                .is_none()
        );
    }
}
