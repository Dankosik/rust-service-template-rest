use std::{
    mem::DropGuard,
    num::NonZeroUsize,
    sync::{Arc, OnceLock, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use foldhash::HashMap;
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use moka::{Expiry, sync::Cache};
use operation_context::{OperationContext, Stopped};
use parking_lot::Mutex;
use secrecy::ExposeSecret;
use shared_rules::oidc_introspection::{CACHE_CAPACITY, CACHE_TTL};
use tokio::sync::Semaphore;

use crate::{
    InvalidCacheOptions, PreparationError, PresentedToken, Principal, ProviderFailure,
    VerificationError, VerificationReason, Verifier,
    claims::{ClaimPolicy, VerifiedClaims, validate_introspection_claims},
    provider::{CallerStop, ProviderClient},
};

const MAX_RETAINED_PAYLOAD_BYTES: usize = 64 * 1024;

impl From<ProviderFailure> for VerificationError {
    fn from(failure: ProviderFailure) -> Self {
        Self::unavailable(VerificationReason::Provider(failure.reason()))
    }
}

#[derive(Clone, derive_more::Debug)]
#[debug("IntrospectionOptions([REDACTED])")]
pub struct IntrospectionOptions {
    pub issuer: crate::IssuerUrl,
    pub audiences: Vec<String>,
    pub endpoint: crate::EndpointUrl,
    pub client_id: String,
    pub client_secret: secrecy::SecretString,
    pub provider_concurrency: NonZeroUsize,
    pub cache: Option<IntrospectionCacheOptions>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntrospectionCacheOptions {
    capacity: usize,
    ttl: Duration,
}

impl IntrospectionCacheOptions {
    pub fn try_new(capacity: usize, ttl: Duration) -> Result<Self, InvalidCacheOptions> {
        if !CACHE_CAPACITY.contains(&capacity) || !CACHE_TTL.contains(&ttl) {
            return Err(InvalidCacheOptions);
        }
        Ok(Self { capacity, ttl })
    }
}

pub fn prepare_introspection(options: IntrospectionOptions) -> Result<Verifier, PreparationError> {
    IntrospectionVerifier::try_new(options, ProviderClient::try_new()?).map(Verifier::introspection)
}

#[cfg(any(test, feature = "test-support"))]
pub fn prepare_introspection_with_fixture(
    options: IntrospectionOptions,
    fixture: crate::test_support::FixtureTransport,
) -> Result<Verifier, PreparationError> {
    IntrospectionVerifier::try_new(options, fixture.into_provider()).map(Verifier::introspection)
}

#[derive(derive_more::Debug)]
#[debug("IntrospectionVerifier([REDACTED])")]
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
    retained: Cache<TokenDigest, VerifiedClaims, foldhash::quality::RandomState>,
    fills: Mutex<HashMap<TokenDigest, Weak<RunningFill>>>,
}

struct RunningFill {
    shared: Shared<FillFuture>,
    last_waiter_stop: LastWaiterStop,
}

type LastWaiterStop = Arc<OnceLock<Stopped>>;

type TokenDigest = [u8; 32];
type FillResult = Result<Fill, VerificationError>;
type FillFuture = BoxFuture<'static, FillResult>;

impl PositiveCache {
    async fn join_fill<F>(
        &self,
        key: TokenDigest,
        context: &OperationContext,
        start: impl FnOnce(LastWaiterStop) -> F,
    ) -> FillResult
    where
        F: Future<Output = FillResult> + Send + 'static,
    {
        let running = {
            let mut fills = self.fills.lock();
            fills.get(&key).and_then(Weak::upgrade).unwrap_or_else(|| {
                let last_waiter_stop = LastWaiterStop::default();
                let running = Arc::new(RunningFill {
                    shared: start(Arc::clone(&last_waiter_stop)).boxed().shared(),
                    last_waiter_stop,
                });
                fills.insert(key, Arc::downgrade(&running));
                running
            })
        };
        let registration = Arc::downgrade(&running);
        let retire = || {
            let mut fills = self.fills.lock();
            if fills
                .get(&key)
                .is_some_and(|fill| fill.ptr_eq(&registration))
            {
                fills.remove(&key);
            }
        };
        let waiter = DropGuard::new(running, |running| {
            if let Some(last) = Arc::into_inner(running) {
                if let Some(reason) = context.stopped() {
                    let _ = last.last_waiter_stop.set(reason);
                }
                retire();
            }
        });
        let result = waiter.shared.clone().await;
        retire();
        drop(DropGuard::dismiss(waiter));
        result
    }
}

fn token_digest(token: &PresentedToken<'_>) -> TokenDigest {
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, token.as_bytes());
    let mut key = [0_u8; 32];
    key.copy_from_slice(digest.as_ref());
    key
}

#[derive(Clone)]
enum Fill {
    Fresh(VerifiedClaims),
    RecheckCache,
}

impl IntrospectionVerifier {
    fn try_new(
        options: IntrospectionOptions,
        provider: ProviderClient,
    ) -> Result<Arc<Self>, PreparationError> {
        let policy = ClaimPolicy::try_new(options.issuer.as_str(), options.audiences)?;
        if options.provider_concurrency.get() > Semaphore::MAX_PERMITS {
            return Err(PreparationError::invalid_options());
        }
        Ok(Arc::new(Self {
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
                    .build_with_hasher(foldhash::quality::RandomState::default()),
                fills: Mutex::default(),
            }),
        }))
    }

    pub(crate) async fn verify(
        self: &Arc<Self>,
        token: &PresentedToken<'_>,
        context: &OperationContext,
    ) -> Result<Principal, VerificationError> {
        self.verify_with_now(token, context, crate::unix_now).await
    }

    async fn verify_with_now(
        self: &Arc<Self>,
        token: &PresentedToken<'_>,
        context: &OperationContext,
        unix_now: impl Fn() -> Result<Duration, VerificationError> + Clone + Send + Sync + 'static,
    ) -> Result<Principal, VerificationError> {
        context.check().map_err(|_| ProviderFailure::Timeout)?;
        let Some(cache) = &self.cache else {
            return self
                .introspect(token.as_str(), CallerStop::Context(context), &unix_now)
                .await?
                .into_principal_checked_at(unix_now()?);
        };
        let key = token_digest(token);
        loop {
            if let Some(value) = cache.retained.get(&key)
                && value.is_reusable_at(unix_now()?)
            {
                return Ok(value.principal);
            }
            let filled = cache
                .join_fill(key, context, |last_waiter_stop| {
                    let verifier = Arc::clone(self);
                    let retained = cache.retained.clone();
                    let unix_now = unix_now.clone();
                    let token = token.to_secret();
                    async move {
                        if let Some(value) = retained.get(&key)
                            && value.is_reusable_at(unix_now()?)
                        {
                            return Ok(Fill::RecheckCache);
                        }
                        let value = verifier
                            .introspect(
                                token.expose_secret(),
                                CallerStop::Latch(&last_waiter_stop),
                                &unix_now,
                            )
                            .await?;
                        retained.insert(key, value.clone());
                        Ok(Fill::Fresh(value))
                    }
                })
                .await?;
            if let Fill::Fresh(value) = filled {
                return value.into_principal_checked_at(unix_now()?);
            }
        }
    }

    async fn introspect(
        &self,
        token: &str,
        caller_stop: CallerStop<'_>,
        unix_now: &impl Fn() -> Result<Duration, VerificationError>,
    ) -> Result<VerifiedClaims, VerificationError> {
        unix_now()?;
        let _permit = self
            .permits
            .try_acquire()
            .map_err(|_| VerificationError::unavailable(VerificationReason::Capacity))?;
        let response = self
            .provider
            .post_introspection_form_json(
                self.endpoint.url(),
                &self.client_id,
                self.client_secret.expose_secret(),
                &[("token", token), ("token_type_hint", "access_token")],
                caller_stop,
            )
            .await?;
        validate_introspection_claims(&response, &self.policy, unix_now()?)
    }
}

