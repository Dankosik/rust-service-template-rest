use infra_bearerauthn::Verifier;
use service_config::{AuthnConfig, Config};
// template:begin oidc-jwt:bootstrap-auth-jwt-algorithm-import
use service_config::{JwtAlgorithm, TokenProfile};
// template:end oidc-jwt:bootstrap-auth-jwt-algorithm-import
use super::background::Background;

use super::BootstrapError;

const ISSUER_KEY: &str = "authn.issuer";

#[allow(
    unused_variables,
    clippy::unused_async,
    reason = "JWT discovery and refresh use the future and task ownership; introspection-only output preserves this preparation signature without provider I/O"
)]
pub(super) async fn prepare(
    config: &Config,
    background: &mut Background,
) -> Result<Option<Verifier>, BootstrapError> {
    let mode = config.authn.mode();
    match &config.authn {
        // template:begin oidc-jwt:bootstrap-prepare-auth-jwt
        AuthnConfig::OidcJwt {
            issuer,
            audience,
            token_profile,
            algorithms,
            jwks_uri,
            jwks_max_age,
        } => {
            let issuer = issuer_url(mode, issuer)?;
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
                    jwks_max_age: *jwks_max_age,
                },
                background.child_token(),
            )
            .await
            .map_err(|source| jwt_preparation_error(mode, jwks_uri.is_some(), source))?;
            background.spawn("jwt_refresh", refresh);
            Ok(Some(verifier))
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
            const INTROSPECTION_ENDPOINT_KEY: &str = "authn.introspection_endpoint";
            let issuer = issuer_url(mode, issuer)?;
            let endpoint = infra_bearerauthn::EndpointUrl::parse(introspection_endpoint)
                .map_err(preparation_error(mode, INTROSPECTION_ENDPOINT_KEY))?;
            let provider_concurrency = std::num::NonZeroUsize::try_from(*provider_concurrency)
                .map_err(|_| {
                    service_config::ValidationError::new(
                        "authn.provider_concurrency",
                        "does not fit this platform",
                    )
                })?;
            let cache =
                infra_bearerauthn::IntrospectionCacheOptions::try_new(*cache_capacity, *cache_ttl)
                    .map_err(preparation_error(
                        mode,
                        "authn.cache_capacity/authn.cache_ttl",
                    ))?;
            infra_bearerauthn::prepare_introspection(infra_bearerauthn::IntrospectionOptions {
                issuer,
                audiences: audience.as_slice().to_vec(),
                endpoint,
                client_id: introspection_client_id.clone(),
                client_secret: introspection_client_secret.clone(),
                provider_concurrency,
                cache: cache_enabled.then_some(cache),
            })
            .map(Some)
            .map_err(preparation_error(mode, INTROSPECTION_ENDPOINT_KEY))
        }
        // template:end oidc-introspection:bootstrap-prepare-auth-introspection
        AuthnConfig::None => Ok(None),
    }
}

fn preparation_error(
    mode: &'static str,
    key: &'static str,
) -> impl FnOnce(infra_bearerauthn::PreparationError) -> BootstrapError {
    move |source| BootstrapError::AuthenticationPreparation { mode, key, source }
}

fn issuer_url(
    mode: &'static str,
    value: &str,
) -> Result<infra_bearerauthn::IssuerUrl, BootstrapError> {
    infra_bearerauthn::IssuerUrl::parse(value).map_err(preparation_error(mode, ISSUER_KEY))
}

// template:begin oidc-jwt:bootstrap-auth-jwt-algorithm-converter
const JWKS_URI_KEY: &str = "authn.jwks_uri";

fn jwt_preparation_error(
    mode: &'static str,
    explicit_jwks_uri: bool,
    source: infra_bearerauthn::PreparationError,
) -> BootstrapError {
    let config_key = preparation_config_key(source.phase(), explicit_jwks_uri);
    BootstrapError::AuthenticationPreparation {
        mode,
        key: config_key,
        source,
    }
}

fn preparation_config_key(
    phase: infra_bearerauthn::PreparationPhase,
    explicit_jwks_uri: bool,
) -> &'static str {
    match phase {
        infra_bearerauthn::PreparationPhase::Jwks if explicit_jwks_uri => JWKS_URI_KEY,
        infra_bearerauthn::PreparationPhase::Jwks
        | infra_bearerauthn::PreparationPhase::Discovery
        | infra_bearerauthn::PreparationPhase::Options
        | infra_bearerauthn::PreparationPhase::Client => ISSUER_KEY,
    }
}

fn jwks_endpoint(
    mode: &'static str,
    value: Option<&str>,
) -> Result<Option<infra_bearerauthn::EndpointUrl>, BootstrapError> {
    value
        .map(infra_bearerauthn::EndpointUrl::parse)
        .transpose()
        .map_err(preparation_error(mode, JWKS_URI_KEY))
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

// template:begin oidc-jwt:bootstrap-auth-jwt-key-test
#[cfg(test)]
mod jwt_tests {
    use infra_bearerauthn::PreparationPhase;

    use super::*;

    #[test]
    fn a_jwks_failure_names_the_explicit_key_set_only() {
        assert_eq!(
            preparation_config_key(PreparationPhase::Jwks, true),
            "authn.jwks_uri"
        );
        assert_eq!(
            preparation_config_key(PreparationPhase::Jwks, false),
            "authn.issuer"
        );
        assert_eq!(
            preparation_config_key(PreparationPhase::Discovery, true),
            "authn.issuer"
        );
        assert_eq!(
            preparation_config_key(PreparationPhase::Options, true),
            "authn.issuer"
        );
    }
}
// template:end oidc-jwt:bootstrap-auth-jwt-key-test
