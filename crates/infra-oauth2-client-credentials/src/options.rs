use std::{cell::Cell, num::NonZeroU32, str::FromStr};

use secrecy::{ExposeSecret as _, SecretString};
use shared_rules::uri;
use tokio::sync::Semaphore;
use url::Url;

use crate::ConfigurationError;

/// RFC 8725bis §3.1
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

#[derive(Clone, Eq, PartialEq)]
struct HttpsUrl {
    exact: String,
    url: Url,
}

impl HttpsUrl {
    fn parse(raw: &str, key: &'static str, allow_query: bool) -> Result<Self, ConfigurationError> {
        let url = parse_provider_url(raw, key, Some(("https", "must use HTTPS")), allow_query)?;
        Ok(Self {
            exact: raw.to_owned(),
            url,
        })
    }

    fn as_str(&self) -> &str {
        &self.exact
    }

    const fn url(&self) -> &Url {
        &self.url
    }

    fn into_parts(self) -> (String, Url) {
        (self.exact, self.url)
    }
}

#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("IssuerUrl([REDACTED])")]
pub struct IssuerUrl(HttpsUrl);

impl IssuerUrl {
    pub fn parse(raw: &str) -> Result<Self, ConfigurationError> {
        HttpsUrl::parse(raw, "issuer", false).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) const fn url(&self) -> &Url {
        self.0.url()
    }

    pub(crate) fn into_parts(self) -> (String, Url) {
        self.0.into_parts()
    }
}

impl AsRef<str> for IssuerUrl {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for IssuerUrl {
    type Err = ConfigurationError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("TokenUrl([REDACTED])")]
pub struct TokenUrl(HttpsUrl);

impl TokenUrl {
    pub fn parse(raw: &str) -> Result<Self, ConfigurationError> {
        HttpsUrl::parse(raw, "token_url", true).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) const fn url(&self) -> &Url {
        self.0.url()
    }

