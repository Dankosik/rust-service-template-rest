//! Bearer verifier preparation for the selected authentication engine.

use service_config::{AuthnConfig, Config};
// template:begin oidc-jwt:bootstrap-auth-jwt-algorithm-import
use service_config::{JwtAlgorithm, TokenProfile};
// template:end oidc-jwt:bootstrap-auth-jwt-algorithm-import
use super::background::Background;

use super::{BootstrapError, PreparedAuth};

#[allow(
    unused_variables,
    clippy::unused_async,
    reason = "JWT discovery and refresh use the future and task ownership; introspection-only output preserves this preparation signature without provider I/O"
)]
pub(super) async fn prepare(
    config: &Config,
    background: &mut Background,
) -> Result<PreparedAuth, BootstrapError> {
    let mode = config.authn.mode();
    match &config.authn {
        // template:begin oidc-jwt:bootstrap-prepare-auth-jwt
        AuthnConfig::OidcJwt {
            issuer,
            audience,
            token_profile,
            algorithms,
            jwks_uri,
        } => {
            let issuer = issuer_url(mode, "authn.issuer", issuer)?;
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
                    jwks_uri: jwks_endpoint(mode, jwks_uri.as_deref())?,
                },
                background.child_token(),
            )
            .await
            .map_err(|source| authentication_preparation_error(mode, jwks_uri.is_some(), source))?;
            background.spawn("jwt_refresh", refresh);
            Ok(PreparedAuth::Enabled(Box::new(verifier)))
        }
        // template:end oidc-jwt:bootstrap-prepare-auth-jwt
        // template:begin oidc-introspection:bootstrap-prepare-auth-introspection
        AuthnConfig::OidcIntrospection {
            issuer,
            audience,
            introspection_endpoint,
            introspection_client_id,
            introspection_client_secret: _,
            provider_concurrency,
            cache_enabled,
            cache_capacity,
            cache_ttl,
        } => {
            let issuer = issuer_url(mode, "authn.issuer", issuer)?;
            let endpoint = infra_bearerauthn::EndpointUrl::parse(introspection_endpoint).map_err(
                |source| BootstrapError::AuthenticationPreparation {
                    mode,
                    key: "authn.introspection_endpoint",
                    source,
                },
            )?;
            let client_secret = config.authn.required_introspection_client_secret()?.clone();
            let provider_concurrency = std::num::NonZeroUsize::try_from(*provider_concurrency)
                .map_err(|_| {
                    service_config::ValidationError::new(
                        "authn.provider_concurrency",
                        "does not fit this platform",
                    )
                })?;
            // Checked even with the cache off: configuration-source-policy
            // makes invalid cache options a startup error in either state.
            let cache =
                infra_bearerauthn::IntrospectionCacheOptions::try_new(*cache_capacity, *cache_ttl)
                    .map_err(|source| BootstrapError::AuthenticationPreparation {
                        mode,
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
                mode,
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
/// Attribute a preparation failure to the input that was being read when it
/// failed. Discovery and issuer agreement belong to `authn.issuer`; a fetch
/// from an explicitly configured key set belongs to `authn.jwks_uri`.
fn authentication_preparation_error(
    mode: &'static str,
    explicit_jwks_uri: bool,
    source: infra_bearerauthn::PreparationError,
) -> BootstrapError {
    let key = authentication_key(source.phase(), explicit_jwks_uri);
    BootstrapError::AuthenticationPreparation { mode, key, source }
}

fn authentication_key(
    phase: infra_bearerauthn::PreparationPhase,
    explicit_jwks_uri: bool,
) -> &'static str {
    match (phase, explicit_jwks_uri) {
        (infra_bearerauthn::PreparationPhase::Jwks, true) => "authn.jwks_uri",
        _ => "authn.issuer",
    }
}

fn jwks_endpoint(
    mode: &'static str,
    value: Option<&str>,
) -> Result<Option<infra_bearerauthn::EndpointUrl>, BootstrapError> {
    value
        .map(infra_bearerauthn::EndpointUrl::parse)
        .transpose()
        .map_err(|source| BootstrapError::AuthenticationPreparation {
            mode,
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
// template:end oidc-jwt:bootstrap-auth-jwt-algorithm-converter

// template:begin oidc-introspection:bootstrap-introspection-cache-tests
#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

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
            let mut background = Background::new(CancellationToken::new());
            let result = prepare(&config, &mut background).await;
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

// template:begin oidc-jwt:bootstrap-auth-jwt-key-test
#[cfg(test)]
mod jwt_tests {
    use infra_bearerauthn::PreparationPhase;

    use super::*;

    #[test]
    fn a_jwks_failure_names_the_explicit_key_set_only() {
        assert_eq!(
            authentication_key(PreparationPhase::Jwks, true),
            "authn.jwks_uri"
        );
        assert_eq!(
            authentication_key(PreparationPhase::Jwks, false),
            "authn.issuer"
        );
        assert_eq!(
            authentication_key(PreparationPhase::Discovery, true),
            "authn.issuer"
        );
        assert_eq!(
            authentication_key(PreparationPhase::Options, true),
            "authn.issuer"
        );
    }
}
// template:end oidc-jwt:bootstrap-auth-jwt-key-test
