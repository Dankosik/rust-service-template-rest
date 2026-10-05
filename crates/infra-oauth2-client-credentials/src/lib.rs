//! Private `OAuth2` client credentials for bounded outbound integrations.
//!
//! Composition prepares one immutable credential owner and binds it to a
//! resource client. Neither access tokens, assertions, nor raw provider
//! errors leave it. The owner authenticates to its authorization server only
//! with a signed client assertion (RFC 7523), never a shared secret, and can
//! exchange a verified inbound token (RFC 8693) for one addressed to an
//! integration.

use std::{
    fmt,
    future::Future,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use http::{
    HeaderMap, HeaderValue, Request, Response, StatusCode,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
};
use infra_outbound_http::{Client, Limits};
use moka::{Expiry, ops::compute::Op};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::{
    sync::{Semaphore, SemaphorePermit, mpsc, oneshot},
    time::Instant,
};
use url::Url;
use uuid::Uuid;

#[cfg(test)]
mod tests;

// template:begin outbound-auth-grpc:oauth-grpc-module
#[cfg(feature = "grpc")]
pub mod grpc;
// template:end outbound-auth-grpc:oauth-grpc-module

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const FAILURE_WINDOW: Duration = Duration::from_secs(1);
const EXCHANGE_CACHE_BYTES: u32 = 16 * 1024 * 1024;
/// A token stops being reused this long before it expires, as in Go's `oauth2`.
const REUSE_MARGIN: Duration = Duration::from_secs(10);
/// A reusable token is replaced in the background once at most this much, or a
/// quarter, of its reuse remains, as Azure.Core refreshes five minutes early.
const REFRESH_AHEAD: Duration = Duration::from_mins(5);
/// The minimum spacing before a subsequent background refresh attempt.
const REFRESH_RETRY: Duration = Duration::from_secs(30);
/// A resource 401 evicts only a token at least this old. The provider would
/// answer a younger one with the same token, so a resource that refuses every
/// token costs one token request per this period, not one per call.
const EVICTION_MIN_AGE: Duration = Duration::from_secs(30);
const TOKEN_LIMITS: Limits = Limits {
    operation_timeout: FETCH_TIMEOUT,
    response_header_count: 64,
    response_body_bytes: 1024 * 1024,
};
/// Best-effort retained subject count, combined with the payload byte target.
const EXCHANGE_CACHE_CAPACITY: std::ops::RangeInclusive<u32> = 1..=65_536;
/// RFC 7523 section 2.2 and the OIDF client-assertion notice: one string
/// audience, a fresh `jti`, and an assertion signed for at most this long.
const ASSERTION_LIFETIME_SECS: u64 = 60;
/// An assertion is dated this far back, as Go's `oauth2/jws` does, so a
/// provider whose clock is slightly behind does not see it as issued in the
/// future or expiring too late.
const ASSERTION_BACKDATE_SECS: u64 = 10;
const ASSERTION_TYP: &str = "client-authentication+jwt";
const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";
const GRANT_CLIENT_CREDENTIALS: &str = "client_credentials";
const GRANT_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const TOKEN_TYPE_ACCESS_TOKEN: &str = "urn:ietf:params:oauth:token-type:access_token";
/// The `grant` metric label for token exchange; shorter than the URN the
/// form itself sends as `grant_type`.
const METRIC_GRANT_TOKEN_EXCHANGE: &str = "token_exchange";

/// The client-assertion signing algorithm. One algorithm per key (RFC 8725bis
/// section 3.1), so it is always stated and never defaulted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Algorithm {
    Rs256,
    Ps256,
    Es256,
}

impl From<Algorithm> for jsonwebtoken::Algorithm {
    fn from(value: Algorithm) -> Self {
        match value {
            Algorithm::Rs256 => Self::RS256,
            Algorithm::Ps256 => Self::PS256,
            Algorithm::Es256 => Self::ES256,
        }
    }
}

/// Immutable composition input. Secrets come from the configuration snapshot.
#[derive(Clone)]
pub struct Options {
    pub token_url: String,
    pub client_id: String,
    pub private_key: SecretString,
    pub key_id: String,
    pub algorithm: Algorithm,
    pub assertion_audience: String,
    pub scopes: Vec<String>,
    pub audience: Option<String>,
    /// Best-effort retained subject count, alongside a 16 MiB Bearer payload target.
    pub exchange_cache_capacity: u32,
    /// Maximum simultaneous token attempts shared by both grants. Must be positive.
    pub provider_concurrency: u32,
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Options([REDACTED])")
    }
}

/// Safe admission failure identifying only the option and its static cause.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("OAuth2 {key}: {reason}")]
pub struct ConfigurationError {
    pub key: &'static str,
    pub reason: &'static str,
}

/// Closed acquisition outcomes. Raw protocol and transport errors are discarded.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AcquisitionError {
    #[error("OAuth2 acquisition deadline elapsed")]
    Timeout,
    #[error("OAuth2 token transport failed")]
    Transport,
    #[error("OAuth2 token response exceeded its limit")]
    ResponseLimit,
    #[error("OAuth2 provider is unavailable")]
    Unavailable,
    #[error("OAuth2 provider acquisition capacity is exhausted")]
    AtCapacity,
    #[error("OAuth2 provider rejected the request: {0}")]
    Rejected(Rejection),
    #[error("OAuth2 token response is invalid")]
    InvalidResponse,
    #[error("OAuth2 client assertion could not be signed")]
    Assertion,
}

