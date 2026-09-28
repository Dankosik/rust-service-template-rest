//! Private `OAuth2` client credentials for bounded outbound integrations.
//!
//! Composition prepares one immutable credential owner and binds it to a
//! resource client. Neither access tokens nor raw provider errors leave it.

use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode, header::AUTHORIZATION};
use infra_outbound_http::{Client, Limits};
use oauth2::{
    AuthType, ClientId, ClientSecret, EndpointNotSet, EndpointSet, Scope, TokenResponse, TokenUrl,
    basic::BasicClient,
};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::time::Instant;
use url::Url;

#[cfg(test)]
mod tests;

// template:begin outbound-auth-grpc:oauth-grpc-module
#[cfg(feature = "grpc")]
pub mod grpc;
// template:end outbound-auth-grpc:oauth-grpc-module

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// A token stops being reused this long before it expires, as in Go's `oauth2`.
const REUSE_MARGIN: Duration = Duration::from_secs(10);
const TOKEN_LIMITS: Limits = Limits {
    operation_timeout: FETCH_TIMEOUT,
    response_header_count: 64,
    response_body_bytes: 1024 * 1024,
};

type OAuthClient =
    BasicClient<EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// Immutable composition input. Secrets come from the configuration snapshot.
#[derive(Clone)]
pub struct Options {
    pub token_url: String,
    pub client_id: String,
    pub client_secret: SecretString,
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

/// An idle, cloneable owner of one private credential tuple and its token.
#[derive(Clone)]
pub struct Credentials(Arc<Inner>);

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials([REDACTED])")
    }
}

struct Inner {
    oauth: OAuthClient,
    token_endpoint: Url,
    token_http: Client,
    scopes: Vec<String>,
    audience: Option<String>,
    /// The last token; never held across an `.await`.
    cached: Mutex<Option<Arc<Token>>>,
    /// Held by the one caller requesting a new token, so requests never overlap.
    refresh: tokio::sync::Mutex<()>,
}

