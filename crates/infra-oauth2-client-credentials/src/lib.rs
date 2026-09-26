//! Private `OAuth2` client credentials for bounded outbound integrations.
//!
//! Composition prepares one immutable credential owner and binds it to a
//! resource client. Neither access tokens nor raw provider errors leave it.

use std::{fmt, sync::Arc, time::Duration};

use bytes::Bytes;
use http::{HeaderValue, Request, Response, header::AUTHORIZATION};
use infra_outbound_http::{Client, Limits, Operation};
use moka::{Expiry, future::Cache};
use oauth2::{
    AuthType, ClientId, ClientSecret, EndpointNotSet, EndpointSet, Scope, TokenResponse, TokenUrl,
    basic::BasicClient,
};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::time::Instant;
use url::Url;

#[cfg(test)]
mod tests;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const REUSE_MARGIN: Duration = Duration::from_secs(10);
const TOKEN_LIMITS: Limits = Limits {
    max_active: 1,
    operation_timeout: FETCH_TIMEOUT,
    response_header_count: 64,
    response_body_bytes: 1024 * 1024,
};

type ProtocolClient =
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
    #[error("OAuth2 provider rejected the request")]
    Rejected,
    #[error("OAuth2 token response is invalid")]
    InvalidResponse,
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

/// An idle, cloneable owner of one private credential tuple and its cache.
#[derive(Clone)]
pub struct Credentials(Arc<Owner>);

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials([REDACTED])")
    }
}

struct Owner {
    protocol: ProtocolClient,
    endpoint: Url,
    transport: Client,
    scopes: Vec<String>,
    audience: Option<String>,
    cache: Cache<(), Arc<CachedCredential>>,
}

impl Credentials {
    /// Prepares credentials without performing DNS, token, or resource I/O.
    ///
    /// # Errors
    /// Returns a sanitized option or transport-construction failure.
    pub fn new(options: Options) -> Result<Self, ConfigurationError> {
        let endpoint = admit_options(&options)?;
        let transport = Client::new(&endpoint.origin().ascii_serialization(), TOKEN_LIMITS)
            .map_err(|_| configuration_error("token_url", "transport construction failed"))?;
        Self::prepare(options, endpoint, transport)
    }

