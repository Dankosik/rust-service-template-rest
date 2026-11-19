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

#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("IssuerUrl([REDACTED])")]
pub struct IssuerUrl {
    exact: String,
    url: Url,
}

impl IssuerUrl {
    pub fn parse(raw: &str) -> Result<Self, ConfigurationError> {
        parse_issuer_url(raw, "issuer", Scheme::Https)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn parse_for_test_http(raw: &str) -> Result<Self, ConfigurationError> {
        parse_issuer_url(raw, "issuer", Scheme::Http)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.exact
    }

    pub(crate) const fn url(&self) -> &Url {
        &self.url
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
pub struct TokenUrl {
    exact: String,
    url: Url,
}

impl TokenUrl {
    pub fn parse(raw: &str) -> Result<Self, ConfigurationError> {
        parse_token_url(raw, Scheme::Https)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn parse_for_test_http(raw: &str) -> Result<Self, ConfigurationError> {
        parse_token_url(raw, Scheme::Http)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.exact
    }

    pub(crate) const fn url(&self) -> &Url {
        &self.url
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

pub(crate) struct ValidatedOptions {
    pub(crate) options: Options,
    permits: usize,
}

impl ValidatedOptions {
    pub(crate) fn endpoint(&self) -> &Url {
        self.options.token_url.url()
    }

    pub(crate) fn into_parts(self) -> (Options, Url, usize) {
        let endpoint = self.options.token_url.url.clone();
        (self.options, endpoint, self.permits)
    }
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

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Scheme {
    Https,
    #[cfg(any(test, feature = "test-support"))]
    Http,
}

impl Scheme {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Https => "https",
            #[cfg(any(test, feature = "test-support"))]
            Self::Http => "http",
        }
    }

    const fn mismatch_reason(self) -> &'static str {
        match self {
            Self::Https => "must use HTTPS",
            #[cfg(any(test, feature = "test-support"))]
            Self::Http => "must use loopback HTTP",
        }
    }
}

fn parse_provider_url(
    raw: &str,
    key: &'static str,
    scheme: Option<Scheme>,
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
    if let Some(expected) = scheme
        && url.scheme() != expected.as_str()
    {
        return error(expected.mismatch_reason());
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

fn parse_issuer_url(
    raw: &str,
    key: &'static str,
    scheme: Scheme,
) -> Result<IssuerUrl, ConfigurationError> {
    let url = parse_provider_url(raw, key, Some(scheme), false)?;
    Ok(IssuerUrl {
        exact: raw.to_owned(),
        url,
    })
}

fn parse_token_url(raw: &str, scheme: Scheme) -> Result<TokenUrl, ConfigurationError> {
    let url = parse_provider_url(raw, "token_url", Some(scheme), true)?;
    Ok(TokenUrl {
        exact: raw.to_owned(),
        url,
    })
}

pub(crate) fn parse_discovered_token_endpoint(raw: &str) -> Option<Url> {
    parse_provider_url(raw, "token_endpoint", None, true).ok()
}

pub(crate) fn check_url_scheme(
    url: &Url,
    key: &'static str,
    scheme: Scheme,
) -> Result<(), ConfigurationError> {
    if url.scheme() != scheme.as_str() {
        return Err(ConfigurationError {
            key,
            reason: scheme.mismatch_reason(),
        });
    }
    Ok(())
}

pub(crate) fn validate_options(
    options: Options,
    endpoint_scheme: Scheme,
    audience_scheme: Scheme,
) -> Result<ValidatedOptions, ConfigurationError> {
    let permits = validate_provider_concurrency(options.provider_concurrency)?;
    let error = |key, reason| Err(ConfigurationError { key, reason });
    if options.client_id.trim().is_empty() {
        return error("client_id", "must be nonempty");
    }
    if options.auth.key_id.trim().is_empty() {
        return error("key_id", "must be nonempty");
    }
    check_url_scheme(
        options.auth.audience.url(),
        "assertion_audience",
        audience_scheme,
    )?;
    if options.auth.private_key.expose_secret().trim().is_empty() {
        return error("private_key", "must be nonempty");
    }
    check_url_scheme(options.token_url.url(), "token_url", endpoint_scheme)?;
    if options.auth.audience.url() == options.token_url.url() {
        return error("assertion_audience", "must differ from the token endpoint");
    }
    Ok(ValidatedOptions { options, permits })
}
