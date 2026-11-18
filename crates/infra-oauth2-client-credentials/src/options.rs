use std::cell::Cell;

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

#[derive(Clone, derive_more::Debug)]
#[debug("Options([REDACTED])")]
pub struct Options {
    pub token_url: String,
    pub client_id: String,
    pub private_key: SecretString,
    pub key_id: String,
    pub algorithm: Algorithm,
    pub assertion_audience: String,
    pub provider_concurrency: u32,
}

#[derive(Clone, Default, derive_more::Debug)]
#[debug("ServiceTokenTarget([REDACTED])")]
pub struct ServiceTokenTarget {
    pub(crate) scope: Option<String>,
}

impl ServiceTokenTarget {
    pub fn try_new(scopes: &[String]) -> Result<Self, ConfigurationError> {
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
        scopes: &[String],
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

fn scope_parameter(scopes: &[String]) -> Result<Option<String>, ConfigurationError> {
    if !scopes
        .iter()
        .all(|scope| shared_rules::rfc6749::is_scope_token(scope))
    {
        return Err(ConfigurationError {
            key: "scopes",
            reason: "must contain RFC 6749 scope tokens",
        });
    }
    Ok((!scopes.is_empty()).then(|| scopes.join(" ")))
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

    #[cfg(all(test, feature = "integration"))]
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

fn assertion_issuer(raw: &str) -> Result<Url, ConfigurationError> {
    let invalid_issuer = ConfigurationError {
        key: "assertion_audience",
        reason: "must be an HTTPS issuer URL without userinfo, a query, or a fragment",
    };
    if uri::has_whitespace_or_control(raw) || uri::authority_has_userinfo(raw) {
        return Err(invalid_issuer);
    }
    let invalid_syntax = Cell::new(false);
    let issuer = Url::options()
        .syntax_violation_callback(Some(&|_| invalid_syntax.set(true)))
        .parse(raw)
        .map_err(|_| invalid_issuer)?;
    if invalid_syntax.get()
        || issuer.scheme() != "https"
        || issuer.host().is_none()
        || !issuer.username().is_empty()
        || issuer.password().is_some()
        || issuer.query().is_some()
        || issuer.fragment().is_some()
    {
        return Err(invalid_issuer);
    }
    Ok(issuer)
}

pub(crate) fn admit_options(
    options: Options,
    scheme: &str,
) -> Result<Admitted, ConfigurationError> {
    let permits = admitted_provider_concurrency(options.provider_concurrency)?;
    let error = |key, reason| Err(ConfigurationError { key, reason });
    if options.client_id.trim().is_empty() {
        return error("client_id", "must be nonempty");
    }
    if options.key_id.trim().is_empty() {
        return error("key_id", "must be nonempty");
    }
    let issuer = assertion_issuer(&options.assertion_audience)?;
    if options.private_key.expose_secret().trim().is_empty() {
        return error("private_key", "must be nonempty");
    }
    if uri::has_whitespace_or_control(&options.token_url) {
        return error("token_url", "must not contain whitespace or controls");
    }
    let Ok(endpoint) = Url::parse(&options.token_url) else {
        return error("token_url", "must be a valid HTTPS URL");
    };
    if endpoint.scheme() != scheme {
        return error(
            "token_url",
            if scheme == "https" {
                "must use HTTPS"
            } else {
                "must use loopback HTTP"
            },
        );
    }
    if endpoint.host_str().is_none() {
        return error("token_url", "must have a host");
    }
    if !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || uri::authority_has_userinfo(&options.token_url)
    {
        return error("token_url", "must not include userinfo");
    }
    if endpoint.fragment().is_some() {
        return error("token_url", "must not include a fragment");
    }
    if issuer == endpoint {
        return error("assertion_audience", "must differ from the token endpoint");
    }
    Ok(Admitted {
        options,
        endpoint,
        permits,
    })
}
