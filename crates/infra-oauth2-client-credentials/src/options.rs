use secrecy::{ExposeSecret as _, SecretString};
use tokio::sync::Semaphore;
use url::Url;

use crate::ConfigurationError;

const EXCHANGE_CACHE_CAPACITY_RANGE: std::ops::RangeInclusive<u32> = 1..=65_536;

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

#[derive(Clone, derive_more::Debug)]
#[debug("Options([REDACTED])")]
pub struct Options {
    pub token_url: String,
    pub client_id: String,
    pub private_key: SecretString,
    pub key_id: String,
    pub algorithm: Algorithm,
    pub assertion_audience: String,
    /// RFC 6749 §3.3
    pub scopes: Vec<String>,
    pub audience: Option<String>,
    pub exchange_cache_capacity: u32,
    pub provider_concurrency: u32,
}

pub(crate) struct Admitted {
    options: Options,
    endpoint: Url,
    permits: usize,
}

impl Admitted {
    pub(crate) fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    pub(crate) fn into_parts(self) -> (Options, Url, usize) {
        (self.options, self.endpoint, self.permits)
    }

    #[cfg(test)]
    pub(crate) fn try_new_for_test(
        options: Options,
        endpoint: Url,
    ) -> Result<Self, ConfigurationError> {
        let permits = admitted_provider_concurrency(options.provider_concurrency)?;
        Ok(Self {
            options,
            endpoint,
            permits,
        })
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

pub(crate) fn admit_options(options: Options) -> Result<Admitted, ConfigurationError> {
    let permits = admitted_provider_concurrency(options.provider_concurrency)?;
    let error = |key, reason| Err(ConfigurationError { key, reason });
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
    if !options
        .scopes
        .iter()
        .all(|scope| shared_rules::rfc6749::is_scope_token(scope))
    {
        return error("scopes", "must contain RFC 6749 scope tokens");
    }
    if options
        .audience
        .as_ref()
        .is_some_and(|audience| audience.trim().is_empty())
    {
        return error("audience", "must be nonempty when configured");
    }
    if !EXCHANGE_CACHE_CAPACITY_RANGE.contains(&options.exchange_cache_capacity) {
        return error("exchange_cache_capacity", "must be from 1 to 65536");
    }
    if shared_rules::uri::has_whitespace_or_control(&options.token_url) {
        return error("token_url", "must not contain whitespace or controls");
    }
    let Ok(endpoint) = Url::parse(&options.token_url) else {
        return error("token_url", "must be a valid HTTPS URL");
    };
    if endpoint.scheme() != "https" {
        return error("token_url", "must use HTTPS");
    }
    if endpoint.host_str().is_none() {
        return error("token_url", "must have a host");
    }
    if !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || shared_rules::uri::authority_has_userinfo(&options.token_url)
    {
        return error("token_url", "must not include userinfo");
    }
    if endpoint.fragment().is_some() {
        return error("token_url", "must not include a fragment");
    }
    Ok(Admitted {
        options,
        endpoint,
        permits,
    })
}