impl AcquisitionError {
    fn label(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::ResponseLimit => "limit",
            Self::Unavailable => "unavailable",
            Self::AtCapacity => "capacity",
            Self::Rejected(rejection) => rejection.label(),
            Self::InvalidResponse => "invalid",
            Self::Assertion => "assertion",
        }
    }
}

/// Why the provider refused a token request: its `error` code (RFC 6749
/// section 5.2, RFC 8693 section 2.2.2) when it is one of the registered
/// ones. The code is the only part of the provider's answer that is kept.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rejection {
    InvalidRequest,
    InvalidClient,
    InvalidGrant,
    UnauthorizedClient,
    UnsupportedGrantType,
    InvalidScope,
    InvalidTarget,
    /// No `error` code, or one outside the registered set.
    Other,
}

impl Rejection {
    fn from_body(body: &[u8]) -> Self {
        #[derive(serde::Deserialize)]
        struct ErrorResponse {
            error: String,
        }
        let Ok(response) = serde_json::from_slice::<ErrorResponse>(body) else {
            return Self::Other;
        };
        match response.error.as_str() {
            "invalid_request" => Self::InvalidRequest,
            "invalid_client" => Self::InvalidClient,
            "invalid_grant" => Self::InvalidGrant,
            "unauthorized_client" => Self::UnauthorizedClient,
            "unsupported_grant_type" => Self::UnsupportedGrantType,
            "invalid_scope" => Self::InvalidScope,
            "invalid_target" => Self::InvalidTarget,
            _ => Self::Other,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidClient => "invalid_client",
            Self::InvalidGrant => "invalid_grant",
            Self::UnauthorizedClient => "unauthorized_client",
            Self::UnsupportedGrantType => "unsupported_grant_type",
            Self::InvalidScope => "invalid_scope",
            Self::InvalidTarget => "invalid_target",
            Self::Other => "rejected",
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Other => "unregistered error code",
            registered => registered.label(),
        })
    }
}

/// Authentication failures are distinct from the existing resource transport.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("caller Authorization header conflicts with OAuth2 authentication")]
    AuthorizationConflict,
    #[error("request carries no subject to act on behalf of")]
    SubjectRequired,
    #[error(transparent)]
    Acquisition(#[from] AcquisitionError),
    #[error(transparent)]
    Resource(#[from] infra_outbound_http::Error),
}

/// The verified inbound access token a caller acts on behalf of (RFC 8693).
/// Attached to a request's extensions, it replaces the service token with one
/// exchanged for the subject it names.
#[derive(Clone)]
pub struct OnBehalfOf(SecretString);

impl OnBehalfOf {
    #[must_use]
    pub fn new(subject_token: SecretString) -> Self {
        Self(subject_token)
    }
}

impl fmt::Debug for OnBehalfOf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OnBehalfOf([REDACTED])")
    }
}

/// An idle, cloneable owner of one private credential tuple and its tokens.
#[derive(Clone)]
pub struct Credentials(Arc<Owner>);

struct Owner {
    inner: Arc<Inner>,
    refresh: mpsc::Sender<RefreshRequest>,
    // Dropping the final external owner closes this channel. The driver never
    // owns Owner, so its own Inner reference cannot keep credentials alive.
    _lifetime: oneshot::Sender<()>,
}

struct RefreshRequest {
    current: Arc<Token>,
    deadline: Instant,
}

/// The refresh lifetime owned by the integration's composition root.
/// Drive [`Self::run`] while credentials are used and await its completion before
/// dropping dependencies. Dropping this driver closes all surviving clients.
#[must_use = "drive and await RefreshDriver::run for the integration lifetime"]
pub struct RefreshDriver {
    inner: Arc<Inner>,
    requests: mpsc::Receiver<RefreshRequest>,
    owner_gone: oneshot::Receiver<()>,
}

impl fmt::Debug for RefreshDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RefreshDriver([REDACTED])")
    }
}

impl RefreshDriver {
    /// Runs refreshes inline until shutdown or final external-owner release.
    /// This method spawns no tasks; completion observes all owned refresh work.
    pub async fn run(mut self, shutdown: impl Future<Output = ()>) {
        tokio::pin!(shutdown);
        loop {
            let request = tokio::select! {
                biased;
                () = &mut shutdown => break,
                _ = &mut self.owner_gone => break,
                request = self.requests.recv() => {
                    let Some(request) = request else { break };
                    request
                }
            };
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                _ = &mut self.owner_gone => break,
                () = self.inner.refresh_ahead(request) => {}
            }
            self.inner.cached().refresh_pending = false;
        }
        self.requests.close();
    }
}

impl Drop for RefreshDriver {
    fn drop(&mut self) {
        self.requests.close();
        self.inner.cached().refresh_pending = false;
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials([REDACTED])")
    }
}

struct Inner {
    token_endpoint: Url,
    token_http: Client,
    scopes: Vec<String>,
    audience: Option<String>,
    signer: Signer,
    /// The last service token; never held across an `.await`.
    cached: Mutex<Cached>,
    /// Held by the one caller requesting a new service token, so requests
    /// for it never overlap.
    refresh: tokio::sync::Mutex<()>,
    permits: Semaphore,
    /// Tokens exchanged on behalf of a verified subject, keyed by the SHA-256
    /// digest of its access token. Moka lets concurrent misses for one
    /// subject share a single exchange.
    exchanged: moka::future::Cache<[u8; 32], Arc<Token>>,
}