    pub(crate) fn into_url(self) -> Url {
        self.0.url
    }
}

impl AsRef<str> for TokenUrl {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for TokenUrl {
    type Err = ConfigurationError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

#[derive(Clone, derive_more::Debug)]
#[debug("PrivateKeyJwt([REDACTED])")]
pub struct PrivateKeyJwt {
    pub private_key: SecretString,
    pub key_id: String,
    pub algorithm: Algorithm,
    pub audience: IssuerUrl,
}

#[derive(Clone, derive_more::Debug)]
#[debug("Options([REDACTED])")]
pub struct Options {
    pub token_url: TokenUrl,
    pub client_id: String,
    pub auth: PrivateKeyJwt,
    pub provider_concurrency: NonZeroU32,
}

#[derive(Clone, Default, derive_more::Debug)]
#[debug("ServiceTokenTarget([REDACTED])")]
pub struct ServiceTokenTarget {
    pub(crate) scope: Option<String>,
}

impl ServiceTokenTarget {
    pub fn try_new(
        scopes: impl IntoIterator<Item: AsRef<str>>,
    ) -> Result<Self, ConfigurationError> {
        Ok(Self {
            scope: scope_parameter(scopes)?,
        })
    }
}

#[derive(Clone, Default, derive_more::Debug)]
#[debug("ExchangeTokenTarget([REDACTED])")]
pub struct ExchangeTokenTarget {
    pub(crate) scope: Option<String>,
    pub(crate) audience: Option<String>,
}

impl ExchangeTokenTarget {
    pub fn try_new(
        scopes: impl IntoIterator<Item: AsRef<str>>,
        audience: Option<String>,
    ) -> Result<Self, ConfigurationError> {
        let scope = scope_parameter(scopes)?;
        if audience
            .as_ref()
            .is_some_and(|audience| audience.trim().is_empty())
        {
            return Err(ConfigurationError {
                key: "audience",
                reason: "must be nonempty when configured",
            });
        }
        Ok(Self { scope, audience })
    }
}

fn scope_parameter(
    scopes: impl IntoIterator<Item: AsRef<str>>,
) -> Result<Option<String>, ConfigurationError> {
    let mut joined = String::new();
    for scope in scopes {
        let scope = scope.as_ref();
        if !shared_rules::rfc6749::is_scope_token(scope) {
            return Err(ConfigurationError {
                key: "scopes",
                reason: "must contain RFC 6749 scope tokens",
            });
        }
        if !joined.is_empty() {
            joined.push(' ');
        }
        joined.push_str(scope);
    }
    Ok((!joined.is_empty()).then_some(joined))
}

pub(crate) struct RawRegistration {
    pub(crate) token_url: Url,
    pub(crate) client_id: String,
    pub(crate) private_key: SecretString,
    pub(crate) key_id: String,
    pub(crate) algorithm: Algorithm,
    pub(crate) audience_exact: String,
    pub(crate) audience_url: Url,
    pub(crate) provider_concurrency: NonZeroU32,
}

impl From<Options> for RawRegistration {
    fn from(options: Options) -> Self {
        let (audience_exact, audience_url) = options.auth.audience.into_parts();
        Self {
            token_url: options.token_url.into_url(),
            client_id: options.client_id,
            private_key: options.auth.private_key,
            key_id: options.auth.key_id,
            algorithm: options.auth.algorithm,
            audience_exact,
            audience_url,
            provider_concurrency: options.provider_concurrency,
        }
    }
}

pub(crate) struct ValidatedRegistration {
    pub(crate) token_url: Url,
    pub(crate) client_id: String,
    pub(crate) private_key: SecretString,
    pub(crate) key_id: String,
    pub(crate) algorithm: Algorithm,
    pub(crate) audience: String,
    pub(crate) permits: usize,
}

fn validate_provider_concurrency(value: NonZeroU32) -> Result<usize, ConfigurationError> {
    usize::try_from(value.get())
        .ok()
        .filter(|&permits| permits <= Semaphore::MAX_PERMITS)
        .ok_or(ConfigurationError {
            key: "provider_concurrency",
            reason: "must be supported on this target",
        })
}

fn parse_provider_url(
    raw: &str,
    key: &'static str,
    required_scheme: Option<(&'static str, &'static str)>,
    allow_query: bool,
) -> Result<Url, ConfigurationError> {
    let error = |reason| Err(ConfigurationError { key, reason });
    if uri::has_whitespace_or_control(raw) {
        return error("must not contain whitespace or controls");
    }
    if uri::authority_has_userinfo(raw) {
        return error("must not include userinfo");
    }
    let syntax_valid = Cell::new(true);
    let Ok(url) = Url::options()
        .syntax_violation_callback(Some(&|_| syntax_valid.set(false)))
        .parse(raw)
    else {
        return error("must be a valid HTTPS URL");
    };
    if !syntax_valid.get() {
        return error("must be a valid HTTPS URL");
    }
    if let Some((expected, reason)) = required_scheme
        && url.scheme() != expected
    {
        return error(reason);
    }
    if url.host().is_none() {
        return error("must have a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return error("must not include userinfo");
    }
    if !allow_query && url.query().is_some() {
        return error("must not include a query");
    }
    if url.fragment().is_some() {
        return error("must not include a fragment");
    }
    Ok(url)
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn parse_http_url(
    raw: &str,
    key: &'static str,
    allow_query: bool,
) -> Result<Url, ConfigurationError> {
    parse_provider_url(
        raw,
        key,
        Some(("http", "must use loopback HTTP")),
        allow_query,
    )
}

pub(crate) fn parse_discovered_token_endpoint(raw: &str) -> Option<Url> {
    parse_provider_url(raw, "token_endpoint", None, true).ok()
}

pub(crate) fn validate_options(
    options: Options,
) -> Result<ValidatedRegistration, ConfigurationError> {
    validate_registration(options.into())
}

pub(crate) fn validate_registration(
    raw: RawRegistration,
) -> Result<ValidatedRegistration, ConfigurationError> {
    let permits = validate_provider_concurrency(raw.provider_concurrency)?;
    let error = |key, reason| Err(ConfigurationError { key, reason });
    if raw.client_id.trim().is_empty() {
        return error("client_id", "must be nonempty");
    }
    if raw.key_id.trim().is_empty() {
        return error("key_id", "must be nonempty");
    }
    if raw.private_key.expose_secret().trim().is_empty() {
        return error("private_key", "must be nonempty");
    }
    if raw.audience_url == raw.token_url {
        return error("assertion_audience", "must differ from the token endpoint");
    }
    Ok(ValidatedRegistration {
        token_url: raw.token_url,
        client_id: raw.client_id,
        private_key: raw.private_key,
        key_id: raw.key_id,
        algorithm: raw.algorithm,
        audience: raw.audience_exact,
        permits,
    })
}
