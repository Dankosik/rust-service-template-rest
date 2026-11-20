use std::num::NonZeroU32;

use crate::{
    ConfigurationError, PrivateKeyJwt, Registration,
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
