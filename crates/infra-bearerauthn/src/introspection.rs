//! RFC 7662 opaque-token introspection.

use std::{
    fmt,
    num::NonZeroUsize,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use moka::{Expiry, future::Cache};
use secrecy::ExposeSecret;
use tokio::sync::Semaphore;

use crate::{
    BearerToken, Failure, PreparationError, Principal, VerificationError, VerificationReason,
    Verifier,
    claims::{ClaimPolicy, VerifiedIntrospection, validate_introspection_claims},
    provider::ProviderClient,
};

/// The largest verified provider payload one cache entry retains.
const MAX_ENTRY_BYTES: usize = 64 * 1024;

/// Bootstrap input for RFC 7662 token introspection.
#[derive(Clone)]
pub struct IntrospectionOptions {
    pub issuer: crate::IssuerUrl,
    pub audiences: Vec<String>,
    pub endpoint: crate::EndpointUrl,
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
    IntrospectionVerifier::new(options, ProviderClient::new()?).map(Verifier::introspection)
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
    IntrospectionVerifier::new(options, fixture.into_provider()).map(Verifier::introspection)
}

/// A bounded provider client with optional positive retention for one
/// immutable trust context.
pub(crate) struct IntrospectionVerifier {
    endpoint: crate::EndpointUrl,
    client_id: String,
    client_secret: secrecy::SecretString,
    policy: ClaimPolicy,
    provider: ProviderClient,
    permits: Semaphore,
    cache: Option<PositiveCache>,
}

struct PositiveCache {
    retained: Cache<[u8; 32], VerifiedIntrospection>,
    fills: Cache<[u8; 32], Fill>,
}

#[derive(Clone)]
enum Fill {
    Fresh(VerifiedIntrospection),
    Retained(VerifiedIntrospection),
}

impl IntrospectionVerifier {
    fn new(
        options: IntrospectionOptions,
        provider: ProviderClient,
    ) -> Result<Self, PreparationError> {
        let policy = ClaimPolicy::new(options.issuer.as_str().to_owned(), options.audiences)?;
        crate::describe_verification();
        Ok(Self {
            endpoint: options.endpoint,
            client_id: options.client_id,
            client_secret: options.client_secret,
            policy,
            provider,
            permits: Semaphore::new(options.provider_concurrency.get()),
            cache: options.cache.map(|cache| PositiveCache {
                retained: Cache::builder()
                    .max_capacity(cache.capacity as u64)
                    .expire_after(Retention { ttl: cache.ttl })
                    .build(),
                fills: Cache::builder()
                    .max_capacity(cache.capacity as u64)
                    .time_to_live(Duration::ZERO)
                    .build(),
            }),
        })
    }

    /// Rechecks retained calendar eligibility; concurrent misses share tagged
    /// evidence so a waiter cannot mistake retention for fresh provider trust.
    pub(crate) async fn verify(
        &self,
        token: &BearerToken<'_>,
    ) -> Result<Principal, VerificationError> {
        self.verify_with_now(token, crate::unix_now).await
    }

    async fn verify_with_now(
        &self,
        token: &BearerToken<'_>,
        now: impl Fn() -> Result<u64, VerificationError>,
    ) -> Result<Principal, VerificationError> {
        now()?;
        let Some(cache) = &self.cache else {
            return self.introspect(token, &now).await?.deliver(now()?);
        };
        let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, token.as_bytes());
        let mut key = [0_u8; 32];
        key.copy_from_slice(digest.as_ref());
        loop {
            if let Some(value) = cache.retained.get(&key).await
                && value.reusable_at(now()?)
            {
                return Ok(value.principal);
            }
            let filled = cache
                .fills
                .try_get_with(key, async {
                    now()?;
                    if let Some(value) = cache.retained.get(&key).await
                        && value.reusable_at(now()?)
                    {
                        return Ok::<_, VerificationError>(Fill::Retained(value));
                    }
                    let value = self.introspect(token, &now).await?;
                    cache.retained.insert(key, value.clone()).await;
                    Ok(Fill::Fresh(value))
                })
                .await
                .map_err(|error| *error)?;
            match filled {
                Fill::Fresh(value) => return value.deliver(now()?),
                Fill::Retained(value) if value.reusable_at(now()?) => {
                    return Ok(value.principal);
                }
                Fill::Retained(_) => {}
            }
        }
    }

    async fn introspect(
        &self,
        token: &BearerToken<'_>,
        now: &impl Fn() -> Result<u64, VerificationError>,
    ) -> Result<VerifiedIntrospection, VerificationError> {
        now()?;
        let _permit = self.permits.try_acquire().map_err(|_| {
            VerificationError::new(Failure::Unavailable, VerificationReason::Capacity)
        })?;
        let response = self
            .provider
            .post_form_json(
                self.endpoint.url(),
                &form_encode(&self.client_id),
                &form_encode(self.client_secret.expose_secret()),
                form_body(token),
            )
            .await
            .map_err(|failure| {
                VerificationError::new(Failure::Unavailable, VerificationReason::Provider(failure))
            })?;
        validate_introspection_claims(&response, &self.policy, now()?, token.access_token())
    }
}

