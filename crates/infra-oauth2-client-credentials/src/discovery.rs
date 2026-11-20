use bytes::Bytes;
use http::{Request, StatusCode, header};
use infra_outbound_http::{Client, ExchangeError, prepare_tls};
use operation_context::OperationContext;
use serde::Deserialize;
use url::Url;

use crate::{
    ConfigurationError, DiscoveryError, DiscoveryTransportError, IssuerUrl, Options, Registration,
    TokenUrl, TransportBuildFailure,
    options::{parse_discovered_token_endpoint, validate_options},
    token_endpoint::{self, FETCH_TIMEOUT},
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
        let validated = validate_options(options)?;
        if issuer.url() == &validated.token_url {
            return Err(ConfigurationError {
                key: "issuer",
                reason: "must differ from the token endpoint",
            }
            .into());
        }
        verify_https_metadata(&issuer, &validated.token_url).await?;
        let token_http = token_endpoint::build_https_client(&validated.token_url, "token_url")?;
        Self::build(validated, token_http).map_err(DiscoveryError::from)
    }

    #[cfg(test)]
    pub(crate) async fn prepare_with_oidc_discovery_for_test_http(
        issuer: IssuerUrl,
        options: crate::test_support::HttpOptions,
    ) -> Result<Self, DiscoveryError> {
        let build_test_http = |url: &Url, endpoint: &'static str| {
            token_endpoint::build_http_client(url).map_err(|error| match error {
                infra_outbound_http::TestBuildError::InvalidOrigin(_) => {
                    DiscoveryError::from(ConfigurationError {
                        key: endpoint,
                        reason: "requires an HTTP loopback IP address",
                    })
                }
                infra_outbound_http::TestBuildError::Tls(cause) => {
                    DiscoveryError::from(TransportBuildFailure::new(endpoint, cause))
                }
            })
        };
        let token_url = crate::options::parse_http_url(&options.token_url, "token_url", true)?;
        let validated = crate::options::validate_registration(
            token_url,
            options.client_id,
            options.auth,
            options.provider_concurrency,
        )?;
        if issuer.url() == &validated.token_url {
            return Err(ConfigurationError {
                key: "issuer",
                reason: "must differ from the token endpoint",
            }
            .into());
        }
        let discovery_http = build_test_http(issuer.url(), "issuer")?;
        verify_metadata(&issuer, &validated.token_url, &discovery_http).await?;
        let token_http = build_test_http(&validated.token_url, "token_url")?;
        Self::build(validated, token_http).map_err(DiscoveryError::from)
    }
}

pub async fn verify_oidc_metadata(
    issuer: &IssuerUrl,
    token_url: &TokenUrl,
) -> Result<(), DiscoveryError> {
    if issuer.url() == token_url.url() {
        return Err(ConfigurationError {
            key: "issuer",
            reason: "must differ from the token endpoint",
        }
        .into());
    }
    verify_https_metadata(issuer, token_url.url()).await
}

async fn verify_https_metadata(issuer: &IssuerUrl, endpoint: &Url) -> Result<(), DiscoveryError> {
    prepare_tls().await.map_err(TransportBuildFailure::issuer)?;
    let discovery_http = token_endpoint::build_https_client(issuer.url(), "issuer")?;
    verify_metadata(issuer, endpoint, &discovery_http).await
}

async fn verify_metadata(
    issuer: &IssuerUrl,
    endpoint: &Url,
    discovery_http: &Client,
) -> Result<(), DiscoveryError> {
    let metadata = fetch_metadata(issuer.url(), discovery_http).await?;
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
    let response = match client.execute(&context, request).await {
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
