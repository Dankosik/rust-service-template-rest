//! Bearer verifier preparation for the selected authentication engine.

use service_config::{AuthnConfig, Config};
// template:begin oidc-jwt:bootstrap-auth-jwt-algorithm-import
use service_config::{JwtAlgorithm, TokenProfile};
// template:end oidc-jwt:bootstrap-auth-jwt-algorithm-import
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::{BootstrapError, PreparedAuth};

#[allow(
    unused_variables,
    clippy::unused_async,
    reason = "JWT discovery and refresh use the future and task ownership; introspection-only output preserves this preparation signature without provider I/O"
)]
pub(super) async fn prepare(
    config: &Config,
    background: &mut JoinSet<()>,
    cancel: &CancellationToken,
) -> Result<PreparedAuth, BootstrapError> {
    match &config.authn {
        // template:begin oidc-jwt:bootstrap-prepare-auth-jwt
        AuthnConfig::OidcJwt {
            issuer,
            audience,
            token_profile,
            algorithms,
            jwks_uri,
        } => {
            let issuer = issuer_url("oidc-jwt", "authn.issuer", issuer)?;
            let (verifier, refresh) = infra_bearerauthn::prepare_jwt(
                infra_bearerauthn::JwtOptions {
                    issuer,
                    audiences: audience.as_slice().to_vec(),
                    token_profile: match token_profile {
                        TokenProfile::ResourceServer => {
                            infra_bearerauthn::TokenProfile::ResourceServer
                        }
                        TokenProfile::Rfc9068 => infra_bearerauthn::TokenProfile::Rfc9068,
                    },
                    algorithms: algorithms.iter().copied().map(jwt_algorithm).collect(),
                    jwks_uri: jwks_endpoint(jwks_uri.as_deref())?,
                },
                cancel.child_token(),
            )
            .await
            .map_err(|source| BootstrapError::AuthenticationPreparation {
                mode: "oidc-jwt",
                key: "authn.issuer",
                source,
            })?;
            background.spawn(refresh);
            Ok(PreparedAuth::Enabled(Box::new(verifier)))
        }
        // template:end oidc-jwt:bootstrap-prepare-auth-jwt
        // template:begin oidc-introspection:bootstrap-prepare-auth-introspection
        AuthnConfig::OidcIntrospection {
            issuer,
            audience,
            introspection_endpoint,
            introspection_client_id,
            introspection_client_secret,
            provider_concurrency,
            cache_enabled,
            cache_capacity,
            cache_ttl,
        } => {
            let issuer = issuer_url("oidc-introspection", "authn.issuer", issuer)?;
            let endpoint = infra_bearerauthn::EndpointUrl::parse(introspection_endpoint).map_err(
                |source| BootstrapError::AuthenticationPreparation {
                    mode: "oidc-introspection",
                    key: "authn.introspection_endpoint",
                    source,
                },
            )?;
            // Configuration validation requires the secret in this mode.
            let client_secret = introspection_client_secret.clone().ok_or_else(|| {
                service_config::ValidationError::new(
                    "authn.introspection_client_secret",
                    "is required when authn.mode = oidc-introspection",
                )
            })?;
            let provider_concurrency = std::num::NonZeroUsize::try_from(*provider_concurrency)
                .map_err(|_| {
                    service_config::ValidationError::new(
                        "authn.provider_concurrency",
                        "does not fit this platform",
                    )
                })?;
            let cache =
                infra_bearerauthn::IntrospectionCacheOptions::new(*cache_capacity, *cache_ttl)
                    .map_err(|source| BootstrapError::AuthenticationPreparation {
                        mode: "oidc-introspection",
                        key: "authn.cache_capacity/authn.cache_ttl",
                        source,
                    })?;
            infra_bearerauthn::prepare_introspection(infra_bearerauthn::IntrospectionOptions {
                issuer,
                audiences: audience.as_slice().to_vec(),
                endpoint,
                client_id: introspection_client_id.clone(),
                client_secret,
                provider_concurrency,
                cache: cache_enabled.then_some(cache),
            })
            .map(|verifier| PreparedAuth::Enabled(Box::new(verifier)))
            .map_err(|source| BootstrapError::AuthenticationPreparation {
                mode: "oidc-introspection",
                key: "authn.introspection_endpoint",
                source,
            })
        }
        // template:end oidc-introspection:bootstrap-prepare-auth-introspection
        AuthnConfig::None {} => Ok(PreparedAuth::None),
    }
}

fn issuer_url(
    mode: &'static str,
    key: &'static str,
    value: &str,
) -> Result<infra_bearerauthn::IssuerUrl, BootstrapError> {
    infra_bearerauthn::IssuerUrl::parse(value)
        .map_err(|source| BootstrapError::AuthenticationPreparation { mode, key, source })
}

// template:begin oidc-jwt:bootstrap-auth-jwt-algorithm-converter
fn jwks_endpoint(
    value: Option<&str>,
) -> Result<Option<infra_bearerauthn::EndpointUrl>, BootstrapError> {
    value
        .map(infra_bearerauthn::EndpointUrl::parse)
        .transpose()
        .map_err(|source| BootstrapError::AuthenticationPreparation {
            mode: "oidc-jwt",
            key: "authn.jwks_uri",
            source,
        })
}

const fn jwt_algorithm(algorithm: JwtAlgorithm) -> infra_bearerauthn::JwtAlgorithm {
    match algorithm {
        JwtAlgorithm::Rs256 => infra_bearerauthn::JwtAlgorithm::Rs256,
        JwtAlgorithm::Es256 => infra_bearerauthn::JwtAlgorithm::Es256,
        JwtAlgorithm::Ps256 => infra_bearerauthn::JwtAlgorithm::Ps256,
        JwtAlgorithm::EdDsa => infra_bearerauthn::JwtAlgorithm::EdDsa,
    }
}

// template:begin oidc-introspection:bootstrap-introspection-cache-tests
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn introspection_preparation_validates_cache_options_even_when_disabled() {
        for (capacity, ttl) in [(0, "30s"), (1025, "30s"), (256, "0s"), (256, "301s")] {
            let config = Config {
                authn: serde_json::from_value(serde_json::json!({
                "mode": "oidc-introspection",
                "issuer": "https://issuer.example.test",
                "audience": "api",
                "introspection_endpoint": "https://issuer.example.test/introspect?tenant=private",
                "introspection_client_id": "service",
                "introspection_client_secret": "fixture-secret",
                "cache_enabled": false,
                "cache_capacity": capacity,
                "cache_ttl": ttl,
            }))
                .expect("raw typed configuration"),
                ..Config::default()
            };
            let mut background = JoinSet::new();
            let result = prepare(&config, &mut background, &CancellationToken::new()).await;
            assert!(matches!(
                result,
                Err(BootstrapError::AuthenticationPreparation {
                    key: "authn.cache_capacity/authn.cache_ttl",
                    ..
                })
            ));
            assert!(background.is_empty());
        }
    }
}
// template:end oidc-introspection:bootstrap-introspection-cache-tests