impl fmt::Debug for IntrospectionVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IntrospectionVerifier([REDACTED])")
    }
}

/// Fixes each entry's lifetime when it is inserted; reads never extend it.
struct Retention {
    ttl: Duration,
}

impl Expiry<[u8; 32], VerifiedIntrospection> for Retention {
    fn expire_after_create(
        &self,
        _: &[u8; 32],
        value: &VerifiedIntrospection,
        _: std::time::Instant,
    ) -> Option<Duration> {
        Some(retention(&value.principal, self.ttl, SystemTime::now()))
    }

    fn expire_after_update(
        &self,
        _: &[u8; 32],
        value: &VerifiedIntrospection,
        _: std::time::Instant,
        _: Option<Duration>,
    ) -> Option<Duration> {
        Some(retention(&value.principal, self.ttl, SystemTime::now()))
    }
}

/// The earlier of the configured TTL and token expiry, without leeway. Zero
/// means the result is returned to its waiters but not retained.
fn retention(principal: &Principal, ttl: Duration, now: SystemTime) -> Duration {
    if now.duration_since(UNIX_EPOCH).is_err() || principal.payload_len() > MAX_ENTRY_BYTES {
        return Duration::ZERO;
    }
    // An expiry beyond what `SystemTime` represents is simply far away.
    let Some(token_expiry) = UNIX_EPOCH.checked_add(Duration::from_secs(principal.expires_at()))
    else {
        return ttl;
    };
    token_expiry
        .duration_since(now)
        .map_or(Duration::ZERO, |remaining| remaining.min(ttl))
}

fn form_body(token: &BearerToken<'_>) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("token", token.as_str())
        .append_pair("token_type_hint", "access_token")
        .finish()
}

