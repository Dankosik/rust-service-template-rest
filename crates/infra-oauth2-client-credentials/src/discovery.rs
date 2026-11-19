use bytes::Bytes;
use http::{Request, StatusCode, header};
use infra_outbound_http::{BuildError, Client, ExchangeError, prepare_tls};
use operation_context::OperationContext;
use serde::Deserialize;
use url::Url;

use crate::{
    ConfigurationError, DiscoveryError, DiscoveryTransportError, IssuerUrl, Options, Registration,
    TokenUrl, TransportBuildFailure,
    options::{Scheme, check_url_scheme, parse_discovered_token_endpoint, validate_options},
    token_endpoint::{FETCH_TIMEOUT, TOKEN_LIMITS},
};

#[derive(Deserialize)]
struct Metadata {
    issuer: String,
    token_endpoint: String,
}

impl Registration {
    pub async fn prepare_with_oidc_discovery(
        issuer: IssuerUrl,
        options: Options,
    ) -> Result<Self, DiscoveryError> {
        Self::prepare_with_oidc_discovery_scheme(issuer, options, Scheme::Https).await
    }

    #[cfg(test)]
    pub(crate) async fn prepare_with_oidc_discovery_for_test_http(
        issuer: IssuerUrl,
        options: Options,
    ) -> Result<Self, DiscoveryError> {
        Self::prepare_with_oidc_discovery_scheme(issuer, options, Scheme::Http).await
    }

    async fn prepare_with_oidc_discovery_scheme(
        issuer: IssuerUrl,
        options: Options,
        scheme: Scheme,
    ) -> Result<Self, DiscoveryError> {
        check_url_scheme(issuer.url(), "issuer", scheme)?;
        let validated = validate_options(options, scheme, scheme)?;
        if issuer.url() == validated.endpoint() {
            return Err(ConfigurationError {
                key: "issuer",
                reason: "must differ from the token endpoint",
            }
            .into());
        }
        verify_metadata(&issuer, validated.endpoint(), scheme).await?;
        let token_http =
            build_client(scheme, validated.endpoint()).map_err(TransportBuildFailure::token_url)?;
        Self::build(validated, token_http).map_err(DiscoveryError::from)
    }
}

pub async fn verify_oidc_metadata(
    issuer: &IssuerUrl,
    token_url: &TokenUrl,
) -> Result<(), DiscoveryError> {
    check_url_scheme(issuer.url(), "issuer", Scheme::Https)?;
    check_url_scheme(token_url.url(), "token_url", Scheme::Https)?;
    if issuer.url() == token_url.url() {
        return Err(ConfigurationError {
            key: "issuer",
            reason: "must differ from the token endpoint",
        }
        .into());
    }
    verify_metadata(issuer, token_url.url(), Scheme::Https).await
}

fn build_client(scheme: Scheme, url: &Url) -> Result<Client, BuildError> {
    match scheme {
        Scheme::Https => Client::try_new(url, TOKEN_LIMITS),
        #[cfg(any(test, feature = "test-support"))]
        Scheme::Http => Client::try_new_for_test_http(url, TOKEN_LIMITS),
    }
}

async fn verify_metadata(
    issuer: &IssuerUrl,
    endpoint: &Url,
    scheme: Scheme,
) -> Result<(), DiscoveryError> {
    if scheme == Scheme::Https {
        prepare_tls().await.map_err(TransportBuildFailure::issuer)?;
    }
    let discovery_http =
        build_client(scheme, issuer.url()).map_err(TransportBuildFailure::issuer)?;
    let metadata = fetch_metadata(issuer.url(), &discovery_http).await?;
    if metadata.issuer != issuer.as_str() {
        return Err(DiscoveryError::Metadata(
            "issuer differs from configured issuer",
        ));
    }
    if parse_discovered_token_endpoint(&metadata.token_endpoint).as_ref() != Some(endpoint) {
        return Err(DiscoveryError::Metadata(
            "token_endpoint is invalid or differs from token_url",
        ));
    }
    Ok(())
}

/// `OpenID` Connect Discovery 1.0 §§4.1–4.3
async fn fetch_metadata(issuer: &Url, client: &Client) -> Result<Metadata, DiscoveryError> {
    let issuer = issuer.as_str();
    let base = issuer.strip_suffix('/').unwrap_or(issuer);
    let request = Request::get(format!("{base}/.well-known/openid-configuration"))
        .header(header::ACCEPT, "application/json")
        .body(Bytes::new())
        .map_err(|_| DiscoveryError::Metadata("issuer discovery URL is invalid"))?;
    let context = OperationContext::with_timeout(FETCH_TIMEOUT);
    let response = match client.execute_buffered(request, &context).await {
        Ok(response) if response.status() != StatusCode::OK => {
            return Err(DiscoveryError::Status(response.status()));
        }
        Err(ExchangeError::AfterResponse { status, .. }) if status != StatusCode::OK => {
            return Err(DiscoveryError::Status(status));
        }
        Ok(response) => response,
        Err(error) => {
            return Err(DiscoveryError::Transport(DiscoveryTransportError::new(
                error.into_error(),
            )));
        }
    };
    decode_metadata(&response)
}

fn decode_metadata(response: &http::Response<Bytes>) -> Result<Metadata, DiscoveryError> {
    if !response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(shared_rules::rfc9110::is_json_content_type)
    {
        return Err(DiscoveryError::Metadata(
            "issuer discovery did not return application/json",
        ));
    }
    let body = response.body();
    if !body.trim_ascii_start().starts_with(b"{") {
        return Err(DiscoveryError::Metadata("invalid issuer metadata"));
    }
    serde_json::from_slice(body).map_err(DiscoveryError::Decode)
}