    fn prepare(
        options: Options,
        endpoint: Url,
        transport: Client,
    ) -> Result<Self, ConfigurationError> {
        let token_url = TokenUrl::new(endpoint.to_string())
            .map_err(|_| configuration_error("token_url", "invalid endpoint"))?;
        let protocol = BasicClient::new(ClientId::new(options.client_id))
            .set_client_secret(ClientSecret::new(
                options.client_secret.expose_secret().to_owned(),
            ))
            .set_auth_type(AuthType::BasicAuth)
            .set_token_uri(token_url);
        metrics::describe_counter!(
            "oauth2_token_acquisitions_total",
            "OAuth2 token attempts by closed terminal outcome"
        );
        Ok(Self(Arc::new(Owner {
            protocol,
            endpoint,
            transport,
            scopes: options.scopes,
            audience: options.audience,
            cache: Cache::builder()
                .max_capacity(1)
                .expire_after(ReuseExpiry)
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

    async fn acquire(&self, deadline: Instant) -> Result<Arc<CachedCredential>, AcquisitionError> {
        if Instant::now() >= deadline {
            return Err(AcquisitionError::Timeout);
        }
        let result = tokio::time::timeout_at(
            deadline,
            self.0.cache.try_get_with((), self.0.fetch(deadline)),
        )
        .await
        .map_err(|_| AcquisitionError::Timeout)?;
        match result {
            Ok(value) => Ok(value),
            Err(error) => match error.as_ref() {
                FillError::NotRetained(value) => Ok(value.clone()),
                FillError::Failed(error) => Err(*error),
            },
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
    /// Completed responses, including 401 and 403, are returned without replay.
    ///
    /// # Errors
    /// Rejects caller Authorization before I/O, acquisition failure before
    /// resource dispatch, and otherwise preserves bounded resource errors.
    pub async fn execute(
        &self,
        mut request: Request<Bytes>,
        operation: Operation,
    ) -> Result<Response<Bytes>, Error> {
        if request.headers().contains_key(AUTHORIZATION) {
            return Err(Error::AuthorizationConflict);
        }
        let value = self.credentials.acquire(operation.deadline).await?;
        if value
            .hard_expiry
            .is_some_and(|expiry| Instant::now() >= expiry)
        {
            return Err(AcquisitionError::InvalidResponse.into());
        }
        request
            .headers_mut()
            .insert(AUTHORIZATION, value.header.clone());
        Ok(self.resource.execute(request, operation).await?)
    }
}

impl Owner {
    async fn fetch(&self, caller_deadline: Instant) -> Result<Arc<CachedCredential>, FillError> {
        let started = Instant::now();
        let deadline = caller_deadline.min(started + FETCH_TIMEOUT);
        let mut attempt = Attempt {
            outcome: "cancelled",
            deadline,
        };
        let result = self.fetch_token(started, deadline).await;
        attempt.outcome = match &result {
            Ok(_) => "success",
            Err(AcquisitionError::Timeout) => "timeout",
            Err(AcquisitionError::Transport) => "transport",
            Err(AcquisitionError::ResponseLimit) => "limit",
            Err(AcquisitionError::Rejected) => "rejected",
            Err(AcquisitionError::InvalidResponse) => "invalid",
        };
        let value = Arc::new(result.map_err(FillError::Failed)?);
        if value
            .reuse_until
            .is_some_and(|cutoff| Instant::now() < cutoff)
        {
            Ok(value)
        } else {
            Err(FillError::NotRetained(value))
        }
    }

    async fn fetch_token(
        &self,
        started: Instant,
        deadline: Instant,
    ) -> Result<CachedCredential, AcquisitionError> {
        let mut exchange = self
            .protocol
            .exchange_client_credentials()
            .add_scopes(self.scopes.iter().cloned().map(Scope::new));
        if let Some(audience) = &self.audience {
            exchange = exchange.add_extra_param("audience", audience.as_str());
        }
        let hook = |request| self.exchange(request, deadline);
        let response = tokio::time::timeout_at(deadline, exchange.request_async(&hook))
            .await
            .map_err(|_| AcquisitionError::Timeout)?
            .map_err(|error| match error {
                oauth2::RequestTokenError::Request(error) => error,
                oauth2::RequestTokenError::ServerResponse(_) => AcquisitionError::Rejected,
                oauth2::RequestTokenError::Parse(_, _) | oauth2::RequestTokenError::Other(_) => {
                    AcquisitionError::InvalidResponse
                }
            })?;
        if Instant::now() >= deadline {
            return Err(AcquisitionError::Timeout);
        }
        if !response
            .token_type()
            .as_ref()
            .eq_ignore_ascii_case("bearer")
        {
            return Err(AcquisitionError::InvalidResponse);
        }
        let token = response.access_token().secret();
        let unpadded = token.trim_end_matches('=');
        if unpadded.is_empty()
            || !unpadded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
        {
            return Err(AcquisitionError::InvalidResponse);
        }
        let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| AcquisitionError::InvalidResponse)?;
        header.set_sensitive(true);
        let hard_expiry = response
            .expires_in()
            .and_then(|ttl| started.checked_add(ttl));
        if hard_expiry.is_some_and(|expiry| Instant::now() >= expiry) {
            return Err(AcquisitionError::InvalidResponse);
        }
        let reuse_until = hard_expiry.and_then(|expiry| expiry.checked_sub(REUSE_MARGIN));
        Ok(CachedCredential {
            header,
            hard_expiry,
            reuse_until,
        })
    }

    async fn exchange(
        &self,
        request: oauth2::HttpRequest,
        deadline: Instant,
    ) -> Result<oauth2::HttpResponse, AcquisitionError> {
        if request.uri() != self.endpoint.as_str() {
            return Err(AcquisitionError::InvalidResponse);
        }
        let (mut parts, body) = request.into_parts();
        parts.uri = self.endpoint[url::Position::BeforePath..]
            .parse()
            .map_err(|_| AcquisitionError::InvalidResponse)?;
        if let Some(header) = parts.headers.get_mut(AUTHORIZATION) {
            header.set_sensitive(true);
        }
        let response = self
            .transport
            .execute(
                Request::from_parts(parts, Bytes::from(body)),
                Operation {
                    deadline,
                    response_body_bytes: None,
                },
            )
            .await
            .map_err(|error| match error {
                infra_outbound_http::Error::Timeout { .. } => AcquisitionError::Timeout,
                infra_outbound_http::Error::ResponseBodyTooLarge => AcquisitionError::ResponseLimit,
                _ => AcquisitionError::Transport,
            })?;
        if !response.status().is_success() {
            return Err(AcquisitionError::Rejected);
        }
        let (parts, body) = response.into_parts();
        Ok(Response::from_parts(parts, body.to_vec()))
    }
}

struct CachedCredential {
    header: HeaderValue,
    hard_expiry: Option<Instant>,
    reuse_until: Option<Instant>,
}

impl fmt::Debug for CachedCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CachedCredential([REDACTED])")
    }
}

enum FillError {
    Failed(AcquisitionError),
    NotRetained(Arc<CachedCredential>),
}

struct ReuseExpiry;

impl Expiry<(), Arc<CachedCredential>> for ReuseExpiry {
    fn expire_after_create(
        &self,
        (): &(),
        value: &Arc<CachedCredential>,
        _: std::time::Instant,
    ) -> Option<Duration> {
        Some(value.reuse_until.map_or(Duration::ZERO, |cutoff| {
            cutoff.saturating_duration_since(Instant::now())
        }))
    }

    fn expire_after_update(
        &self,
        key: &(),
        value: &Arc<CachedCredential>,
        now: std::time::Instant,
        _: Option<Duration>,
    ) -> Option<Duration> {
        self.expire_after_create(key, value, now)
    }
    // The default read callback preserves the remaining duration.
}

struct Attempt {
    outcome: &'static str,
    deadline: Instant,
}

impl Drop for Attempt {
    fn drop(&mut self) {
        let outcome = if self.outcome == "cancelled" && Instant::now() >= self.deadline {
            "timeout"
        } else {
            self.outcome
        };
        metrics::counter!("oauth2_token_acquisitions_total", "outcome" => outcome).increment(1);
    }
}

fn configuration_error(key: &'static str, reason: &'static str) -> ConfigurationError {
    ConfigurationError { key, reason }
}

fn admit_options(options: &Options) -> Result<Url, ConfigurationError> {
    if options.client_id.is_empty() {
        return Err(configuration_error("client_id", "must be nonempty"));
    }
    if options.client_secret.expose_secret().is_empty() {
        return Err(configuration_error("client_secret", "must be nonempty"));
    }
    if options.scopes.iter().any(|scope| {
        scope.is_empty()
            || !scope
                .bytes()
                .all(|b| matches!(b, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
    }) {
        return Err(configuration_error(
            "scopes",
            "must contain RFC 6749 scope tokens",
        ));
    }
    if options.audience.as_ref().is_some_and(String::is_empty) {
        return Err(configuration_error(
            "audience",
            "must be nonempty when configured",
        ));
    }
    if options
        .token_url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(configuration_error(
            "token_url",
            "must be HTTPS without userinfo or fragment",
        ));
    }
    let endpoint = Url::parse(&options.token_url)
        .map_err(|_| configuration_error("token_url", "invalid URL"))?;
    if endpoint.scheme() != "https"
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
        || options
            .token_url
            .split_once("://")
            .is_some_and(|(_, tail)| {
                tail.split(['/', '?', '#'])
                    .next()
                    .is_some_and(|authority| authority.contains('@'))
            })
    {
        return Err(configuration_error(
            "token_url",
            "must be HTTPS without userinfo or fragment",
        ));
    }
    Ok(endpoint)
}