/// RFC 6749 section 2.3.1 form-encodes client credentials before Basic encoding.
fn form_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
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
    };
    use tokio_util::sync::CancellationToken;

    use super::{
        IntrospectionCacheOptions, IntrospectionOptions, IntrospectionVerifier, MAX_ENTRY_BYTES,
        form_body, form_encode, retention,
    };
    use crate::{
        EndpointUrl, Failure, IssuerUrl,
        claims::{ClaimPolicy, validate_introspection_claims},
        parse_bearer,
        provider::{ProviderClient, fixture_acceptor, new_fixture_client},
    };

    const FIXTURE_HOST: &str = "provider.test";

    struct Fixture {
        endpoint: EndpointUrl,
        provider: ProviderClient,
        calls: Arc<AtomicUsize>,
        received: Arc<Semaphore>,
        response: Arc<Mutex<Vec<u8>>>,
        gate: Arc<Mutex<Option<Arc<Semaphore>>>>,
        cancel: CancellationToken,
        task: JoinHandle<()>,
    }

    impl Fixture {
        #[allow(
            clippy::excessive_nesting,
            reason = "test fixture keeps connection ownership, bounded exchanges and abort/join teardown together"
        )]
        async fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (acceptor, root) = fixture_acceptor(FIXTURE_HOST);
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
                endpoint: EndpointUrl::parse(&format!(
                    "https://{FIXTURE_HOST}:{}/introspect",
                    address.port()
                ))
                .unwrap(),
                provider: new_fixture_client(
                    FIXTURE_HOST,
                    address,
                    &root,
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
                issuer: IssuerUrl::parse("https://issuer.example").unwrap(),
                audiences: vec!["api".to_owned()],
                endpoint: self.endpoint.clone(),
                client_id: "fixture-client".to_owned(),
                client_secret: secrecy::SecretString::from("fixture-secret"),
                provider_concurrency: NonZeroUsize::new(1).unwrap(),
                cache,
            }
        }

        fn verifier(&self, cache: Option<IntrospectionCacheOptions>) -> Arc<IntrospectionVerifier> {
            Arc::new(
                IntrospectionVerifier::new(self.options(cache), self.provider.clone()).unwrap(),
            )
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
        verifier.verify(&token).await.map_err(|error| error.failure)
    }
    async fn poll_pending<T>(mut future: Pin<&mut impl Future<Output = T>>) {
        assert!(
            poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx)))
                .await
                .is_pending()
        );
    }

    #[test]
    fn client_credentials_are_form_encoded_before_basic_authentication() {
        assert_eq!(form_encode("a:b"), "a%3Ab");
        assert_eq!(form_encode("c d"), "c+d");
    }

    #[test]
    fn form_body_keeps_the_token_out_of_the_url() {
        let token = parse_bearer([b"Bearer a+b/=".as_slice()]).unwrap();
        assert_eq!(
            form_body(&token),
            "token=a%2Bb%2F%3D&token_type_hint=access_token"
        );
    }

    #[tokio::test]
    async fn access_token_equals_the_presented_bearer_text() {
        use secrecy::ExposeSecret;
        let fixture = Fixture::new().await;
        let uncached = fixture.verifier(None);
        let principal = verify(&uncached, b"Bearer first").await.unwrap();
        assert_eq!(principal.access_token().expose_secret(), "first");
        fixture.finish().await;
    }

    #[tokio::test]
    async fn a_refusing_provider_is_counted_under_its_status_class() {
        use crate::{ProviderFailure, VerificationError, VerificationReason};
        let fixture = Fixture::new().await;
        let verifier = fixture.verifier(None);
        let token = parse_bearer([b"Bearer first".as_slice()]).unwrap();
        for (status, code, label) in [
            ("401 Unauthorized", 401, "provider_status_4xx"),
            ("503 Service Unavailable", 503, "provider_status_5xx"),
            ("204 No Content", 204, "provider_status_other"),
        ] {
            fixture.respond(status, b"");
            let error = verifier.verify(&token).await.unwrap_err();
            assert_eq!(
                error,
                VerificationError::new(
                    Failure::Unavailable,
                    VerificationReason::Provider(ProviderFailure::Status(code))
                )
            );
            assert_eq!(error.reason.label(), label);
        }
        fixture.finish().await;
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
        let permit = cached.permits.acquire().await.unwrap();
        assert_eq!(
            verify(&Arc::clone(&cached), b"Bearer first").await.unwrap(),
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
        let changed_context =
            IntrospectionVerifier::new(options, fixture.provider.clone()).unwrap();
        assert_eq!(
            verify(&changed_context, b"Bearer first").await,
            Err(Failure::Invalid)
        );
        assert_eq!(fixture.calls(), 6);
        fixture.finish().await;
    }

    // Moka keeps its own clock, so this one test waits in real time.
    #[tokio::test]
    async fn lifetime_does_not_slide_and_expired_entries_never_mask_provider_failure() {
        let fixture = Fixture::new().await;
        let options = IntrospectionCacheOptions::new(1, Duration::from_secs(2)).unwrap();
        let verifier = fixture.verifier(Some(options));
        verify(&verifier, b"Bearer first").await.unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        fixture.respond("503 Service Unavailable", b"{}");
        verify(&verifier, b"Bearer first").await.unwrap();
        assert_eq!(fixture.calls(), 1);
        tokio::time::sleep(Duration::from_millis(1000)).await;
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
        for (status, response, expected, retained) in [
            (
                "200 OK",
                active_response("subject", epoch_now() + 600),
                None,
                true,
            ),
            ("200 OK", oversized, None, false),
            (
                "200 OK",
                active_response("subject", epoch_now() - 1),
                None,
                false,
            ),
            (
                "200 OK",
                br#"{"active":false}"#.to_vec(),
                Some(Failure::Invalid),
                false,
            ),
            (
                "503 Service Unavailable",
                b"{}".to_vec(),
                Some(Failure::Unavailable),
                false,
            ),
        ] {
            fixture.respond(status, &response);
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
        let permit = verifier.permits.acquire().await.unwrap();
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
        let permit = verifier.permits.acquire().await.unwrap();
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
    async fn retained_evidence_checks_forward_expiry_and_backward_not_before() {
        let fixture = Fixture::new().await;
        let base = epoch_now();
        let token = parse_bearer([b"Bearer calendar".as_slice()]).unwrap();
        for (expiry, not_before, changed) in [
            (base + 60, None, base + 60),
            (base + 600, Some(base), base - 31),
        ] {
            let verifier = fixture.verifier(Some(cache_options(1)));
            let mut response: serde_json::Value =
                serde_json::from_slice(&active_response("old", expiry)).unwrap();
            response["nbf"] = not_before.into();
            fixture.respond("200 OK", &serde_json::to_vec(&response).unwrap());
            verifier.verify_with_now(&token, || Ok(base)).await.unwrap();
            let before = fixture.calls();
            fixture.respond("503 Service Unavailable", b"{}");
            // Still physically retained: the changed calendar alone forces provider work.
            let error = verifier
                .verify_with_now(&token, || Ok(changed))
                .await
                .unwrap_err();
            assert_eq!(error.failure, Failure::Unavailable);
            assert_eq!(fixture.calls(), before + 1);
            // Rejecting old evidence must not delete it or poison later valid clock samples.
            assert_eq!(
                verifier
                    .verify_with_now(&token, || Ok(base))
                    .await
                    .unwrap()
                    .subject(),
                Some("old")
            );
            assert_eq!(fixture.calls(), before + 1);
            fixture.respond("200 OK", &active_response("replacement", base + 900));
            assert_eq!(
                verifier
                    .verify_with_now(&token, || Ok(changed))
                    .await
                    .unwrap()
                    .subject(),
                Some("replacement")
            );
            assert_eq!(
                verifier
                    .verify_with_now(&token, || Ok(changed))
                    .await
                    .unwrap()
                    .subject(),
                Some("replacement")
            );
            assert_eq!(fixture.calls(), before + 2);
        }
        fixture.finish().await;
    }

    #[tokio::test]
    async fn joined_retained_evidence_cannot_become_fresh_or_remove_a_replacement() {
        let fixture = Fixture::new().await;
        let base = epoch_now();
        let token = parse_bearer([b"Bearer provenance".as_slice()]).unwrap();
        let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, token.as_bytes());
        let mut key = [0_u8; 32];
        key.copy_from_slice(digest.as_ref());
        for replacement in [false, true] {
            fixture.respond("200 OK", &active_response("old", base + 60));
            let verifier = fixture.verifier(Some(cache_options(1)));
            verifier.verify_with_now(&token, || Ok(base)).await.unwrap();
            let cache = verifier.cache.as_ref().unwrap();
            let old = cache.retained.get(&key).await.unwrap();
            let before = fixture.calls();
            let gate = Semaphore::new(0);
            // Hold native publication of retained evidence so a joined consumer
            // resumes after a calendar step (and optionally a newer replacement).
            let mut publication = Box::pin(cache.fills.try_get_with(key, async {
                gate.acquire().await.unwrap().forget();
                Ok::<_, crate::VerificationError>(super::Fill::Retained(old))
            }));
            poll_pending(publication.as_mut()).await;
            let mut waiter = Box::pin(verifier.verify_with_now(&token, || Ok(base + 60)));
            poll_pending(waiter.as_mut()).await;
            if replacement {
                let newer = validate_introspection_claims(
                    &active_response("new", base + 600),
                    &verifier.policy,
                    base,
                    token.access_token(),
                )
                .unwrap();
                cache.retained.insert(key, newer).await;
            }
            fixture.respond("503 Service Unavailable", b"{}");
            gate.add_permits(1);
            assert!(
                tokio::time::timeout(Duration::from_secs(5), publication)
                    .await
                    .unwrap()
                    .is_ok()
            );
            let result = tokio::time::timeout(Duration::from_secs(5), waiter)
                .await
                .unwrap();
            if replacement {
                assert_eq!(result.unwrap().subject(), Some("new"));
                assert_eq!(fixture.calls(), before);
            } else {
                // At exp the old value still fits fresh leeway, but is ineligible
                // retained evidence. An outage must not fall back to it.
                assert_eq!(result.unwrap_err().failure, Failure::Unavailable);
                assert_eq!(fixture.calls(), before + 1);
            }
        }
        fixture.finish().await;
    }

    #[tokio::test]
    async fn unusable_clock_refuses_dispatch_and_retained_trust_then_recovers() {
        let fixture = Fixture::new().await;
        fixture.respond("200 OK", &active_response("extreme", u64::MAX));
        let token = parse_bearer([b"Bearer clock".as_slice()]).unwrap();
        for options in [None, Some(cache_options(1))] {
            let verifier = fixture.verifier(options);
            let before = fixture.calls();
            let unavailable = || crate::unix_time(UNIX_EPOCH - Duration::from_secs(1));
            for warmed in [false, true] {
                if warmed {
                    verifier.verify_with_now(&token, || Ok(100)).await.unwrap();
                }
                let error = verifier
                    .verify_with_now(&token, unavailable)
                    .await
                    .unwrap_err();
                assert_eq!(
                    error,
                    crate::VerificationError::new(
                        Failure::Unavailable,
                        crate::VerificationReason::Clock
                    )
                );
                assert_eq!(fixture.calls(), before + usize::from(warmed));
            }
            assert_eq!(
                verifier
                    .verify_with_now(&token, || Ok(100))
                    .await
                    .unwrap()
                    .subject(),
                Some("extreme")
            );
        }
        fixture.finish().await;
    }

    #[tokio::test]
    async fn joined_fresh_delivery_uses_each_callers_current_lifetime() {
        let fixture = Fixture::new().await;
        let base = epoch_now();
        let token = parse_bearer([b"Bearer delayed".as_slice()]).unwrap();
        for (expiry, not_before, delivery, expected) in [
            (base + 30, None, Some(base + 60), None),
            (
                base + 30,
                None,
                Some(base + 61),
                Some((Failure::Invalid, crate::VerificationReason::Expired)),
            ),
            (base + 600, Some(base), Some(base - 30), None),
            (
                base + 600,
                Some(base),
                Some(base - 31),
                Some((Failure::Invalid, crate::VerificationReason::NotYetValid)),
            ),
            (
                base + 600,
                None,
                None,
                Some((Failure::Unavailable, crate::VerificationReason::Clock)),
            ),
        ] {
            let mut response: serde_json::Value =
                serde_json::from_slice(&active_response("fresh", expiry)).unwrap();
            response["nbf"] = not_before.into();
            fixture.respond("200 OK", &serde_json::to_vec(&response).unwrap());
            let verifier = fixture.verifier(Some(cache_options(1)));
            let gate = fixture.block_responses();
            let before = fixture.calls();
            let clock = Mutex::new(Some(base));
            let mut leader = Box::pin(verifier.verify_with_now(&token, || Ok(base)));
            tokio::select! { () = fixture.received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
            let mut waiter =
                Box::pin(
                    verifier.verify_with_now(&token, || match *clock.lock().unwrap() {
                        Some(now) => Ok(now),
                        None => crate::unix_time(UNIX_EPOCH - Duration::from_secs(1)),
                    }),
                );
            poll_pending(waiter.as_mut()).await;
            gate.add_permits(1);
            tokio::time::timeout(Duration::from_secs(5), leader)
                .await
                .unwrap()
                .unwrap();
            // The joined waiter resumes only after the leader published fresh evidence.
            *clock.lock().unwrap() = delivery;
            let result = tokio::time::timeout(Duration::from_secs(5), waiter)
                .await
                .unwrap();
            assert_eq!(
                result.err().map(|error| (error.failure, error.reason)),
                expected
            );
            assert_eq!(fixture.calls(), before + 1);
            // A caller-local delivery rejection neither retries nor invalidates retention.
            verifier.verify_with_now(&token, || Ok(base)).await.unwrap();
            assert_eq!(fixture.calls(), before + 1);
            *fixture.gate.lock().unwrap() = None;
        }
        fixture.finish().await;
    }

    // Moka's monotonic retention clock is independent of Tokio's test clock.
    #[tokio::test]
    async fn replacement_starts_its_own_fixed_retention() {
        let fixture = Fixture::new().await;
        let base = epoch_now();
        fixture.respond("200 OK", &active_response("old", base + 60));
        let verifier = fixture.verifier(Some(
            IntrospectionCacheOptions::new(1, Duration::from_secs(2)).unwrap(),
        ));
        let token = parse_bearer([b"Bearer replaced".as_slice()]).unwrap();
        verifier.verify_with_now(&token, || Ok(base)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        fixture.respond("200 OK", &active_response("new", base + 600));
        verifier
            .verify_with_now(&token, || Ok(base + 60))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1000)).await;
        fixture.respond("503 Service Unavailable", b"{}");
        assert_eq!(
            verifier
                .verify_with_now(&token, || Ok(base + 60))
                .await
                .unwrap()
                .subject(),
            Some("new")
        );
        assert_eq!(fixture.calls(), 2);
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
    fn retention_ends_at_the_earlier_of_ttl_and_token_expiry() {
        let policy =
            ClaimPolicy::new("https://issuer.example".to_owned(), vec!["api".to_owned()]).unwrap();
        let principal = |custom: &str| {
            let response = serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":131,"sub":"subject","custom":custom});
            validate_introspection_claims(
                response.to_string().as_bytes(),
                &policy,
                100,
                secrecy::SecretString::from("presented-token"),
            )
            .unwrap()
            .principal
        };
        let small = principal("");
        let at = |millis| UNIX_EPOCH + Duration::from_millis(millis);
        let ttl = Duration::from_secs(60);
        assert_eq!(
            retention(&small, Duration::from_secs(5), at(100_500)),
            Duration::from_secs(5)
        );
        assert_eq!(
            retention(&small, ttl, at(100_500)),
            Duration::from_millis(30_500)
        );
        assert_eq!(retention(&small, ttl, at(131_000)), Duration::ZERO);
        assert_eq!(retention(&small, ttl, at(140_000)), Duration::ZERO);
        assert_eq!(
            retention(&small, ttl, UNIX_EPOCH - Duration::from_secs(1)),
            Duration::ZERO
        );
        let unbounded = validate_introspection_claims(
            format!(r#"{{"active":true,"iss":"https://issuer.example","aud":"api","exp":{},"sub":"subject"}}"#, u64::MAX).as_bytes(),
            &policy,
            100,
            secrecy::SecretString::from("presented-token"),
        )
        .unwrap().principal;
        assert_eq!(retention(&unbounded, ttl, at(100_500)), ttl);
        for (length, expected) in [
            (MAX_ENTRY_BYTES, Duration::from_millis(30_500)),
            (MAX_ENTRY_BYTES + 1, Duration::ZERO),
        ] {
            let prefix = r#"{"active":true,"iss":"https://issuer.example","aud":"api","exp":131,"sub":"subject","custom":""#;
            let response = format!("{prefix}{}\"}}", "x".repeat(length - prefix.len() - 2));
            let padded = format!(" \t\r\n{response}\n\r\t ");
            let principal = validate_introspection_claims(
                padded.as_bytes(),
                &policy,
                100,
                secrecy::SecretString::from("presented-token"),
            )
            .unwrap()
            .principal;
            assert_eq!(principal.payload_len(), length);
            assert_eq!(retention(&principal, ttl, at(100_500)), expected);
        }
    }
}