/// Builds and signs client assertions. Construction proves the key matches
/// its algorithm; every subsequent token request signs a fresh one.
struct Signer {
    key: jsonwebtoken::EncodingKey,
    header: jsonwebtoken::Header,
    client_id: String,
    assertion_audience: String,
}

impl Signer {
    fn new(options: &Options) -> Result<Self, ConfigurationError> {
        let invalid_key = ConfigurationError {
            key: "private_key",
            reason: "must be a PEM key matching its configured algorithm",
        };
        let pem = options.private_key.expose_secret().as_bytes();
        let key = match options.algorithm {
            Algorithm::Rs256 | Algorithm::Ps256 => jsonwebtoken::EncodingKey::from_rsa_pem(pem),
            Algorithm::Es256 => jsonwebtoken::EncodingKey::from_ec_pem(pem),
        }
        .map_err(|_| invalid_key)?;
        let mut header = jsonwebtoken::Header::new(options.algorithm.into());
        header.kid = Some(options.key_id.clone());
        header.typ = Some(ASSERTION_TYP.to_owned());
        let signer = Self {
            key,
            header,
            client_id: options.client_id.clone(),
            assertion_audience: options.assertion_audience.clone(),
        };
        // Proves the key matches its algorithm without performing any I/O.
        signer.sign().map_err(|_| invalid_key)?;
        Ok(signer)
    }

    fn form_fields<'a>(&'a self, assertion: &'a str) -> [(&'static str, &'a str); 3] {
        [
            ("client_id", self.client_id.as_str()),
            ("client_assertion_type", CLIENT_ASSERTION_TYPE),
            ("client_assertion", assertion),
        ]
    }

    /// Signs one fresh assertion. Never cached: RFC 7523bis requires a
    /// single-use `jti` and every shortlisted server enforces it.
    fn sign(&self) -> Result<String, AcquisitionError> {
        let issued = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AcquisitionError::Assertion)?
            .as_secs()
            .saturating_sub(ASSERTION_BACKDATE_SECS);
        let claims = AssertionClaims {
            iss: &self.client_id,
            sub: &self.client_id,
            aud: &self.assertion_audience,
            iat: issued,
            nbf: issued,
            exp: issued + ASSERTION_LIFETIME_SECS,
            jti: Uuid::new_v4().to_string(),
        };
        jsonwebtoken::encode(&self.header, &claims, &self.key)
            .map_err(|_| AcquisitionError::Assertion)
    }
}

#[derive(serde::Serialize)]
struct AssertionClaims<'a> {
    iss: &'a str,
    sub: &'a str,
    aud: &'a str,
    iat: u64,
    nbf: u64,
    exp: u64,
    jti: String,
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: Option<u64>,
    issued_token_type: Option<String>,
}

#[derive(Default)]
struct Cached {
    token: Option<Arc<Token>>,
    /// When the next background refresh of a still reusable token may start.
    refresh_after: Option<Instant>,
    failure: Option<CompletedFailure>,
    refresh_pending: bool,
}

struct CompletedFailure {
    error: AcquisitionError,
    until: Instant,
}

/// A sensitive `Bearer` header value and the instant it stops being reused.
struct Token {
    header: HeaderValue,
    /// When its token request started.
    acquired: Instant,
    /// `None` when the provider gave no lifetime: use only for its requesting call.
    reuse_until: Option<Instant>,
    /// When a background refresh first replaces this token. Unused by an
    /// exchanged token, which has no background refresh.
    refresh_after: Option<Instant>,
}

impl Token {
    fn is_reusable(&self, now: Instant) -> bool {
        self.reuse_until.is_some_and(|until| now < until)
    }

    /// Whether a resource 401 may evict it; see [`EVICTION_MIN_AGE`].
    fn is_evictable(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.acquired) >= EVICTION_MIN_AGE
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token([REDACTED])")
    }
}

/// Which cache a dispatched token came from, so a resource 401 evicts the
/// right entry without a second lookup.
enum Acquired {
    Service(Arc<Token>),
    Exchanged { key: [u8; 32], token: Arc<Token> },
}

impl Acquired {
    fn token(&self) -> &Arc<Token> {
        match self {
            Self::Service(token) | Self::Exchanged { token, .. } => token,
        }
    }
}

/// Reclaims memory only. A cache hit is used only when
/// [`Token::is_reusable`] agrees against the Tokio clock, so this Expiry may
/// freely use Moka's own real-time clock, unlike a decision that must move
/// with `tokio::time::advance`.
struct ExchangedExpiry;

impl Expiry<[u8; 32], Arc<Token>> for ExchangedExpiry {
    fn expire_after_create(
        &self,
        _key: &[u8; 32],
        token: &Arc<Token>,
        _current_time: std::time::Instant,
    ) -> Option<Duration> {
        // Reclamation only; initializer freshness and explicit reuse checks
        // decide whether each caller may use the returned token.
        Some(token.reuse_until.map_or(Duration::ZERO, |until| {
            until.saturating_duration_since(Instant::now())
        }))
    }
}

