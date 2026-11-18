use bytes::Bytes;
use http::{Request, StatusCode, header};
use infra_outbound_http::{BuildError, Client, Limits, prepare_tls};
use serde::Deserialize;
use tokio::time::Instant;
use url::Url;

use crate::{
    ConfigurationError, Options, PreparationError, Registration,
    options::{admit_options, assertion_issuer, strict_provider_url, token_endpoint},
    token_endpoint::{FETCH_TIMEOUT, TOKEN_LIMITS},
};

#[derive(Deserialize)]
struct Metadata {
    issuer: String,
    token_endpoint: String,
}

impl Registration {
    pub async fn prepare_keycloak(options: Options) -> Result<Self, PreparationError> {
        Self::prepare_keycloak_with(options, "https", Client::try_new).await
    }

    #[cfg(test)]
    pub(crate) async fn prepare_keycloak_for_test_http(
        options: Options,
    ) -> Result<Self, PreparationError> {
        Self::prepare_keycloak_with(options, "http", Client::try_new_for_test_http).await
    }

    async fn prepare_keycloak_with(
        options: Options,
        scheme: &str,
        client: fn(&Url, Limits) -> Result<Client, BuildError>,
    ) -> Result<Self, PreparationError> {
        let admitted = admit_options(options, scheme, scheme)?;
        verify_metadata(
            &admitted.options.assertion_audience,
            &admitted.issuer,
            &admitted.endpoint,
            client,
        )
        .await?;
        let token_http = client(&admitted.endpoint, TOKEN_LIMITS).map_err(|source| {
            PreparationError::Transport {
                endpoint: "token_url",
                source,
            }
        })?;
        Self::build(admitted, token_http).map_err(PreparationError::from)
    }
}

pub async fn verify_keycloak_metadata(
    issuer: &str,
    token_url: &str,
) -> Result<(), PreparationError> {
    let issuer_url = assertion_issuer(issuer, "https")?;
    let endpoint = token_endpoint(token_url, "https", &issuer_url)?;
    verify_metadata(issuer, &issuer_url, &endpoint, Client::try_new).await
}

async fn verify_metadata(
    issuer: &str,
    issuer_url: &Url,
    endpoint: &Url,
    client: fn(&Url, Limits) -> Result<Client, BuildError>,
) -> Result<(), PreparationError> {
    let discovery_transport = |source| PreparationError::Transport {
        endpoint: "assertion_audience",
        source,
    };
    if issuer_url.scheme() == "https" {
        prepare_tls().await.map_err(discovery_transport)?;
    }
    let discovery_http = client(issuer_url, TOKEN_LIMITS).map_err(discovery_transport)?;
    let metadata = fetch_metadata(issuer_url, &discovery_http).await?;
    if metadata.issuer != issuer {
        return Err(PreparationError::DiscoveryResponse(
            "issuer differs from assertion_audience",
        ));
    }
    if strict_provider_url(&metadata.token_endpoint).as_ref() != Some(endpoint) {
        return Err(PreparationError::DiscoveryResponse(
            "token_endpoint is invalid or differs from token_url",
        ));
    }
    Ok(())
}

/// `OpenID` Connect Discovery 1.0 §§4.1–4.3
async fn fetch_metadata(issuer: &Url, client: &Client) -> Result<Metadata, PreparationError> {
    let issuer = issuer.as_str();
    let base = issuer.strip_suffix('/').unwrap_or(issuer);
    let request = Request::get(format!("{base}/.well-known/openid-configuration"))
        .header(header::ACCEPT, "application/json")
        .body(Bytes::new())
        .map_err(|_| ConfigurationError {
            key: "assertion_audience",
            reason: "issuer discovery URL is invalid",
        })?;
    let response = client
        .execute(request, Instant::now() + FETCH_TIMEOUT)
        .await
        .map_err(|source| PreparationError::DiscoveryRequest { source })?;
    if response.status() != StatusCode::OK {
        return Err(PreparationError::DiscoveryResponse(
            "issuer discovery did not return HTTP 200",
        ));
    }
    if !response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(shared_rules::rfc9110::is_json_content_type)
    {
        return Err(PreparationError::DiscoveryResponse(
            "issuer discovery did not return application/json",
        ));
    }
    let body = response.body();
    if !body.trim_ascii_start().starts_with(b"{") {
        return Err(PreparationError::DiscoveryResponse(
            "invalid issuer metadata",
        ));
    }
    serde_json::from_slice(body).map_err(PreparationError::DiscoveryMetadata)
}
