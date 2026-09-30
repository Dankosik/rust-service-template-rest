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
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use http::{
    HeaderMap, HeaderValue, Request, Response, StatusCode,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
};
use infra_outbound_http::{Client, Limits};
use moka::Expiry;
use secrecy::{ExposeSecret as _, SecretString};
use tokio::time::Instant;
use url::Url;
use uuid::Uuid;

#[cfg(test)]
mod tests;

// template:begin outbound-auth-grpc:oauth-grpc-module
#[cfg(feature = "grpc")]
pub mod grpc;
// template:end outbound-auth-grpc:oauth-grpc-module

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// A token stops being reused this long before it expires, as in Go's `oauth2`.
const REUSE_MARGIN: Duration = Duration::from_secs(10);
/// A reusable token is replaced in the background once at most this much, or a
/// quarter, of its reuse remains, as Azure.Core refreshes five minutes early.
const REFRESH_AHEAD: Duration = Duration::from_mins(5);
/// A background refresh attempt waits this long after the previous one.
const REFRESH_RETRY: Duration = Duration::from_secs(30);
const TOKEN_LIMITS: Limits = Limits {
    operation_timeout: FETCH_TIMEOUT,
    response_header_count: 64,
    response_body_bytes: 1024 * 1024,
};
/// The largest number of distinct subjects whose exchanged token is retained.
const EXCHANGED_CACHE_CAPACITY: u64 = 1024;
/// RFC 7523 section 2.2 and the OIDF client-assertion notice: one string
/// audience, a fresh `jti`, and an assertion signed for at most this long.
const ASSERTION_LIFETIME_SECS: u64 = 60;
const ASSERTION_TYP: &str = "client-authentication+jwt";
const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";
const GRANT_CLIENT_CREDENTIALS: &str = "client_credentials";
const GRANT_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const TOKEN_TYPE_ACCESS_TOKEN: &str = "urn:ietf:params:oauth:token-type:access_token";
/// The `grant` metric label for token exchange; shorter than the URN the
/// form itself sends as `grant_type`.
const METRIC_GRANT_TOKEN_EXCHANGE: &str = "token_exchange";

/// The client-assertion signing algorithm. One algorithm per key (RFC 8725bis
/// section 3.1); `RS256` is accepted by every shortlisted authorization server.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Algorithm {
    #[default]
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
    #[error("OAuth2 provider rejected the request")]
    Rejected,
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
            Self::Rejected => "rejected",
            Self::InvalidResponse => "invalid",
            Self::Assertion => "assertion",
        }
    }
}

/// Authentication failures are distinct from the existing resource transport.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("caller Authorization header conflicts with OAuth2 authentication")]
    AuthorizationConflict,
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
pub struct Credentials(Arc<Inner>);

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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AcquisitionError::Assertion)?
            .as_secs();
        let claims = AssertionClaims {
            iss: &self.client_id,
            sub: &self.client_id,
            aud: &self.assertion_audience,
            iat: now,
            nbf: now,
            exp: now + ASSERTION_LIFETIME_SECS,
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
}

/// A sensitive `Bearer` header value and the instant it stops being reused.
struct Token {
    header: HeaderValue,
    /// `None` when the provider gave no lifetime: reuse until a resource 401.
    reuse_until: Option<Instant>,
    /// When a background refresh first replaces this token. Unused by an
    /// exchanged token, which has no background refresh.
    refresh_after: Option<Instant>,
}

impl Token {
    fn is_reusable(&self, now: Instant) -> bool {
        self.reuse_until.is_none_or(|until| now < until)
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
        // A token with no expiry must serve only the request(s) that fetched
        // it (never a later lookup): the earlier single-token cache reuses
        // such a token until a 401, but doing that per subject here would let
        // an unbounded number of subjects' entries outlive their session with
        // no expiry to bound them, evictable only by a 401 that may never
        // come. Zero retention reuses the introspection cache's identical
        // trick for an oversized entry: the coalesced initializer returns the
        // value to every caller coalesced into this exchange, but Moka treats
        // the entry as expired for every later lookup.
        Some(token.reuse_until.map_or(Duration::ZERO, |until| {
            until.saturating_duration_since(Instant::now())
        }))
    }
}

impl Credentials {
    /// Prepares credentials without performing DNS, token, or resource I/O.
    ///
    /// # Errors
    /// Returns a sanitized option or transport-construction failure.
    pub fn new(options: Options) -> Result<Self, ConfigurationError> {
        let endpoint = admit_options(&options)?;
        let token_http = Client::new(&endpoint, TOKEN_LIMITS).map_err(|_| ConfigurationError {
            key: "token_url",
            reason: "transport construction failed",
        })?;
        Self::prepare(options, &endpoint, token_http)
    }