impl Credentials {
    /// Prepares credentials without performing DNS, token, or resource I/O.
    /// The integration must drive and await the returned [`RefreshDriver`].
    /// Dropping or completing that driver closes any surviving clients.
    ///
    /// # Errors
    /// Returns a sanitized option or transport-construction failure.
    pub fn prepare(options: Options) -> Result<(Self, RefreshDriver), ConfigurationError> {
        let endpoint = admit_options(&options)?;
        let token_http = Client::new(&endpoint, TOKEN_LIMITS).map_err(|_| ConfigurationError {
            key: "token_url",
            reason: "transport construction failed",
        })?;
        Self::build(options, &endpoint, token_http)
    }

    fn build(
        options: Options,
        endpoint: &Url,
        token_http: Client,
    ) -> Result<(Self, RefreshDriver), ConfigurationError> {
        let permits = admitted_provider_concurrency(options.provider_concurrency)?;

        let signer = Signer::new(&options)?;
        metrics::describe_counter!(
            "oauth2_token_acquisitions_total",
            "OAuth2 token attempts by grant and closed terminal outcome"
        );
        let minimum_weight = EXCHANGE_CACHE_BYTES.div_ceil(options.exchange_cache_capacity);
        let inner = Arc::new(Inner {
            token_endpoint: endpoint.clone(),
            token_http,
            scopes: options.scopes,
            audience: options.audience,
            signer,
            cached: Mutex::default(),
            refresh: tokio::sync::Mutex::new(()),
            permits: Semaphore::new(permits),
            exchanged: moka::future::Cache::builder()
                .max_capacity(u64::from(EXCHANGE_CACHE_BYTES))
                .weigher(move |_: &[u8; 32], token: &Arc<Token>| {
                    u32::try_from(token.header.as_bytes().len())
                        .unwrap_or(u32::MAX)
                        .max(minimum_weight)
                })
                .expire_after(ExchangedExpiry)
                .build(),
        });
        let (refresh, requests) = mpsc::channel(1);
        let (lifetime, owner_gone) = oneshot::channel();
        Ok((
            Self(Arc::new(Owner {
                inner: inner.clone(),
                refresh,
                _lifetime: lifetime,
            })),
            RefreshDriver {
                inner,
                requests,
                owner_gone,
            },
        ))
    }

    /// Rejects closed owners before transport-local composition refusals.
    fn check_lifecycle(&self, deadline: Instant) -> Result<(), AcquisitionError> {
        if self.0.refresh.is_closed() {
            return Err(if Instant::now() >= deadline {
                AcquisitionError::Timeout
            } else {
                AcquisitionError::Unavailable
            });
        }
        Ok(())
    }

    fn check_admission(&self, deadline: Instant) -> Result<(), AcquisitionError> {
        if Instant::now() >= deadline {
            return Err(AcquisitionError::Timeout);
        }
        self.check_lifecycle(deadline)
    }

    /// Binds these credentials to the concrete provider's bounded HTTP client.
    #[must_use]
    pub fn http(&self, resource: Client) -> AuthenticatedClient {
        AuthenticatedClient {
            credentials: self.clone(),
            resource,
            subject_required: false,
        }
    }

    /// Inserts a sensitive Bearer header for the service token or, when
    /// `on_behalf_of` is set, a token exchanged for its subject. Returns what
    /// was used, so a resource 401 can [`reject`](Self::reject_acquired)
    /// exactly that token.
    async fn authorize(
        &self,
        headers: &mut HeaderMap,
        on_behalf_of: Option<OnBehalfOf>,
        deadline: Instant,
    ) -> Result<Acquired, AcquisitionError> {
        self.check_admission(deadline)?;
        let acquired = match on_behalf_of {
            Some(OnBehalfOf(subject)) => {
                let key = subject_key(subject.expose_secret().as_bytes());
                let token = self.exchange(key, &subject, deadline).await?;
                Acquired::Exchanged { key, token }
            }
            None => Acquired::Service(self.service_token(deadline).await?),
        };
        headers.insert(AUTHORIZATION, acquired.token().header.clone());
        Ok(acquired)
    }

    /// Returns the cached service token while it is reusable, otherwise
    /// requests a new one. Waiting for another caller's request spends this
    /// caller's deadline.
    async fn service_token(&self, deadline: Instant) -> Result<Arc<Token>, AcquisitionError> {
        self.check_admission(deadline)?;
        let now = Instant::now();
        if let Some(token) = self.reusable_service_token(now) {
            return Ok(token);
        }
        // Boxed so that the reuse path above keeps a small future.
        Box::pin(self.acquire_service_token(deadline)).await
    }

    async fn acquire_service_token(
        &self,
        deadline: Instant,
    ) -> Result<Arc<Token>, AcquisitionError> {
        let inner = &self.0.inner;
        let _refresh = tokio::time::timeout_at(deadline, inner.refresh.lock())
            .await
            .map_err(|_| AcquisitionError::Timeout)?;
        self.check_admission(deadline)?;
        let now = Instant::now();
        if let Some(token) = self.reusable_service_token(now) {
            return Ok(token);
        }
        if let Some(error) = inner.completed_failure(now) {
            return Err(error);
        }
        let full_deadline = now + FETCH_TIMEOUT;
        let shortened = deadline < full_deadline;
        let result = inner.fetch_service_token(deadline.min(full_deadline)).await;
        if shortened && Instant::now() >= deadline {
            return Err(AcquisitionError::Timeout);
        }
        match result {
            Ok(token) => Ok(inner.store_service_token(token)),
            Err(error) => {
                if error != AcquisitionError::Timeout || !shortened {
                    inner.remember_failure(error);
                }
                Err(error)
            }
        }
    }

