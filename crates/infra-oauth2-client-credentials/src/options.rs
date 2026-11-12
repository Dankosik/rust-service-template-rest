use secrecy::{ExposeSecret as _, SecretString};
use tokio::sync::Semaphore;
use url::Url;

use crate::ConfigurationError;

/// The admitted inclusive range for [`Options::exchange_cache_capacity`].
const EXCHANGE_CACHE_CAPACITY_RANGE: std::ops::RangeInclusive<u32> = 1..=65_536;

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
#[derive(Clone, derive_more::Debug)]
#[debug("Options([REDACTED])")]
pub struct Options {
    /// The token endpoint URL; must be HTTPS without userinfo or fragment and
    /// must not contain whitespace or control characters.
    pub token_url: String,
    /// The client identifier sent in token requests and as the assertion `iss` and `sub`.
    pub client_id: String,
    /// The PEM private key that signs client assertions; must match `algorithm`.
    pub private_key: SecretString,
    /// The key identifier carried in the client-assertion header as `kid`.
    pub key_id: String,
    /// The client-assertion signing algorithm; it must match the key.
    pub algorithm: Algorithm,
    /// The audience asserted in the signed client assertion, normally the
    /// authorization server's issuer identifier.
    pub assertion_audience: String,
    /// Scopes requested from the provider; each is an RFC 6749 scope token, and an empty list requests none.
    pub scopes: Vec<String>,
    /// The `audience` parameter of token requests; `None` omits it.
    pub audience: Option<String>,
    /// Best-effort retained subject count, from 1 to 65,536, alongside a 16 MiB
    /// Bearer payload target. Zero does not disable the cache; it is refused.
    pub exchange_cache_capacity: u32,
    /// Maximum simultaneous token attempts shared by both grants. Must be
    /// positive and supported as a semaphore permit count on this target.
    pub provider_concurrency: u32,
}

/// Options [`admit_options`] accepted, in the form
/// [`Credentials::build`](crate::Credentials::build) uses.
pub(crate) struct Admitted {
    pub(crate) endpoint: Url,
    pub(crate) permits: usize,
}

pub(crate) fn admitted_provider_concurrency(value: u32) -> Result<usize, ConfigurationError> {
    usize::try_from(value)
        .ok()
        .filter(|&permits| permits > 0 && permits <= Semaphore::MAX_PERMITS)
        .ok_or(ConfigurationError {
            key: "provider_concurrency",
            reason: "must be positive and supported on this target",
        })
}

pub(crate) fn admit_options(options: &Options) -> Result<Admitted, ConfigurationError> {
    let permits = admitted_provider_concurrency(options.provider_concurrency)?;
    let error = |key, reason| Err(ConfigurationError { key, reason });
    // Direct construction of `Options` bypasses the configuration loader.
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
    // `Url::parse` silently strips tabs and newlines, so refuse them first with
    // the rule that actually failed.
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
    Ok(Admitted { endpoint, permits })
}