    fn prepare(
        options: Options,
        endpoint: &Url,
        token_http: Client,
    ) -> Result<Self, ConfigurationError> {
        let signer = Signer::new(&options)?;
        metrics::describe_counter!(
            "oauth2_token_acquisitions_total",
            "OAuth2 token attempts by grant and closed terminal outcome"
        );
        Ok(Self(Arc::new(Inner {
            token_endpoint: endpoint.clone(),
            token_http,
            scopes: options.scopes,
            audience: options.audience,
            signer,
            cached: Mutex::default(),
            refresh: tokio::sync::Mutex::new(()),
            exchanged: moka::future::Cache::builder()
                .max_capacity(EXCHANGED_CACHE_CAPACITY)
                .expire_after(ExchangedExpiry)
                .build(),
        })))
    }

    /// Binds these credentials to the concrete provider's bounded HTTP client.
    #[must_use]
    pub fn http(&self, resource: Client) -> AuthenticatedClient {
        AuthenticatedClient {
            credentials: self.clone(),
            resource,
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
        let now = Instant::now();
        if now >= deadline {
            return Err(AcquisitionError::Timeout);
        }
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
        let _refresh = tokio::time::timeout_at(deadline, self.0.refresh.lock())
            .await
            .map_err(|_| AcquisitionError::Timeout)?;
        // The caller that held the lock may have just stored a reusable token.
        if let Some(token) = self.reusable_service_token(Instant::now()) {
            return Ok(token);
        }
        Ok(self.store_service_token(self.0.fetch_service_token(deadline).await?))
    }

    /// Returns the cached service token while it is reusable. The first
    /// caller to find it past its refresh time also starts a background
    /// refresh.
    fn reusable_service_token(&self, now: Instant) -> Option<Arc<Token>> {
        let mut cached = self.cached();
        let token = cached
            .token
            .as_ref()
            .filter(|token| token.is_reusable(now))?
            .clone();
        if cached.refresh_after.is_some_and(|after| now >= after) {
            cached.refresh_after = Some(now + REFRESH_RETRY);
            drop(cached);
            self.refresh_ahead(token.clone());
        }
        Some(token)
    }

    /// Replaces `current` without delaying any caller. A failure keeps it until
    /// its reuse cutoff; the next attempt waits [`REFRESH_RETRY`].
    fn refresh_ahead(&self, current: Arc<Token>) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let credentials = self.clone();
        runtime.spawn(async move {
            let _refresh = credentials.0.refresh.lock().await;
            // A caller that held the lock may have replaced or evicted it.
            let unchanged = credentials
                .cached()
                .token
                .as_ref()
                .is_some_and(|token| Arc::ptr_eq(token, &current));
            if unchanged
                && let Ok(token) = credentials
                    .0
                    .fetch_service_token(Instant::now() + FETCH_TIMEOUT)
                    .await
            {
                credentials.store_service_token(token);
                // A provider may return a token already inside its own
                // refresh window; still wait before the next attempt.
                let retry = Instant::now() + REFRESH_RETRY;
                let mut cached = credentials.cached();
                cached.refresh_after = cached.refresh_after.map(|after| after.max(retry));
            }
        });
    }

    fn store_service_token(&self, token: Token) -> Arc<Token> {
        let token = Arc::new(token);
        let mut cached = self.cached();
        cached.refresh_after = token.refresh_after;
        cached.token = Some(token.clone());
        token
    }

    /// Forgets the service token `used` unless a newer token already replaced it.
    fn reject_service_token(&self, used: &Arc<Token>) {
        let mut cached = self.cached();
        if cached
            .token
            .as_ref()
            .is_some_and(|token| Arc::ptr_eq(token, used))
        {
            *cached = Cached::default();
        }
    }

    fn cached(&self) -> MutexGuard<'_, Cached> {
        self.0.cached.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Forgets whichever token `acquired` used, from whichever cache it came
    /// from, unless a newer token already replaced it there.
    async fn reject_acquired(&self, acquired: &Acquired) {
        match acquired {
            Acquired::Service(token) => self.reject_service_token(token),
            Acquired::Exchanged { key, token } => self.reject_exchanged(*key, token).await,
        }
    }

    async fn reject_exchanged(&self, key: [u8; 32], used: &Arc<Token>) {
        let current = self.0.exchanged.get(&key).await;
        if current.is_some_and(|token| Arc::ptr_eq(&token, used)) {
            self.0.exchanged.invalidate(&key).await;
        }
    }