    /// A cache hit may enqueue one bounded refresh without waiting for it.
    fn reusable_service_token(&self, now: Instant) -> Option<Arc<Token>> {
        let mut cached = self.cached();
        let token = cached
            .token
            .as_ref()
            .filter(|token| token.is_reusable(now))?
            .clone();
        if cached.refresh_after.is_some_and(|after| now >= after)
            && !cached.refresh_pending
            && cached
                .failure
                .as_ref()
                .is_none_or(|failure| now >= failure.until)
        {
            cached.refresh_after = Some(now + refresh_retry());
            cached.refresh_pending = true;
            if self
                .0
                .refresh
                .try_send(RefreshRequest {
                    current: token.clone(),
                    deadline: now + FETCH_TIMEOUT,
                })
                .is_err()
            {
                cached.refresh_pending = false;
            }
        }
        Some(token)
    }

    /// Forgets the service token `used` unless it is too young to evict or a
    /// newer token already replaced it.
    fn reject_service_token(&self, used: &Arc<Token>) {
        if !used.is_evictable(Instant::now()) {
            return;
        }
        let mut cached = self.cached();
        if cached
            .token
            .as_ref()
            .is_some_and(|token| Arc::ptr_eq(token, used))
        {
            cached.token = None;
            cached.refresh_after = None;
        }
    }

    fn cached(&self) -> MutexGuard<'_, Cached> {
        self.0.inner.cached()
    }

    /// Forgets whichever token `acquired` used, from whichever cache it came
    /// from, unless it is too young to evict or a newer token already
    /// replaced it there.
    async fn reject_acquired(&self, acquired: &Acquired) {
        match acquired {
            Acquired::Service(token) => self.reject_service_token(token),
            Acquired::Exchanged { key, token } => self.reject_exchanged(*key, token).await,
        }
    }

    async fn reject_exchanged(&self, key: [u8; 32], used: &Arc<Token>) {
        if used.is_evictable(Instant::now()) {
            self.forget_exchanged(key, used).await;
        }
    }

    /// Removes `used` only while it is still the entry the cache returns for
    /// `key`, so a token another caller stored meanwhile survives. Moka runs
    /// such steps for one key one at a time and each completes without I/O,
    /// so this wait needs no deadline.
    async fn forget_exchanged(&self, key: [u8; 32], used: &Arc<Token>) {
        self.0
            .inner
            .exchanged
            .entry(key)
            .and_compute_with(|current| {
                let unchanged = current.is_some_and(|entry| Arc::ptr_eq(entry.value(), used));
                std::future::ready(if unchanged { Op::Remove } else { Op::Nop })
            })
            .await;
    }

    /// Returns a live exchanged token for `subject`, exchanging it when the
    /// cache has none. Moka coalesces concurrent misses for one subject into
    /// the exchange of the first caller, bounded by that caller's `deadline`
    /// and [`FETCH_TIMEOUT`]. When that caller is dropped or out of budget,
    /// its exchange is cancelled and a waiting caller starts its own; a
    /// waiter never fails because of another caller's deadline. Exchanges
    /// for different subjects run concurrently.
    async fn exchange(
        &self,
        key: [u8; 32],
        subject: &SecretString,
        deadline: Instant,
    ) -> Result<Arc<Token>, AcquisitionError> {
        loop {
            self.check_admission(deadline)?;
            let (token, fresh) = self.exchanged_or_fetch(key, subject, deadline).await?;
            // Only the initializer may use a newly acquired request-only token.
            if fresh || token.is_reusable(Instant::now()) {
                return Ok(token);
            }
            self.forget_exchanged(key, &token).await;
        }
    }

    /// Returns the cached or newly exchanged token and whether this call
    /// inserted it.
    async fn exchanged_or_fetch(
        &self,
        key: [u8; 32],
        subject: &SecretString,
        deadline: Instant,
    ) -> Result<(Arc<Token>, bool), AcquisitionError> {
        let fetch_deadline = deadline.min(Instant::now() + FETCH_TIMEOUT);
        let attempt = self
            .0
            .inner
            .exchanged
            .entry(key)
            .or_try_insert_with(self.0.inner.fetch_exchange(subject, fetch_deadline));
        tokio::select! {
            // Drop this caller's initializer before it can publish its own
            // deadline as a shared failure; a surviving waiter may take over.
            biased;
            () = tokio::time::sleep_until(deadline) => Err(AcquisitionError::Timeout),
            result = attempt => match result {
                Ok(entry) => {
                    let fresh = entry.is_fresh();
                    Ok((entry.into_value(), fresh))
                }
                Err(error) => Err(*error),
            },

        }
    }
}

/// A bounded resource client with private machine authentication.
#[derive(Clone)]
pub struct AuthenticatedClient {
    credentials: Credentials,
    resource: Client,
    subject_required: bool,
}

impl fmt::Debug for AuthenticatedClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthenticatedClient([REDACTED])")
    }
}

