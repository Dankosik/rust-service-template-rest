use std::num::NonZeroU32;

use crate::{
    ConfigurationError, ExchangeTokenTarget, ExchangeTokens, PrivateKeyJwt, RefreshDriver,
    Registration, ServiceTokenTarget, ServiceTokens,
    options::{parse_http_url, validate_registration},
    token_endpoint,
};

#[derive(Clone, derive_more::Debug)]
#[debug("HttpOptions([REDACTED])")]
pub struct HttpOptions {
    pub token_url: String,
    pub client_id: String,
    pub auth: PrivateKeyJwt,
    pub provider_concurrency: NonZeroU32,
}

pub fn prepare_http(options: HttpOptions) -> Result<Registration, ConfigurationError> {
    let token_url = parse_http_url(&options.token_url, "token_url", true)?;
    let validated = validate_registration(
        token_url,
        options.client_id,
        options.auth,
        options.provider_concurrency,
    )?;
    let token_http = token_endpoint::build_http_client(&validated.token_url).map_err(|_| {
        ConfigurationError {
            key: "token_url",
            reason: "requires an HTTP loopback IP address",
        }
    })?;
    Registration::build(validated, token_http)
}

pub fn service_tokens_http(
    registration: &Registration,
    target: ServiceTokenTarget,
    resource_origin: &str,
) -> Result<(ServiceTokens, RefreshDriver), ConfigurationError> {
    Ok(registration.service_tokens_uri(target, parse_http_origin(resource_origin)?))
}

pub fn exchange_tokens_http(
    registration: &Registration,
    target: ExchangeTokenTarget,
    resource_origin: &str,
    capacity: NonZeroU32,
) -> Result<ExchangeTokens, ConfigurationError> {
    registration.exchange_tokens_uri(target, parse_http_origin(resource_origin)?, capacity)
}

fn parse_http_origin(raw: &str) -> Result<http::Uri, ConfigurationError> {
    let url = parse_http_url(raw, "resource_origin", false)?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(_)) | None => false,
    };
    let raw_bare = http::Uri::try_from(raw).is_ok_and(|uri| {
        uri.path_and_query()
            .is_none_or(|pq| pq.query().is_none() && matches!(pq.path(), "" | "/"))
    });
    if !loopback || !raw_bare {
        return Err(ConfigurationError {
            key: "resource_origin",
            reason: "requires an HTTP loopback IP origin",
        });
    }
    http::Uri::try_from(url.origin().ascii_serialization()).map_err(|_| ConfigurationError {
        key: "resource_origin",
        reason: "requires an HTTP loopback IP origin",
    })
}
