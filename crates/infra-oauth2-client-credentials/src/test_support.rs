use std::num::NonZeroU32;

use infra_outbound_http::Client;

use crate::{
    ConfigurationError, PrivateKeyJwt, Registration,
    options::{RawRegistration, parse_http_url, validate_registration},
    token_endpoint::TOKEN_LIMITS,
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
    let (audience_exact, audience_url) = options.auth.audience.into_parts();
    let validated = validate_registration(RawRegistration {
        token_url,
        client_id: options.client_id,
        private_key: options.auth.private_key,
        key_id: options.auth.key_id,
        algorithm: options.auth.algorithm,
        audience_exact,
        audience_url,
        provider_concurrency: options.provider_concurrency,
    })?;
    let token_http = Client::try_new_for_test_http(
        &validated.token_url.origin().ascii_serialization(),
        TOKEN_LIMITS,
    )
    .map_err(|_| ConfigurationError {
        key: "token_url",
        reason: "requires an HTTP loopback IP address",
    })?;
    Registration::build(validated, token_http)
}