impl AuthenticatedClient {
    /// Refuses a request without [`OnBehalfOf`] instead of sending the service
    /// token. For an integration that only ever acts for a verified user, so
    /// that a forgotten subject is an error and never a call made with the
    /// service's own authority.
    #[must_use]
    pub fn require_on_behalf_of(mut self) -> Self {
        self.subject_required = true;
        self
    }

    /// Acquires credentials, injects Bearer, and spends the original deadline.
    /// A request carrying [`OnBehalfOf`] in its extensions is sent with a
    /// token exchanged for that subject instead of the service token; the
    /// extension is always removed before dispatch. Completed responses,
    /// including 401 and 403, are returned without replay; a 401 evicts the
    /// token it used so the next call acquires anew.
    ///
    /// # Errors
    /// Rejects caller Authorization, or a missing subject this client
    /// [requires](Self::require_on_behalf_of), before I/O, acquisition failure
    /// before resource dispatch, and otherwise preserves bounded resource
    /// errors.
    pub async fn execute(
        &self,
        mut request: Request<Bytes>,
        deadline: Instant,
    ) -> Result<Response<Bytes>, Error> {
        self.credentials.check_lifecycle(deadline)?;
        if request.headers().contains_key(AUTHORIZATION) {
            return Err(Error::AuthorizationConflict);
        }
        let on_behalf_of = request.extensions_mut().remove::<OnBehalfOf>();
        if self.subject_required && on_behalf_of.is_none() {
            return Err(Error::SubjectRequired);
        }
        let acquired = self
            .credentials
            .authorize(request.headers_mut(), on_behalf_of, deadline)
            .await?;
        let response = self.resource.execute(request, deadline).await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            self.credentials.reject_acquired(&acquired).await;
        }
        Ok(response)
    }
}

impl Inner {
    fn cached(&self) -> MutexGuard<'_, Cached> {
        self.cached.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn completed_failure(&self, now: Instant) -> Option<AcquisitionError> {
        self.cached()
            .failure
            .as_ref()
            .filter(|failure| now < failure.until)
            .map(|failure| failure.error)
    }

    fn remember_failure(&self, error: AcquisitionError) {
        // Local capacity refusal is not a completed provider failure.
        if error == AcquisitionError::AtCapacity {
            return;
        }
        self.cached().failure = Some(CompletedFailure {
            error,
            until: Instant::now() + FAILURE_WINDOW,
        });
    }

    fn store_service_token(&self, token: Token) -> Arc<Token> {
        let token = Arc::new(token);
        let mut cached = self.cached();
        cached.failure = None;
        if token.is_reusable(Instant::now()) {
            cached.refresh_after = token.refresh_after;
            cached.token = Some(token.clone());
        }
        token
    }

    async fn refresh_ahead(&self, request: RefreshRequest) {
        let Ok(_refresh) = tokio::time::timeout_at(request.deadline, self.refresh.lock()).await
        else {
            return;
        };
        let now = Instant::now();
        if now >= request.deadline || self.completed_failure(now).is_some() {
            return;
        }
        if !self
            .cached()
            .token
            .as_ref()
            .is_some_and(|token| Arc::ptr_eq(token, &request.current))
        {
            return;
        }
        match self.fetch_service_token(request.deadline).await {
            Ok(token) => {
                self.store_service_token(token);
                let retry = Instant::now() + refresh_retry();
                let mut cached = self.cached();
                cached.refresh_after = cached.refresh_after.map(|after| after.max(retry));
            }
            Err(error) => {
                self.remember_failure(error);
                tracing::warn!(
                    server.address = self.token_endpoint.host_str(),
                    error.type = error.label(),
                    "oauth2_background_refresh_failed"
                );
            }
        }
    }