/// A sensitive `Bearer` header value and the instant it stops being reused.
struct Token {
    header: HeaderValue,
    /// `None` when the provider gave no lifetime: reuse until a resource 401.
    reuse_until: Option<Instant>,
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
        let invalid_endpoint = ConfigurationError {
            key: "token_url",
            reason: "invalid endpoint",
        };
        let token_url = TokenUrl::new(endpoint.to_string()).map_err(|_| invalid_endpoint)?;
        let oauth = BasicClient::new(ClientId::new(options.client_id))
            .set_client_secret(ClientSecret::new(
                options.client_secret.expose_secret().to_owned(),
            ))
            .set_auth_type(AuthType::BasicAuth)
            .set_token_uri(token_url);
        metrics::describe_counter!(
            "oauth2_token_acquisitions_total",
            "OAuth2 token attempts by closed terminal outcome"
        );
        Ok(Self(Arc::new(Inner {
            oauth,
            token_endpoint: endpoint.clone(),
            token_http,
            scopes: options.scopes,
            audience: options.audience,
            cached: Mutex::new(None),
            refresh: tokio::sync::Mutex::new(()),
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

    /// Inserts a sensitive Bearer header and returns the token it used, so a
    /// resource 401 can [`reject`](Self::reject) exactly that token.
    async fn authorize(
        &self,
        headers: &mut HeaderMap,
        deadline: Instant,
    ) -> Result<Arc<Token>, AcquisitionError> {
        let token = self.token(deadline).await?;
        headers.insert(AUTHORIZATION, token.header.clone());
        Ok(token)
    }

    /// Returns the cached token while it is reusable, otherwise requests a new
    /// one. Waiting for another caller's request spends this caller's deadline.
    async fn token(&self, deadline: Instant) -> Result<Arc<Token>, AcquisitionError> {
        let now = Instant::now();
        if now >= deadline {
            return Err(AcquisitionError::Timeout);
        }
        if let Some(token) = self.reusable(now) {
            return Ok(token);
        }
        // Boxed so that the reuse path above keeps a small future.
        Box::pin(self.acquire(deadline)).await
    }

    async fn acquire(&self, deadline: Instant) -> Result<Arc<Token>, AcquisitionError> {
        let _refresh = tokio::time::timeout_at(deadline, self.0.refresh.lock())
            .await
            .map_err(|_| AcquisitionError::Timeout)?;
        // The caller that held the lock may have just stored a reusable token.
        if let Some(token) = self.reusable(Instant::now()) {
            return Ok(token);
        }
        let token = Arc::new(self.0.fetch(deadline).await?);
        *self.cached() = Some(token.clone());
        Ok(token)
    }

    /// Forgets `used` unless a newer token already replaced it.
    fn reject(&self, used: &Arc<Token>) {
        let mut cached = self.cached();
        if cached
            .as_ref()
            .is_some_and(|token| Arc::ptr_eq(token, used))
        {
            *cached = None;
        }
    }

    fn reusable(&self, now: Instant) -> Option<Arc<Token>> {
        self.cached()
            .as_ref()
            .filter(|token| token.is_reusable(now))
            .cloned()
    }

    fn cached(&self) -> MutexGuard<'_, Option<Arc<Token>>> {
        self.0.cached.lock().unwrap_or_else(PoisonError::into_inner)
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
    /// Completed responses, including 401 and 403, are returned without replay;
    /// a 401 evicts the token it used so the next call acquires anew.
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
        let token = self
            .credentials
            .authorize(request.headers_mut(), deadline)
            .await?;
        let response = self.resource.execute(request, deadline).await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            self.credentials.reject(&token);
        }
        Ok(response)
    }
}

impl Inner {
    /// Performs one token request, bounded by the caller's deadline and
    /// [`FETCH_TIMEOUT`], and records its outcome.
    async fn fetch(&self, caller_deadline: Instant) -> Result<Token, AcquisitionError> {
        let started = Instant::now();
        let deadline = caller_deadline.min(started + FETCH_TIMEOUT);
        let mut metric = AttemptMetric {
            outcome: None,
            deadline,
        };
        let result = self.request_token(started, deadline).await;
        metric.outcome = Some(
            result
                .as_ref()
                .err()
                .map_or("success", |error| error.label()),
        );
        result
    }

    async fn request_token(
        &self,
        started: Instant,
        deadline: Instant,
    ) -> Result<Token, AcquisitionError> {
        let mut exchange = self
            .oauth
            .exchange_client_credentials()
            .add_scopes(self.scopes.iter().cloned().map(Scope::new));
        if let Some(audience) = &self.audience {
            exchange = exchange.add_extra_param("audience", audience.as_str());
        }
        let http = TokenHttp {
            endpoint: self.token_endpoint.clone(),
            client: self.token_http.clone(),
            deadline,
        };
        let response = exchange
            .request_async(&http)
            .await
            .map_err(|error| match error {
                oauth2::RequestTokenError::Request(error) => error,
                oauth2::RequestTokenError::ServerResponse(_) => AcquisitionError::Rejected,
                oauth2::RequestTokenError::Parse(_, _) | oauth2::RequestTokenError::Other(_) => {
                    AcquisitionError::InvalidResponse
                }
            })?;
        if !response
            .token_type()
            .as_ref()
            .eq_ignore_ascii_case("bearer")
        {
            return Err(AcquisitionError::InvalidResponse);
        }
        let token = response.access_token().secret();
        if token.is_empty() {
            return Err(AcquisitionError::InvalidResponse);
        }
        let mut header = HeaderValue::try_from(format!("Bearer {token}"))
            .map_err(|_| AcquisitionError::InvalidResponse)?;
        header.set_sensitive(true);
        let reuse_until = match response
            .expires_in()
            .and_then(|lifetime| started.checked_add(lifetime))
        {
            None => None,
            Some(expiry) if Instant::now() >= expiry => {
                return Err(AcquisitionError::InvalidResponse);
            }
            // A token already inside the margin serves only this request.
            Some(expiry) => Some(expiry.checked_sub(REUSE_MARGIN).unwrap_or(started)),
        };
        Ok(Token {
            header,
            reuse_until,
        })
    }
}

/// Sends oauth2's token request through the bounded token client.
struct TokenHttp {
    endpoint: Url,
    client: Client,
    deadline: Instant,
}

impl<'client> oauth2::AsyncHttpClient<'client> for TokenHttp {
    type Error = AcquisitionError;
    type Future =
        Pin<Box<dyn Future<Output = Result<oauth2::HttpResponse, Self::Error>> + Send + 'client>>;

    fn call(&'client self, request: oauth2::HttpRequest) -> Self::Future {
        Box::pin(async move {
            if request.uri() != self.endpoint.as_str() {
                return Err(AcquisitionError::InvalidResponse);
            }
            let (mut parts, body) = request.into_parts();
            if let Some(header) = parts.headers.get_mut(AUTHORIZATION) {
                header.set_sensitive(true);
            }
            let response = self
                .client
                .execute(Request::from_parts(parts, Bytes::from(body)), self.deadline)
                .await
                .map_err(|error| match error {
                    infra_outbound_http::Error::Timeout => AcquisitionError::Timeout,
                    infra_outbound_http::Error::ResponseBodyTooLarge => {
                        AcquisitionError::ResponseLimit
                    }
                    _ => AcquisitionError::Transport,
                })?;
            let status = response.status();
            if status.is_server_error() {
                return Err(AcquisitionError::Unavailable);
            }
            if !status.is_success() {
                return Err(AcquisitionError::Rejected);
            }
            let (parts, body) = response.into_parts();
            Ok(Response::from_parts(parts, body.into()))
        })
    }
}

/// Counts one token request exactly once, including when it is dropped.
struct AttemptMetric {
    outcome: Option<&'static str>,
    deadline: Instant,
}

impl Drop for AttemptMetric {
    fn drop(&mut self) {
        let outcome = self.outcome.unwrap_or(if Instant::now() >= self.deadline {
            "timeout"
        } else {
            "cancelled"
        });
        metrics::counter!("oauth2_token_acquisitions_total", "outcome" => outcome).increment(1);
    }
}

fn admit_options(options: &Options) -> Result<Url, ConfigurationError> {
    let error = |key, reason| Err(ConfigurationError { key, reason });
    if options.client_id.is_empty() {
        return error("client_id", "must be nonempty");
    }
    if options.client_secret.expose_secret().is_empty() {
        return error("client_secret", "must be nonempty");
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