    /// Returns a live exchanged token for `subject`, exchanging it when the
    /// cache has none. The shared exchange, coalesced by Moka across
    /// concurrent misses for the same subject, is bounded by its own start
    /// plus [`FETCH_TIMEOUT`], not by this caller's deadline; this caller
    /// still stops waiting for it at `deadline`.
    async fn exchange(
        &self,
        key: [u8; 32],
        subject: &SecretString,
        deadline: Instant,
    ) -> Result<Arc<Token>, AcquisitionError> {
        if Instant::now() >= deadline {
            return Err(AcquisitionError::Timeout);
        }
        let (token, fresh) = self.exchanged_or_fetch(key, subject, deadline).await?;
        // A token this call just fetched serves it even inside its margin,
        // as a fresh service token does; only a stale hit is fetched again.
        if fresh || token.is_reusable(Instant::now()) {
            return Ok(token);
        }
        self.0.exchanged.invalidate(&key).await;
        Ok(self.exchanged_or_fetch(key, subject, deadline).await?.0)
    }

    /// Returns the cached or newly exchanged token and whether this call
    /// inserted it.
    async fn exchanged_or_fetch(
        &self,
        key: [u8; 32],
        subject: &SecretString,
        deadline: Instant,
    ) -> Result<(Arc<Token>, bool), AcquisitionError> {
        let fetch_deadline = Instant::now() + FETCH_TIMEOUT;
        let attempt = self
            .0
            .exchanged
            .entry(key)
            .or_try_insert_with(self.0.fetch_exchange(subject, fetch_deadline));
        match tokio::time::timeout_at(deadline, attempt).await {
            Ok(Ok(entry)) => {
                let fresh = entry.is_fresh();
                Ok((entry.into_value(), fresh))
            }
            Ok(Err(error)) => Err(*error),
            Err(_) => Err(AcquisitionError::Timeout),
        }
    }
}

/// A bounded resource client with private machine authentication.
#[derive(Clone)]
pub struct AuthenticatedClient {
    credentials: Credentials,
    resource: Client,
}

impl fmt::Debug for AuthenticatedClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthenticatedClient([REDACTED])")
    }
}

impl AuthenticatedClient {
    /// Acquires credentials, injects Bearer, and spends the original deadline.
    /// A request carrying [`OnBehalfOf`] in its extensions is sent with a
    /// token exchanged for that subject instead of the service token; the
    /// extension is always removed before dispatch. Completed responses,
    /// including 401 and 403, are returned without replay; a 401 evicts the
    /// token it used so the next call acquires anew.
    ///
    /// # Errors
    /// Rejects caller Authorization before I/O, acquisition failure before
    /// resource dispatch, and otherwise preserves bounded resource errors.
    pub async fn execute(
        &self,
        mut request: Request<Bytes>,
        deadline: Instant,
    ) -> Result<Response<Bytes>, Error> {
        if request.headers().contains_key(AUTHORIZATION) {
            return Err(Error::AuthorizationConflict);
        }
        let on_behalf_of = request.extensions_mut().remove::<OnBehalfOf>();
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
    /// Performs one client-credentials token request, bounded by the
    /// caller's deadline and [`FETCH_TIMEOUT`], and records its outcome.
    async fn fetch_service_token(
        &self,
        caller_deadline: Instant,
    ) -> Result<Token, AcquisitionError> {
        let started = Instant::now();
        let deadline = caller_deadline.min(started + FETCH_TIMEOUT);
        let mut metric = AttemptMetric::new(GRANT_CLIENT_CREDENTIALS, deadline);
        let result = self.request_client_credentials(started, deadline).await;
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
        let result = self
            .request_token_exchange(subject, started, deadline)
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
        if status.is_server_error() {
            return Err(AcquisitionError::Unavailable);
        }
        if !status.is_success() {
            return Err(AcquisitionError::Rejected);
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
        _ => AcquisitionError::Transport,
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
    let reuse_until = match response
        .expires_in
        .and_then(|lifetime| started.checked_add(Duration::from_secs(lifetime)))
    {
        None => None,
        Some(expiry) if Instant::now() >= expiry => {
            return Err(AcquisitionError::InvalidResponse);
        }
        // A token already inside the margin serves only this request.
        Some(expiry) => Some(expiry.checked_sub(REUSE_MARGIN).unwrap_or(started)),
    };
    let refresh_after = reuse_until
        .map(|until| until - REFRESH_AHEAD.min(until.saturating_duration_since(started) / 4));
    Ok(Token {
        header,
        reuse_until,
        refresh_after,
    })
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

fn admit_options(options: &Options) -> Result<Url, ConfigurationError> {
    let error = |key, reason| Err(ConfigurationError { key, reason });
    if options.client_id.is_empty() {
        return error("client_id", "must be nonempty");
    }
    if options.key_id.is_empty() {
        return error("key_id", "must be nonempty");
    }
    if options.assertion_audience.is_empty() {
        return error("assertion_audience", "must be nonempty");
    }
    if options.private_key.expose_secret().is_empty() {
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
        || endpoint.fragment().is_some()
    {
        return error("token_url", "must be HTTPS without userinfo or fragment");
    }
    Ok(endpoint)
}