    fn admit_attempt(&self, deadline: Instant) -> Result<SemaphorePermit<'_>, AcquisitionError> {
        if Instant::now() >= deadline {
            return Err(AcquisitionError::Timeout);
        }
        self.permits
            .try_acquire()
            .map_err(|_| AcquisitionError::AtCapacity)
    }

    /// Performs one client-credentials token request, bounded by the
    /// caller's deadline and [`FETCH_TIMEOUT`], and records its outcome.
    async fn fetch_service_token(&self, deadline: Instant) -> Result<Token, AcquisitionError> {
        let started = Instant::now();
        let mut metric = AttemptMetric::new(GRANT_CLIENT_CREDENTIALS, deadline);
        let result = tokio::time::timeout_at(deadline, async {
            let _permit = self.admit_attempt(deadline)?;
            self.request_client_credentials(started, deadline).await
        })
        .await
        .unwrap_or(Err(AcquisitionError::Timeout));
        let result = if Instant::now() >= deadline {
            Err(AcquisitionError::Timeout)
        } else {
            result
        };

        metric.finish(&result);
        result
    }

    async fn request_client_credentials(
        &self,
        started: Instant,
        deadline: Instant,
    ) -> Result<Token, AcquisitionError> {
        let assertion = self.signer.sign()?;
        let scope = joined_scopes(&self.scopes);
        let mut fields = vec![("grant_type", GRANT_CLIENT_CREDENTIALS)];
        fields.extend(self.signer.form_fields(&assertion));
        if let Some(scope) = &scope {
            fields.push(("scope", scope.as_str()));
        }
        if let Some(audience) = &self.audience {
            fields.push(("audience", audience.as_str()));
        }
        let response = self.post_form(&fields, deadline).await?;
        into_token(&response, started, None)
    }

    /// Performs one token-exchange request for `subject`, bounded by
    /// `deadline`, and records its outcome. Called only as the initializer
    /// of [`Credentials::exchanged_or_fetch`]'s `or_try_insert_with`, so it
    /// runs at most once per coalesced group of concurrent callers for one subject.
    async fn fetch_exchange(
        &self,
        subject: &SecretString,
        deadline: Instant,
    ) -> Result<Arc<Token>, AcquisitionError> {
        let started = Instant::now();
        let mut metric = AttemptMetric::new(METRIC_GRANT_TOKEN_EXCHANGE, deadline);
        let result = async {
            let _permit = self.admit_attempt(deadline)?;
            self.request_token_exchange(subject, started, deadline)
                .await
        }
        .await;
        metric.finish(&result);
        result.map(Arc::new)
    }

    async fn request_token_exchange(
        &self,
        subject: &SecretString,
        started: Instant,
        deadline: Instant,
    ) -> Result<Token, AcquisitionError> {
        let assertion = self.signer.sign()?;
        let scope = joined_scopes(&self.scopes);
        let mut fields = vec![
            ("grant_type", GRANT_TOKEN_EXCHANGE),
            ("subject_token", subject.expose_secret()),
            ("subject_token_type", TOKEN_TYPE_ACCESS_TOKEN),
            ("requested_token_type", TOKEN_TYPE_ACCESS_TOKEN),
        ];
        if let Some(scope) = &scope {
            fields.push(("scope", scope.as_str()));
        }
        if let Some(audience) = &self.audience {
            fields.push(("audience", audience.as_str()));
        }
        fields.extend(self.signer.form_fields(&assertion));
        let response = self.post_form(&fields, deadline).await?;
        into_token(&response, started, Some(TOKEN_TYPE_ACCESS_TOKEN))
    }

    /// Sends one `application/x-www-form-urlencoded` request to the token
    /// endpoint through the bounded token client, shared by both grants.
    async fn post_form(
        &self,
        fields: &[(&str, &str)],
        deadline: Instant,
    ) -> Result<TokenResponse, AcquisitionError> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish();
        let request = Request::post(self.token_endpoint.as_str())
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(ACCEPT, "application/json")
            .body(Bytes::from(body))
            .map_err(|_| AcquisitionError::InvalidResponse)?;
        let response = self
            .token_http
            .execute(request, deadline)
            .await
            .map_err(|error| map_transport_error(&error))?;
        let status = response.status();
        // A throttled request, like a 5xx, may succeed later unchanged.
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            return Err(AcquisitionError::Unavailable);
        }
        if !status.is_success() {
            return Err(AcquisitionError::Rejected(Rejection::from_body(
                response.body(),
            )));
        }
        if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
            let media = content_type
                .to_str()
                .map_err(|_| AcquisitionError::InvalidResponse)?
                .split(';')
                .next()
                .unwrap_or_default()
                .trim();
            if !media.eq_ignore_ascii_case("application/json") {
                return Err(AcquisitionError::InvalidResponse);
            }
        }
        serde_json::from_slice(response.body().as_ref())
            .map_err(|_| AcquisitionError::InvalidResponse)
    }
}

fn map_transport_error(error: &infra_outbound_http::Error) -> AcquisitionError {
    match error {
        infra_outbound_http::Error::Timeout => AcquisitionError::Timeout,
        infra_outbound_http::Error::ResponseBodyTooLarge => AcquisitionError::ResponseLimit,
        infra_outbound_http::Error::InvalidTarget
        | infra_outbound_http::Error::Transport { .. } => AcquisitionError::Transport,
    }
}

fn joined_scopes(scopes: &[String]) -> Option<String> {
    if scopes.is_empty() {
        None
    } else {
        Some(scopes.join(" "))
    }
}

/// Builds `Token` from a parsed response: the Bearer check, header-value
/// check, sensitive header, reuse cutoff and refresh-ahead computation shared
/// by both grants. `require_issued_token_type` admits a token-exchange
/// response only when it reports that exact issued type (RFC 8693 section 2.2.1).
fn into_token(
    response: &TokenResponse,
    started: Instant,
    require_issued_token_type: Option<&str>,
) -> Result<Token, AcquisitionError> {
    if !response.token_type.eq_ignore_ascii_case("bearer") {
        return Err(AcquisitionError::InvalidResponse);
    }
    if let Some(expected) = require_issued_token_type
        && response.issued_token_type.as_deref() != Some(expected)
    {
        return Err(AcquisitionError::InvalidResponse);
    }
    if response.access_token.is_empty() {
        return Err(AcquisitionError::InvalidResponse);
    }
    let mut header = HeaderValue::try_from(format!("Bearer {}", response.access_token))
        .map_err(|_| AcquisitionError::InvalidResponse)?;
    header.set_sensitive(true);
    let reuse_until = match response.expires_in {
        None => None,
        Some(lifetime) => {
            let expiry = started
                .checked_add(Duration::from_secs(lifetime))
                .ok_or(AcquisitionError::InvalidResponse)?;
            if Instant::now() >= expiry {
                return Err(AcquisitionError::InvalidResponse);
            }
            Some(expiry.checked_sub(REUSE_MARGIN).unwrap_or(started))
        }
    };
    let refresh_after = reuse_until
        .filter(|until| require_issued_token_type.is_none() && Instant::now() < *until)
        .map(|until| {
            let maximum_lead = REFRESH_AHEAD.min(until.saturating_duration_since(started) / 4);
            until - (maximum_lead - sampled_refresh_spread(maximum_lead / 10))
        });
    Ok(Token {
        header,
        acquired: started,
        reuse_until,
        refresh_after,
    })
}