struct Retention {
    ttl: Duration,
}

impl Expiry<TokenDigest, VerifiedClaims> for Retention {
    fn expire_after_create(
        &self,
        _: &TokenDigest,
        value: &VerifiedClaims,
        _: std::time::Instant,
    ) -> Option<Duration> {
        Some(retention(&value.principal, self.ttl, SystemTime::now()))
    }

    fn expire_after_update(
        &self,
        _: &TokenDigest,
        value: &VerifiedClaims,
        _: std::time::Instant,
        _: Option<Duration>,
    ) -> Option<Duration> {
        Some(retention(&value.principal, self.ttl, SystemTime::now()))
    }
}

fn retention(principal: &Principal, ttl: Duration, now: SystemTime) -> Duration {
    match now.duration_since(UNIX_EPOCH) {
        Ok(now_from_epoch) if principal.payload_len() <= MAX_RETAINED_PAYLOAD_BYTES => principal
            .expiry_from_epoch()
            .saturating_sub(now_from_epoch)
            .min(ttl),
        _ => Duration::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::{HeaderMap, Method, Request, Response, StatusCode, Uri, header};
    use http_body_util::{BodyExt as _, Full};
    use hyper::{body::Incoming, server::conn::http1, service::service_fn};
    use hyper_util::rt::TokioIo;
    use std::{
        num::NonZeroUsize,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, UNIX_EPOCH},
    };
    use tokio::{
        net::TcpListener,
        sync::Semaphore,
        task::{JoinHandle, JoinSet},
    };
    use tokio_util::sync::CancellationToken;

    use super::{
        IntrospectionCacheOptions, IntrospectionOptions, IntrospectionVerifier,
        MAX_RETAINED_PAYLOAD_BYTES, retention,
    };
    use crate::{
        EndpointUrl, Failure, IssuerUrl, PreparationError,
        claims::{ClaimPolicy, validate_introspection_claims},
        parse_bearer,
        provider::{ProviderClient, fixture_acceptor, new_fixture_client},
        test_metrics::{Diagnostics, labeled},
    };
    use operation_context::{OperationContext, Stopped};

    const FIXTURE_HOST: &str = "provider.test";

    fn unix_seconds() -> u64 {
        crate::unix_now().unwrap().as_secs()
    }

    #[derive(Clone, Debug)]
    struct RecordedRequest {
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    }

    #[derive(Clone)]
    struct FixtureState {
        calls: Arc<AtomicUsize>,
        received: Arc<Semaphore>,
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        response: Arc<Mutex<(StatusCode, Bytes)>>,
        gate: Arc<Mutex<Option<Arc<Semaphore>>>>,
    }

    async fn serve_fixture_connection(
        stream: tokio::net::TcpStream,
        acceptor: tokio_rustls::TlsAcceptor,
        state: FixtureState,
    ) {
        tokio::time::timeout(Duration::from_secs(5), async move {
            let stream = acceptor.accept(stream).await.unwrap();
            let service = service_fn(move |request: Request<Incoming>| {
                let state = state.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let body = body.collect().await?.to_bytes();
                    assert!(body.len() <= 128 * 1024);
                    state.requests.lock().unwrap().push(RecordedRequest {
                        method: parts.method,
                        uri: parts.uri,
                        headers: parts.headers,
                        body,
                    });
                    let (status, response_body) = state.response.lock().unwrap().clone();
                    let gate = state.gate.lock().unwrap().clone();
                    state.calls.fetch_add(1, Ordering::SeqCst);
                    state.received.add_permits(1);
                    if let Some(gate) = gate {
                        gate.acquire().await.unwrap().forget();
                    }
                    Ok::<_, hyper::Error>(
                        Response::builder()
                            .status(status)
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Full::new(response_body))
                            .unwrap(),
                    )
                }
            });
            let _ = http1::Builder::new()
                .keep_alive(false)
                .serve_connection(TokioIo::new(stream), service)
                .await;
        })
        .await
        .unwrap();
    }

    async fn run_fixture_server(
        listener: TcpListener,
        acceptor: tokio_rustls::TlsAcceptor,
        state: FixtureState,
        cancel: CancellationToken,
    ) {
        let mut connections = JoinSet::new();
        loop {
            let stream = tokio::select! {
                () = cancel.cancelled() => break,
                Some(result) = connections.join_next(), if !connections.is_empty() => { result.unwrap(); continue; },
                accepted = listener.accept() => accepted.unwrap().0,
            };
            connections.spawn(serve_fixture_connection(
                stream,
                acceptor.clone(),
                state.clone(),
            ));
        }
        connections.abort_all();
        while let Some(result) = connections.join_next().await {
            if let Err(error) = result {
                assert!(error.is_cancelled());
            }
        }
    }

    struct Fixture {
        endpoint: EndpointUrl,
        provider: ProviderClient,
        state: FixtureState,
        cancel: CancellationToken,
        task: JoinHandle<()>,
    }

    impl Fixture {
        async fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (acceptor, root) = fixture_acceptor(FIXTURE_HOST);
            let state = FixtureState {
                calls: Arc::new(AtomicUsize::new(0)),
                received: Arc::new(Semaphore::new(0)),
                requests: Arc::new(Mutex::new(Vec::new())),
                response: Arc::new(Mutex::new((
                    StatusCode::OK,
                    Bytes::from(active_response("subject", unix_seconds() + 600)),
                ))),
                gate: Arc::new(Mutex::new(None)),
            };
            let cancel = CancellationToken::new();
            let task = tokio::spawn(run_fixture_server(
                listener,
                acceptor,
                state.clone(),
                cancel.clone(),
            ));
            Self {
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
                state,
                cancel,
                task,
            }
        }

        fn respond(&self, status: StatusCode, body: &[u8]) {
            *self.state.response.lock().unwrap() = (status, Bytes::copy_from_slice(body));
        }

        fn requests(&self) -> Vec<RecordedRequest> {
            self.state.requests.lock().unwrap().clone()
        }

        fn block_responses(&self) -> Arc<Semaphore> {
            let gate = Arc::new(Semaphore::new(0));
            *self.state.gate.lock().unwrap() = Some(gate.clone());
            gate
        }

        async fn received(&self) {
            tokio::time::timeout(Duration::from_secs(5), self.state.received.acquire())
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
                provider_concurrency: NonZeroUsize::MIN,
                cache,
            }
        }

        fn verifier(&self, cache: Option<IntrospectionCacheOptions>) -> Arc<IntrospectionVerifier> {
            IntrospectionVerifier::try_new(self.options(cache), self.provider.clone()).unwrap()
        }

        fn prepared(&self, cache: Option<IntrospectionCacheOptions>) -> crate::Verifier {
            crate::Verifier::introspection(self.verifier(cache))
        }

        fn calls(&self) -> usize {
            self.state.calls.load(Ordering::SeqCst)
        }

        async fn finish(self) {
            self.cancel.cancel();
            tokio::time::timeout(Duration::from_secs(5), self.task)
                .await
                .unwrap()
                .unwrap();
        }
    }

    fn active_response(subject: &str, expiry: u64) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":expiry,"sub":subject,"scope":"read write"})).unwrap()
    }
    fn cache_options(capacity: usize) -> IntrospectionCacheOptions {
        IntrospectionCacheOptions::try_new(capacity, Duration::from_secs(30)).unwrap()
    }
    async fn verify(
        verifier: &Arc<IntrospectionVerifier>,
        value: &[u8],
    ) -> Result<crate::Principal, Failure> {
        let token = parse_bearer([value]).unwrap();
        verifier
            .verify(&token, &OperationContext::unbounded())
            .await
            .map_err(|error| error.failure)
    }

    #[tokio::test]
    async fn the_request_form_encodes_client_credentials_and_sends_the_token_only_in_the_body() {
        let fixture = Fixture::new().await;
        let mut options = fixture.options(None);
        options.client_id = "a:b".to_owned();
        options.client_secret = secrecy::SecretString::from("c d");
        let verifier = IntrospectionVerifier::try_new(options, fixture.provider.clone()).unwrap();
        verify(&verifier, b"Bearer a+b/=").await.unwrap();
        let requests = fixture.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.method, Method::POST);
        assert_eq!(request.uri, "/introspect");
        assert_eq!(
            request.headers.get(header::CONTENT_TYPE).unwrap(),
            "application/x-www-form-urlencoded"
        );
        assert_eq!(
            request.headers.get(header::AUTHORIZATION).unwrap(),
            "Basic YSUzQWI6Yytk"
        );
        assert!(!format!("{:?}", request.headers).contains("a+b"));
        assert!(!format!("{:?}", request.headers).contains("a%2Bb"));
        assert_eq!(
            request.body.as_ref(),
            b"token=a%2Bb%2F%3D&token_type_hint=access_token"
        );
        fixture.finish().await;
    }

    #[tokio::test]
    async fn stopped_callers_never_dispatch_introspection_or_receive_cached_success() {
        let fixture = Fixture::new().await;
        let token = parse_bearer([b"Bearer first".as_slice()]).unwrap();
        for cache in [None, Some(cache_options(1))] {
            let verifier = fixture.prepared(cache);
            let cancelled = OperationContext::unbounded();
            cancelled.cancel();
            for context in [OperationContext::with_timeout(Duration::ZERO), cancelled] {
                assert_eq!(
                    verifier.verify_with_context(&token, &context).await.err(),
                    Some(Failure::Unavailable)
                );
            }
        }
        assert_eq!(fixture.calls(), 0);
        let verifier = fixture.prepared(Some(cache_options(1)));
        verifier.verify(&token).await.unwrap();
        assert_eq!(
            verifier
                .verify_with_context(&token, &OperationContext::with_timeout(Duration::ZERO))
                .await
                .err(),
            Some(Failure::Unavailable)
        );
        assert_eq!(fixture.calls(), 1);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn cancellation_during_envelope_parsing_is_not_credential_evidence() {
        let fixture = Fixture::new().await;
        let verifier = fixture.prepared(None);
        for header in [b"Bearer =".as_slice(), b"Bearer first".as_slice()] {
            let context = OperationContext::unbounded();
            let authorization = std::iter::once_with(|| {
                context.cancel();
                header
            });
            assert_eq!(
                verifier
                    .authenticate(&context, authorization, crate::Transport::Http)
                    .await
                    .err(),
                Some(Failure::Unavailable)
            );
        }
        assert_eq!(fixture.calls(), 0);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn provider_concurrency_above_the_semaphore_limit_is_an_invalid_option() {
        let fixture = Fixture::new().await;
        let mut options = fixture.options(None);
        options.provider_concurrency = const {
            NonZeroUsize::new(Semaphore::MAX_PERMITS + 1).expect("one past the limit is nonzero")
        };
        let Err(error) = IntrospectionVerifier::try_new(options, fixture.provider.clone()) else {
            panic!("an over-limit concurrency was accepted");
        };
        assert_eq!(
            error,
            PreparationError::Options(crate::OptionsError::InvalidOption)
        );
        fixture.finish().await;
    }

    #[tokio::test]
    async fn a_blank_audience_is_an_invalid_option_as_in_configuration() {
        let fixture = Fixture::new().await;
        let mut options = fixture.options(None);
        options.audiences = vec![" \t".to_owned()];
        let Err(error) = IntrospectionVerifier::try_new(options, fixture.provider.clone()) else {
            panic!("a blank audience was accepted");
        };
        assert_eq!(
            error,
            PreparationError::Options(crate::OptionsError::InvalidOption)
        );
        fixture.finish().await;
    }

    #[tokio::test]
    async fn uncached_introspection_expires_at_the_parent_cutoff_and_releases_capacity() {
        let fixture = Fixture::new().await;
        let verifier = fixture.prepared(None);
        let gate = fixture.block_responses();
        let token = parse_bearer([b"Bearer first".as_slice()]).unwrap();
        let context = OperationContext::with_timeout(Duration::from_secs(2));
        let mut call = Box::pin(verifier.verify_with_context(&token, &context));
        tokio::select! {
            () = fixture.received() => {},
            result = &mut call => panic!("response must be gated: {result:?}"),
        }
        tokio::time::pause();
        tokio::time::advance(context.remaining().unwrap()).await;
        assert_eq!(call.as_mut().await.err(), Some(Failure::Unavailable));
        tokio::time::resume();
        drop(call);
        gate.add_permits(1);
        *fixture.state.gate.lock().unwrap() = None;
        verifier.verify(&token).await.unwrap();
        assert_eq!(fixture.calls(), 2);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn a_refusing_provider_is_counted_under_its_status_class() {
        use crate::{ProviderFailure, VerificationError, VerificationReason};
        let fixture = Fixture::new().await;
        let verifier = fixture.verifier(None);
        let token = parse_bearer([b"Bearer first".as_slice()]).unwrap();
        for (status, label) in [
            (StatusCode::UNAUTHORIZED, "provider_status_4xx"),
            (StatusCode::SERVICE_UNAVAILABLE, "provider_status_5xx"),
            (StatusCode::NO_CONTENT, "provider_status_other"),
        ] {
            fixture.respond(status, b"");
            let error = verifier
                .verify(&token, &OperationContext::unbounded())
                .await
                .unwrap_err();
            assert_eq!(
                error,
                VerificationError::unavailable(VerificationReason::Provider(
                    ProviderFailure::Status(status.as_u16()).reason()
                ))
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
            verify(&Arc::clone(&cached), b"Bearer first")
                .await
                .unwrap()
                .fields(),
            principal.fields()
        );
        assert_eq!(fixture.calls(), 3);
        assert_eq!(
            verify(&cached, b"Bearer second").await.err(),
            Some(Failure::Unavailable)
        );
        drop(permit);
        verify(&cached, b"Bearer second").await.unwrap();
        assert_eq!(fixture.calls(), 4);
        let independent = fixture.verifier(Some(cache_options(2)));
        fixture.respond(
            StatusCode::OK,
            &active_response("new-subject", unix_seconds() + 600),
        );
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
            IntrospectionVerifier::try_new(options, fixture.provider.clone()).unwrap();
        assert_eq!(
            verify(&changed_context, b"Bearer first").await.err(),
            Some(Failure::Invalid)
        );
        assert_eq!(fixture.calls(), 6);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn lifetime_does_not_slide_and_expired_entries_never_mask_provider_failure() {
        let fixture = Fixture::new().await;
        let options = IntrospectionCacheOptions::try_new(1, Duration::from_secs(2)).unwrap();
        let verifier = fixture.verifier(Some(options));
        verify(&verifier, b"Bearer first").await.unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        fixture.respond(StatusCode::SERVICE_UNAVAILABLE, b"{}");
        verify(&verifier, b"Bearer first").await.unwrap();
        assert_eq!(fixture.calls(), 1);
        tokio::time::sleep(Duration::from_millis(1000)).await;
        for _ in 0..2 {
            assert_eq!(
                verify(&verifier, b"Bearer first").await.err(),
                Some(Failure::Unavailable)
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
            (active_response("subject", unix_seconds() - 1), None),
        ] {
            fixture.respond(StatusCode::OK, &response);
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
        let oversized = serde_json::to_vec(&serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":unix_seconds()+600,"sub":"subject","custom":"x".repeat(64*1024)})).unwrap();
        for (status, response, expected, retained) in [
            (
                StatusCode::OK,
                active_response("subject", unix_seconds() + 600),
                None,
                true,
            ),
            (StatusCode::OK, oversized, None, false),
            (
                StatusCode::OK,
                active_response("subject", unix_seconds() - 1),
                None,
                false,
            ),
            (
                StatusCode::OK,
                br#"{"active":false}"#.to_vec(),
                Some(Failure::Invalid),
                false,
            ),
            (
                StatusCode::SERVICE_UNAVAILABLE,
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
            assert!(futures_util::poll!(second.as_mut()).is_pending());
            assert_eq!(
                verify(&verifier, b"Bearer distinct").await.err(),
                Some(Failure::Unavailable)
            );
            gate.add_permits(1);
            let (first, second) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(first, second)
            })
            .await
            .unwrap();
            assert_eq!(first.as_ref().err().copied(), expected);
            assert_eq!(
                first.as_ref().map(crate::Principal::fields),
                second.as_ref().map(crate::Principal::fields)
            );
            assert_eq!(fixture.calls(), before + 1);
            *fixture.state.gate.lock().unwrap() = None;
            assert_eq!(verify(&verifier, b"Bearer shared").await.err(), expected);
            assert_eq!(fixture.calls(), before + if retained { 1 } else { 2 });
            if !retained {
                fixture.received().await;
            }
            if expected.is_some() {
                fixture.respond(
                    StatusCode::OK,
                    &active_response("recovered", unix_seconds() + 600),
                );
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
    async fn cancelling_initializer_preserves_the_surviving_requests_exchange() {
        let fixture = Fixture::new().await;
        let verifier = fixture.prepared(Some(cache_options(2)));
        let gate = fixture.block_responses();
        let token = parse_bearer([b"Bearer shared".as_slice()]).unwrap();
        let context = OperationContext::with_timeout(Duration::from_secs(3));
        let mut leader = Box::pin(verifier.verify_with_context(&token, &context));
        tokio::select! { () = fixture.received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
        let mut survivor = Box::pin(verifier.verify(&token));
        assert!(futures_util::poll!(survivor.as_mut()).is_pending());
        context.cancel();
        assert_eq!(leader.as_mut().await.err(), Some(Failure::Unavailable));
        drop(leader);
        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), survivor)
            .await
            .unwrap()
            .unwrap();
        verifier.verify(&token).await.unwrap();
        assert_eq!(fixture.calls(), 1);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn caller_deadline_and_cancellation_remain_distinct_when_authentication_stops() {
        let diagnostics = Diagnostics::default();
        let _recorder = metrics::set_default_local_recorder(diagnostics.recorder());
        let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let _dispatch = tracing::dispatcher::set_default(&diagnostics.dispatch());
        for (cache, drop_on_stop, stopped, inbound, provider) in [
            (None, false, Stopped::Deadline, "timeout", "caller_deadline"),
            (None, false, Stopped::Cancelled, "cancelled", "cancelled"),
            (None, true, Stopped::Deadline, "timeout", "caller_deadline"),
            (None, true, Stopped::Cancelled, "cancelled", "cancelled"),
            (
                Some(cache_options(1)),
                false,
                Stopped::Deadline,
                "timeout",
                "caller_deadline",
            ),
            (
                Some(cache_options(1)),
                true,
                Stopped::Deadline,
                "timeout",
                "caller_deadline",
            ),
        ] {
            let fixture = Fixture::new().await;
            let verifier = fixture.prepared(cache);
            let _gate = fixture.block_responses();
            let budget = Duration::from_secs(2);
            let context = OperationContext::with_timeout(budget);
            let mut authentication = Box::pin(verifier.authenticate(
                &context,
                [b"Bearer slow".as_slice()],
                crate::Transport::Http,
            ));
            tokio::select! {
                () = fixture.received() => {},
                result = &mut authentication => panic!("response must be gated: {result:?}"),
            }
            tokio::time::pause();
            match stopped {
                Stopped::Deadline => tokio::time::advance(budget).await,
                Stopped::Cancelled => context.cancel(),
            }
            if drop_on_stop {
                tokio::select! {
                    biased;
                    reason = context.wait_stopped() => assert_eq!(reason, stopped),
                    result = authentication => panic!("transport must stop first: {result:?}"),
                }
            } else {
                assert_eq!(authentication.await.err(), Some(Failure::Unavailable));
            }
            tokio::time::resume();

            let counters = diagnostics.take_counters();
            let [(key, 1)] = counters.as_slice() else {
                panic!("expected one inbound authentication outcome, got {counters:?}");
            };
            assert_eq!(key.name(), "authn_verifications_total");
            assert!(labeled(key, &[("result", inbound), ("failure", inbound)]));
            let samples = diagnostics.take_histograms();
            let [sample] = samples.as_slice() else {
                panic!("expected one provider histogram sample, got {samples:?}");
            };
            assert_eq!(sample.name(), "authn_provider_request_duration_seconds");
            assert!(labeled(sample, &[("outcome", provider)]));
            let spans = diagnostics.take_spans();
            assert!(spans.iter().any(|span| {
                span.contains("authn.operation=\"introspection\";")
                    && span.contains(&format!("error.type=\"{provider}\";"))
            }));
            fixture.finish().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn only_the_last_waiter_supplies_the_shared_exchanges_stop_reason() {
        let cache = super::PositiveCache {
            retained: moka::sync::Cache::builder()
                .build_with_hasher(foldhash::quality::RandomState::default()),
            fills: parking_lot::Mutex::default(),
        };
        let cancelled = OperationContext::unbounded();
        let budget = Duration::from_secs(1);
        let deadline = OperationContext::with_timeout(budget);
        let (report, mut observed) = tokio::sync::oneshot::channel();
        let mut provider_stop = None;
        let mut first = Box::pin(cache.join_fill([0; 32], &cancelled, |stop| {
            provider_stop = Some(Arc::clone(&stop));
            async move {
                let _exchange = std::mem::DropGuard::new(report, |report| {
                    report.send(stop.get().copied()).unwrap();
                });
                std::future::pending().await
            }
        }));
        assert!(futures_util::poll!(first.as_mut()).is_pending());
        let mut last = Box::pin(cache.join_fill([0; 32], &deadline, |_| async {
            panic!("started a second provider exchange")
        }));
        assert!(futures_util::poll!(last.as_mut()).is_pending());

        cancelled.cancel();
        drop(first);
        assert_eq!(provider_stop.unwrap().get(), None);
        tokio::time::advance(budget).await;
        drop(last);
        assert_eq!(observed.try_recv().unwrap(), Some(Stopped::Deadline));
    }

    #[tokio::test]
    async fn provider_deadline_is_distinct_from_caller_cancellation_in_exchange_telemetry() {
        let diagnostics = Diagnostics::default();
        let _recorder = metrics::set_default_local_recorder(diagnostics.recorder());
        let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let _dispatch = tracing::dispatcher::set_default(&diagnostics.dispatch());
        let token = parse_bearer([b"Bearer slow".as_slice()]).unwrap();
        for cache in [None, Some(cache_options(1))] {
            let fixture = Fixture::new().await;
            let verifier = fixture.prepared(cache);
            let _gate = fixture.block_responses();
            for (cancel, expected) in [(true, "cancelled"), (false, "provider_timeout")] {
                let context = OperationContext::unbounded();
                let mut verification = Box::pin(verifier.verify_with_context(&token, &context));
                tokio::select! {
                    () = fixture.received() => {},
                    result = &mut verification => panic!("response must be gated: {result:?}"),
                }
                tokio::time::pause();
                if cancel {
                    context.cancel();
                } else {
                    tokio::time::advance(Duration::from_secs(3)).await;
                }
                assert_eq!(
                    tokio::time::timeout(Duration::from_millis(100), verification)
                        .await
                        .unwrap()
                        .err(),
                    Some(Failure::Unavailable)
                );
                tokio::time::resume();
                let samples = diagnostics.take_histograms();
                assert_eq!(samples.len(), 1);
                assert_eq!(samples[0].name(), "authn_provider_request_duration_seconds");
                assert!(labeled(&samples[0], &[("outcome", expected)]));
                let spans = diagnostics.take_spans();
                assert!(spans.iter().any(|span| {
                    span.contains("authn.operation=\"introspection\";")
                        && span.contains(&format!("error.type=\"{expected}\";"))
                }));
            }
            fixture.finish().await;
        }
    }

    #[tokio::test]
    async fn a_cancelled_waiter_preserves_the_original_fill_and_its_live_hit() {
        let fixture = Fixture::new().await;
        let verifier = fixture.verifier(Some(cache_options(1)));
        let gate = fixture.block_responses();
        let mut leader = Box::pin(verify(&verifier, b"Bearer shared"));
        tokio::select! { () = fixture.received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
        let mut waiter = Box::pin(verify(&verifier, b"Bearer shared"));
        assert!(futures_util::poll!(waiter.as_mut()).is_pending());
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
    async fn an_expired_waiter_preserves_the_original_fill_and_its_live_hit() {
        let fixture = Fixture::new().await;
        let verifier = fixture.prepared(Some(cache_options(1)));
        let gate = fixture.block_responses();
        let token = parse_bearer([b"Bearer shared".as_slice()]).unwrap();
        let mut leader = Box::pin(verifier.verify(&token));
        tokio::select! { () = fixture.received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
        tokio::time::pause();
        let context = OperationContext::with_timeout(Duration::from_millis(10));
        let mut waiter = Box::pin(verifier.verify_with_context(&token, &context));
        assert!(futures_util::poll!(waiter.as_mut()).is_pending());
        tokio::time::advance(Duration::from_millis(11)).await;
        assert_eq!(waiter.as_mut().await.err(), Some(Failure::Unavailable));
        tokio::time::resume();
        drop(waiter);
        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), leader)
            .await
            .unwrap()
            .unwrap();
        verifier.verify(&token).await.unwrap();
        assert_eq!(fixture.calls(), 1);
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
        let context = OperationContext::unbounded();
        let fixture = Fixture::new().await;
        let base = unix_seconds();
        let token = parse_bearer([b"Bearer calendar".as_slice()]).unwrap();
        for (expiry, not_before, changed) in [
            (base + 60, None, base + 60),
            (base + 600, Some(base), base - 31),
        ] {
            let verifier = fixture.verifier(Some(cache_options(1)));
            let mut response: serde_json::Value =
                serde_json::from_slice(&active_response("old", expiry)).unwrap();
            response["nbf"] = not_before.into();
            fixture.respond(StatusCode::OK, &serde_json::to_vec(&response).unwrap());
            verifier
                .verify_with_now(&token, &context, move || Ok(Duration::from_secs(base)))
                .await
                .unwrap();
            let before = fixture.calls();
            fixture.respond(StatusCode::SERVICE_UNAVAILABLE, b"{}");
            let error = verifier
                .verify_with_now(&token, &context, move || Ok(Duration::from_secs(changed)))
                .await
                .unwrap_err();
            assert_eq!(error.failure, Failure::Unavailable);
            assert_eq!(fixture.calls(), before + 1);
            assert_eq!(
                verifier
                    .verify_with_now(&token, &context, move || Ok(Duration::from_secs(base)))
                    .await
                    .unwrap()
                    .subject(),
                Some("old")
            );
            assert_eq!(fixture.calls(), before + 1);
            fixture.respond(StatusCode::OK, &active_response("replacement", base + 900));
            assert_eq!(
                verifier
                    .verify_with_now(&token, &context, move || Ok(Duration::from_secs(changed)))
                    .await
                    .unwrap()
                    .subject(),
                Some("replacement")
            );
            assert_eq!(
                verifier
                    .verify_with_now(&token, &context, move || Ok(Duration::from_secs(changed)))
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
    async fn joined_cache_hit_rechecks_the_current_entry() {
        let context = OperationContext::unbounded();
        let fixture = Fixture::new().await;
        let base = unix_seconds();
        let token = parse_bearer([b"Bearer provenance".as_slice()]).unwrap();
        let key = super::token_digest(&token);
        for (elapsed_seconds, replacement) in [(0, false), (60, false), (60, true)] {
            fixture.respond(StatusCode::OK, &active_response("old", base + 60));
            let verifier = fixture.verifier(Some(cache_options(1)));
            verifier
                .verify_with_now(&token, &context, move || Ok(Duration::from_secs(base)))
                .await
                .unwrap();
            let cache = verifier.cache.as_ref().unwrap();
            let before = fixture.calls();
            let gate = Arc::new(Semaphore::new(0));
            let publication_gate = gate.clone();
            let publication = cache.join_fill(key, &context, |_| async move {
                publication_gate.acquire().await.unwrap().forget();
                Ok(super::Fill::RecheckCache)
            });
            let mut publication = Box::pin(publication);
            assert!(futures_util::poll!(publication.as_mut()).is_pending());
            if elapsed_seconds == 0 {
                cache.retained.invalidate(&key);
            }
            let mut waiter = Box::pin(verifier.verify_with_now(&token, &context, move || {
                Ok(Duration::from_secs(base + elapsed_seconds))
            }));
            assert!(futures_util::poll!(waiter.as_mut()).is_pending());
            if replacement {
                let newer = validate_introspection_claims(
                    &active_response("new", base + 600),
                    &verifier.policy,
                    Duration::from_secs(base),
                )
                .unwrap();
                cache.retained.insert(key, newer);
            }
            fixture.respond(StatusCode::SERVICE_UNAVAILABLE, b"{}");
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
                assert_eq!(result.unwrap_err().failure, Failure::Unavailable);
                assert_eq!(fixture.calls(), before + 1);
            }
        }
        fixture.finish().await;
    }

    #[tokio::test]
    async fn unusable_clock_refuses_dispatch_and_retained_trust_then_recovers() {
        let context = OperationContext::unbounded();
        let fixture = Fixture::new().await;
        fixture.respond(StatusCode::OK, &active_response("extreme", u64::MAX));
        let token = parse_bearer([b"Bearer clock".as_slice()]).unwrap();
        for options in [None, Some(cache_options(1))] {
            let verifier = fixture.verifier(options);
            let before = fixture.calls();
            let unavailable = || crate::unix_time(UNIX_EPOCH - Duration::from_secs(1));
            for warmed in [false, true] {
                if warmed {
                    verifier
                        .verify_with_now(&token, &context, move || Ok(Duration::from_secs(100)))
                        .await
                        .unwrap();
                }
                let error = verifier
                    .verify_with_now(&token, &context, unavailable)
                    .await
                    .unwrap_err();
                assert_eq!(
                    error,
                    crate::VerificationError::unavailable(crate::VerificationReason::Clock)
                );
                assert_eq!(fixture.calls(), before + usize::from(warmed));
            }
            assert_eq!(
                verifier
                    .verify_with_now(&token, &context, move || Ok(Duration::from_secs(100)))
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
        let context = OperationContext::unbounded();
        let fixture = Fixture::new().await;
        let base = unix_seconds();
        let token = parse_bearer([b"Bearer delayed".as_slice()]).unwrap();
        for (expiry, not_before, delivery, expected) in [
            (base + 30, None, Some(base + 59), None),
            (
                base + 30,
                None,
                Some(base + 60),
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
            fixture.respond(StatusCode::OK, &serde_json::to_vec(&response).unwrap());
            let verifier = fixture.verifier(Some(cache_options(1)));
            let gate = fixture.block_responses();
            let before = fixture.calls();
            let clock = Arc::new(Mutex::new(Some(base)));
            let mut leader = Box::pin(
                verifier.verify_with_now(&token, &context, move || Ok(Duration::from_secs(base))),
            );
            tokio::select! { () = fixture.received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
            let waiter_clock = clock.clone();
            let mut waiter = Box::pin(verifier.verify_with_now(&token, &context, move || {
                let now = *waiter_clock.lock().unwrap();
                match now {
                    Some(now) => Ok(Duration::from_secs(now)),
                    None => crate::unix_time(UNIX_EPOCH - Duration::from_secs(1)),
                }
            }));
            assert!(futures_util::poll!(waiter.as_mut()).is_pending());
            gate.add_permits(1);
            tokio::time::timeout(Duration::from_secs(5), leader)
                .await
                .unwrap()
                .unwrap();
            *clock.lock().unwrap() = delivery;
            let result = tokio::time::timeout(Duration::from_secs(5), waiter)
                .await
                .unwrap();
            assert_eq!(
                result.err().map(|error| (error.failure, error.reason)),
                expected
            );
            assert_eq!(fixture.calls(), before + 1);
            verifier
                .verify_with_now(&token, &context, move || Ok(Duration::from_secs(base)))
                .await
                .unwrap();
            assert_eq!(fixture.calls(), before + 1);
            *fixture.state.gate.lock().unwrap() = None;
        }
        fixture.finish().await;
    }

    #[tokio::test]
    async fn replacement_starts_its_own_fixed_retention() {
        let context = OperationContext::unbounded();
        let fixture = Fixture::new().await;
        let base = unix_seconds();
        fixture.respond(StatusCode::OK, &active_response("old", base + 60));
        let verifier = fixture.verifier(Some(
            IntrospectionCacheOptions::try_new(1, Duration::from_secs(2)).unwrap(),
        ));
        let token = parse_bearer([b"Bearer replaced".as_slice()]).unwrap();
        verifier
            .verify_with_now(&token, &context, move || Ok(Duration::from_secs(base)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        fixture.respond(StatusCode::OK, &active_response("new", base + 600));
        verifier
            .verify_with_now(&token, &context, move || Ok(Duration::from_secs(base + 60)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1000)).await;
        fixture.respond(StatusCode::SERVICE_UNAVAILABLE, b"{}");
        assert_eq!(
            verifier
                .verify_with_now(&token, &context, move || {
                    Ok(Duration::from_secs(base + 60))
                })
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
            let error = IntrospectionCacheOptions::try_new(capacity, ttl).unwrap_err();
            assert_eq!(error, crate::InvalidCacheOptions);
            assert_eq!(
                PreparationError::from(error).phase(),
                crate::PreparationPhase::Options
            );
        }
        for (capacity, ttl) in [(1, Duration::from_secs(1)), (1024, Duration::from_mins(5))] {
            let options = IntrospectionCacheOptions::try_new(capacity, ttl).unwrap();
            assert_eq!(options.capacity, capacity);
            assert_eq!(options.ttl, ttl);
        }
    }

    #[test]
    fn retention_ends_at_the_earlier_of_ttl_and_token_expiry() {
        let policy =
            ClaimPolicy::try_new("https://issuer.example", vec!["api".to_owned()]).unwrap();
        let identity = |custom: &str| {
            let response = serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":131,"sub":"subject","custom":custom});
            validate_introspection_claims(
                response.to_string().as_bytes(),
                &policy,
                Duration::from_secs(100),
            )
            .unwrap()
            .principal
        };
        let small = identity("");
        let at = |millis| UNIX_EPOCH + Duration::from_millis(millis);
        let ttl = Duration::from_mins(1);
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
            Duration::from_secs(100),
        )
        .unwrap().principal;
        assert_eq!(retention(&unbounded, ttl, at(100_500)), ttl);
        for (length, expected) in [
            (MAX_RETAINED_PAYLOAD_BYTES, Duration::from_millis(30_500)),
            (MAX_RETAINED_PAYLOAD_BYTES + 1, Duration::ZERO),
        ] {
            let prefix = r#"{"active":true,"iss":"https://issuer.example","aud":"api","exp":131,"sub":"subject","custom":""#;
            let response = format!("{prefix}{}\"}}", "x".repeat(length - prefix.len() - 2));
            let padded = format!(" \t\r\n{response}\n\r\t ");
            let identity =
                validate_introspection_claims(padded.as_bytes(), &policy, Duration::from_secs(100))
                    .unwrap()
                    .principal;
            assert_eq!(identity.payload_len(), length);
            assert_eq!(retention(&identity, ttl, at(100_500)), expected);
        }
    }
}