/// The window is at most 30 seconds (one tenth of the maximum refresh lead).
fn sampled_refresh_spread(window: Duration) -> Duration {
    let mut bytes = [0_u8; 2];
    let sample = aws_lc_rs::rand::fill(&mut bytes).map(|()| u16::from_be_bytes(bytes));
    refresh_spread(window, sample)
}

fn refresh_spread(
    window: Duration,
    sample: Result<u16, aws_lc_rs::error::Unspecified>,
) -> Duration {
    // At most 30 billion nanoseconds times 65535 fits in u64. Source failure
    // discards the bytes and preserves the old schedule with zero spread.
    let window_ns = window.as_secs() * 1_000_000_000 + u64::from(window.subsec_nanos());
    Duration::from_nanos(window_ns * u64::from(sample.unwrap_or(0)) / u64::from(u16::MAX))
}

fn refresh_retry() -> Duration {
    REFRESH_RETRY + sampled_refresh_spread(REFRESH_RETRY / 10)
}

fn subject_key(subject_token: &[u8]) -> [u8; 32] {
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, subject_token);
    let mut key = [0_u8; 32];
    key.copy_from_slice(digest.as_ref());
    key
}

/// Counts one token request exactly once, including when it is dropped.
struct AttemptMetric {
    grant: &'static str,
    outcome: Option<&'static str>,
    deadline: Instant,
}

impl AttemptMetric {
    fn new(grant: &'static str, deadline: Instant) -> Self {
        Self {
            grant,
            outcome: None,
            deadline,
        }
    }

    fn finish<T>(&mut self, result: &Result<T, AcquisitionError>) {
        self.outcome = Some(
            result
                .as_ref()
                .err()
                .map_or("success", |error| error.label()),
        );
    }
}

impl Drop for AttemptMetric {
    fn drop(&mut self) {
        let outcome = self.outcome.unwrap_or(if Instant::now() >= self.deadline {
            "timeout"
        } else {
            "cancelled"
        });
        metrics::counter!(
            "oauth2_token_acquisitions_total",
            "grant" => self.grant,
            "outcome" => outcome,
        )
        .increment(1);
    }
}

fn admitted_provider_concurrency(value: u32) -> Result<usize, ConfigurationError> {
    usize::try_from(value)
        .ok()
        .filter(|&permits| permits > 0 && permits <= Semaphore::MAX_PERMITS)
        .ok_or(ConfigurationError {
            key: "provider_concurrency",
            reason: "must be positive and supported on this target",
        })
}

fn admit_options(options: &Options) -> Result<Url, ConfigurationError> {
    admitted_provider_concurrency(options.provider_concurrency)?;
    let error = |key, reason| Err(ConfigurationError { key, reason });
    // The same rules as the typed configuration section, which refuses a
    // whitespace-only value as empty.
    if options.client_id.trim().is_empty() {
        return error("client_id", "must be nonempty");
    }
    if options.key_id.trim().is_empty() {
        return error("key_id", "must be nonempty");
    }
    if options.assertion_audience.trim().is_empty() {
        return error("assertion_audience", "must be nonempty");
    }
    if options.private_key.expose_secret().trim().is_empty() {
        return error("private_key", "must be nonempty");
    }
    if options.scopes.iter().any(|scope| {
        scope.is_empty()
            || !scope
                .bytes()
                .all(|b| matches!(b, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
    }) {
        return error("scopes", "must contain RFC 6749 scope tokens");
    }
    if options.audience.as_ref().is_some_and(String::is_empty) {
        return error("audience", "must be nonempty when configured");
    }
    if !EXCHANGE_CACHE_CAPACITY.contains(&options.exchange_cache_capacity) {
        return error("exchange_cache_capacity", "must be from 1 to 65536");
    }
    // `Url::parse` silently strips tabs and newlines, so refuse them first.
    if options
        .token_url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return error("token_url", "must be HTTPS without userinfo or fragment");
    }
    let Ok(endpoint) = Url::parse(&options.token_url) else {
        return error("token_url", "invalid URL");
    };
    if endpoint.scheme() != "https"
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || raw_authority_has_userinfo(&options.token_url)
        || endpoint.fragment().is_some()
    {
        return error("token_url", "must be HTTPS without userinfo or fragment");
    }
    Ok(endpoint)
}

/// `Url` drops an empty userinfo and reads a backslash as a path separator,
/// so the parsed URL alone admits an `@` that the typed configuration
/// section refuses.
fn raw_authority_has_userinfo(token_url: &str) -> bool {
    let Some((_, authority)) = token_url.split_once("://") else {
        return false;
    };
    let authority_end = authority.find(['/', '?', '#']).unwrap_or(authority.len());
    authority[..authority_end].contains('@')
}
