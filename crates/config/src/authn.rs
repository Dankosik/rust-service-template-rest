//! Optional inbound authentication profile and its static trust inputs.
//!
//! This section validates configuration shape only. Provider URL admission,
//! discovery, credentials use, and verification belong to the authentication
//! adapter.

// template:begin oidc-introspection:authn-nonzero-import
use std::{num::NonZeroU32, time::Duration};
// template:end oidc-introspection:authn-nonzero-import

// template:begin oidc-introspection:authn-secrecy-import
use secrecy::SecretString;
// template:end oidc-introspection:authn-secrecy-import
use serde::{Deserialize, Deserializer};

// template:begin oidc-introspection:authn-occupied-secret-import
use crate::secret_policy::occupied_secret;
// template:end oidc-introspection:authn-occupied-secret-import
use crate::ValidationError;

/// Exact audiences accepted by an authentication profile.
///
/// Configuration accepts either one string or a nonempty list. Values retain
/// their spelling; duplicate list entries collapse to their first occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Audiences(Vec<String>);

impl Audiences {
    /// Borrow the normalized exact audience values.
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    /// Consume the input into normalized exact audience values.
    #[must_use]
    pub fn into_inner(self) -> Vec<String> {
        self.0
    }
}

impl<'de> Deserialize<'de> for Audiences {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            One(String),
            Many(Vec<String>),
        }

        let values = match Input::deserialize(deserializer)? {
            Input::One(value) => vec![value],
            Input::Many(values) => values,
        };
        Self::new(values).map_err(serde::de::Error::custom)
    }
}

impl Audiences {
    fn new(values: Vec<String>) -> Result<Self, &'static str> {
        if values.is_empty() {
            return Err("must be a nonempty string or list of nonblank exact values");
        }
        if values.iter().any(|value| value.trim().is_empty()) {
            return Err("values must be nonblank");
        }

        let mut normalized = Vec::with_capacity(values.len());
        for value in values {
            if !normalized.contains(&value) {
                normalized.push(value);
            }
        }
        Ok(Self(normalized))
    }
}

// template:begin oidc-jwt:authn-token-profile
/// Additional JWT access-token requirements selected by an OIDC JWT profile.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TokenProfile {
    /// Accept the service's resource-server access-token contract.
    #[default]
    ResourceServer,
    /// Require RFC 9068 access-token typing and claims.
    Rfc9068,
}
// template:end oidc-jwt:authn-token-profile

// template:begin oidc-jwt:authn-algorithm
/// Signature algorithms this service admits for JWT access tokens.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
pub enum JwtAlgorithm {
    /// RSA PKCS#1 v1.5 with SHA-256.
    #[default]
    #[serde(rename = "RS256")]
    Rs256,
    /// ECDSA P-256 with SHA-256.
    #[serde(rename = "ES256")]
    Es256,
    /// RSA PSS with SHA-256.
    #[serde(rename = "PS256")]
    Ps256,
    /// Edwards-curve Ed25519 signatures.
    #[serde(rename = "EdDSA")]
    EdDsa,
}
// template:end oidc-jwt:authn-algorithm

/// Static configuration for the optional inbound authentication profiles.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub enum AuthnConfig {
    /// No provider is configured. This is the omitted-section default.
    None {},
    // template:begin oidc-jwt:authn-config-jwt-variant
    /// Verify signed OIDC access tokens against discovered keys.
    OidcJwt {
        /// Exact OIDC issuer used for discovery and verified-claim comparison.
        issuer: String,
        /// Exact accepted audiences, from one string or a nonempty list.
        audience: Audiences,
        /// JWT access-token contract. Missing uses `resource-server`.
        #[serde(default)]
        token_profile: TokenProfile,
        /// Accepted signature algorithms. Missing uses `[RS256]`.
        #[serde(default = "default_algorithms")]
        algorithms: Vec<JwtAlgorithm>,
    },
    // template:end oidc-jwt:authn-config-jwt-variant
    // template:begin oidc-introspection:authn-config-introspection-variant
    /// Verify opaque access tokens with the configured RFC 7662 endpoint.
    OidcIntrospection {
        /// Exact OIDC issuer used for verified-claim comparison.
        issuer: String,
        /// Exact accepted audiences, from one string or a nonempty list.
        audience: Audiences,
        /// Fixed RFC 7662 endpoint, validated by the adapter before I/O.
        introspection_endpoint: String,
        /// Client identifier for the fixed introspection credential.
        introspection_client_id: String,
        /// Environment-only client secret. Missing or blank is rejected.
        #[serde(default, deserialize_with = "occupied_secret")]
        introspection_client_secret: Option<SecretString>,
        /// Immediate provider-exchange capacity. Missing uses 32.
        #[serde(
            default = "default_provider_concurrency",
            deserialize_with = "deserialize_scalar"
        )]
        provider_concurrency: NonZeroU32,
        /// Reuse verified positive results until their fixed expiry. Default off.
        #[serde(default, deserialize_with = "deserialize_scalar")]
        cache_enabled: bool,
        /// Maximum retained results, from 1 through 1024. Missing uses 256.
        #[serde(
            default = "default_cache_capacity",
            deserialize_with = "deserialize_scalar"
        )]
        cache_capacity: usize,
        /// Maximum fixed retention, from 1s through 5m. Missing uses 30s.
        #[serde(default = "default_cache_ttl", with = "humantime_serde")]
        cache_ttl: Duration,
    },
    // template:end oidc-introspection:authn-config-introspection-variant
}

impl Default for AuthnConfig {
    fn default() -> Self {
        Self::None {}
    }
}

// template:begin oidc-jwt:authn-default-algorithms
fn default_algorithms() -> Vec<JwtAlgorithm> {
    vec![JwtAlgorithm::Rs256]
}
// template:end oidc-jwt:authn-default-algorithms

// template:begin oidc-introspection:authn-default-provider-concurrency
fn default_provider_concurrency() -> NonZeroU32 {
    const { NonZeroU32::new(32).expect("32 is nonzero") }
}

const fn default_cache_capacity() -> usize {
    // Bound retained results without implying a throughput target.
    256
}

const fn default_cache_ttl() -> Duration {
    // Keep the opt-in delay in revocation detection short.
    Duration::from_secs(30)
}

// The tagged enum buffers values before decoding fields, so config-rs cannot
// coerce environment strings here. Adapt only these scalar fields; trust and
// credential strings must retain their exact bytes.
fn deserialize_scalar<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + std::str::FromStr,
    T::Err: std::fmt::Display,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Input<T> {
        Typed(T),
        Text(String),
    }

    match Input::<T>::deserialize(deserializer)? {
        Input::Typed(value) => Ok(value),
        Input::Text(value) => value.parse().map_err(serde::de::Error::custom),
    }
}
// template:end oidc-introspection:authn-default-provider-concurrency

impl AuthnConfig {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        match self {
            // template:begin oidc-jwt:authn-config-jwt-validation
            Self::OidcJwt {
                issuer,
                audience,
                algorithms,
                ..
            } => {
                require_nonblank("authn.issuer", issuer, "oidc-jwt")?;
                validate_audience(audience)?;
                if algorithms.is_empty() {
                    return Err(ValidationError::new(
                        "authn.algorithms",
                        "must contain at least one supported algorithm",
                    ));
                }
                Ok(())
            }
            // template:end oidc-jwt:authn-config-jwt-validation
            // template:begin oidc-introspection:authn-config-introspection-validation
            Self::OidcIntrospection {
                issuer,
                audience,
                introspection_endpoint,
                introspection_client_id,
                introspection_client_secret,
                cache_capacity,
                cache_ttl,
                ..
            } => {
                require_nonblank("authn.issuer", issuer, "oidc-introspection")?;
                validate_audience(audience)?;
                require_nonblank(
                    "authn.introspection_endpoint",
                    introspection_endpoint,
                    "oidc-introspection",
                )?;
                require_nonblank(
                    "authn.introspection_client_id",
                    introspection_client_id,
                    "oidc-introspection",
                )?;
                if introspection_client_secret.is_none() {
                    return Err(ValidationError::new(
                        "authn.introspection_client_secret",
                        "is required when authn.mode = oidc-introspection",
                    ));
                }
                if !(1..=1024).contains(cache_capacity) {
                    return Err(ValidationError::new(
                        "authn.cache_capacity",
                        "must be between 1 and 1024, even when caching is disabled",
                    ));
                }
                if !(Duration::from_secs(1)..=Duration::from_secs(300)).contains(cache_ttl) {
                    return Err(ValidationError::new(
                        "authn.cache_ttl",
                        "must be between 1s and 5m, even when caching is disabled",
                    ));
                }
                Ok(())
            }
            // template:end oidc-introspection:authn-config-introspection-validation
            Self::None {} => Ok(()),
        }
    }
}

fn require_nonblank(key: &str, value: &str, mode: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::new(
            key,
            format!("is required when authn.mode = {mode}"),
        ));
    }
    Ok(())
}

fn validate_audience(audience: &Audiences) -> Result<(), ValidationError> {
    if audience.0.is_empty() {
        return Err(ValidationError::new(
            "authn.audience",
            "must be a nonempty string or list of nonblank exact values",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Result<AuthnConfig, toml::de::Error> {
        toml::from_str(toml)
    }

    #[test]
    fn omitted_section_defaults_to_inert_none() {
        let config = AuthnConfig::default();
        assert!(matches!(config, AuthnConfig::None {}));
        config.validate().unwrap();
    }

    // template:begin oidc-jwt:authn-config-jwt-tests
    #[test]
    fn jwt_normalizes_scalar_or_list_audiences_without_changing_values() {
        let scalar = parse(
            r#"
            mode = "oidc-jwt"
            issuer = " issuer grammar belongs to the adapter "
            audience = "service"
            "#,
        )
        .unwrap();
        let list = parse(
            r#"
            mode = "oidc-jwt"
            issuer = "issuer"
            audience = ["service", " api ", "service"]
            algorithms = ["ES256", "EdDSA"]
            token_profile = "rfc9068"
            "#,
        )
        .unwrap();

        // URL grammar is adapter-owned; only required config shape is checked here.
        scalar.validate().unwrap();

        let AuthnConfig::OidcJwt {
            audience,
            token_profile,
            algorithms,
            ..
        } = scalar
        else {
            panic!("expected OIDC JWT configuration");
        };
        assert_eq!(audience.as_slice(), ["service"]);
        assert_eq!(token_profile, TokenProfile::ResourceServer);
        assert_eq!(algorithms, [JwtAlgorithm::Rs256]);

        let AuthnConfig::OidcJwt {
            audience,
            token_profile,
            algorithms,
            ..
        } = list
        else {
            panic!("expected OIDC JWT configuration");
        };
        assert_eq!(audience.as_slice(), ["service", " api "]);
        assert_eq!(token_profile, TokenProfile::Rfc9068);
        assert_eq!(algorithms, [JwtAlgorithm::Es256, JwtAlgorithm::EdDsa]);
    }

    #[test]
    fn jwt_rejects_empty_algorithms_and_invalid_audience_shape() {
        let empty_algorithms = parse(
            r#"
            mode = "oidc-jwt"
            issuer = "issuer"
            audience = "service"
            algorithms = []
            "#,
        )
        .unwrap();
        assert_eq!(
            empty_algorithms.validate().unwrap_err().key,
            "authn.algorithms"
        );

        for audience in ["[]", "[\"service\", \"  \"]", "\"  \""] {
            let source =
                format!("mode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = {audience}\n");
            assert!(parse(&source).is_err(), "{source}");
        }
    }
    // template:end oidc-jwt:authn-config-jwt-tests

    // template:begin oidc-introspection:authn-config-introspection-tests
    #[test]
    fn introspection_requires_structural_inputs_and_uses_nonzero_default_capacity() {
        let config = parse(
            r#"
            mode = "oidc-introspection"
            issuer = "issuer"
            audience = ["service"]
            introspection_endpoint = "endpoint"
            introspection_client_id = "client"
            introspection_client_secret = "secret"
            "#,
        )
        .unwrap();
        let AuthnConfig::OidcIntrospection {
            provider_concurrency,
            cache_enabled,
            cache_capacity,
            cache_ttl,
            ..
        } = &config
        else {
            panic!("expected OIDC introspection configuration");
        };
        assert_eq!(provider_concurrency.get(), 32);
        assert!(!cache_enabled);
        assert_eq!(*cache_capacity, 256);
        assert_eq!(*cache_ttl, Duration::from_secs(30));
        config.validate().unwrap();

        let missing_secret = parse(
            r#"
            mode = "oidc-introspection"
            issuer = "issuer"
            audience = "service"
            introspection_endpoint = "endpoint"
            introspection_client_id = "client"
            "#,
        )
        .unwrap();
        assert_eq!(
            missing_secret.validate().unwrap_err().key,
            "authn.introspection_client_secret"
        );
        assert!(
            parse(
                r#"
            mode = "oidc-introspection"
            issuer = "issuer"
            audience = "service"
            introspection_endpoint = "endpoint"
            introspection_client_id = "client"
            introspection_client_secret = "secret"
            provider_concurrency = 0
            "#,
            )
            .is_err()
        );
    }

    #[test]
    fn introspection_validates_cache_bounds_even_when_disabled() {
        let base = r#"
            mode = "oidc-introspection"
            issuer = "issuer"
            audience = "service"
            introspection_endpoint = "endpoint"
            introspection_client_id = "client"
            introspection_client_secret = "secret"
        "#;
        for enabled in [false, true] {
            for (capacity, ttl) in [(1, "1s"), (1024, "5m")] {
                let source = format!(
                    "{base}\ncache_enabled = {enabled}\ncache_capacity = {capacity}\ncache_ttl = \"{ttl}\"\n"
                );
                parse(&source).unwrap().validate().unwrap();
            }
            for (field, value) in [
                ("cache_capacity", "0"),
                ("cache_capacity", "1025"),
                ("cache_ttl", "\"999ms\""),
                ("cache_ttl", "\"301s\""),
            ] {
                let source = format!("{base}\ncache_enabled = {enabled}\n{field} = {value}\n");
                assert_eq!(
                    parse(&source).unwrap().validate().unwrap_err().key,
                    format!("authn.{field}"),
                    "{source}"
                );
            }
        }
    }
    // template:end oidc-introspection:authn-config-introspection-tests

    #[test]
    fn modes_reject_foreign_provider_fields() {
        assert!(matches!(
            parse("mode = \"none\"\n").unwrap(),
            AuthnConfig::None {}
        ));
        for source in [
            "mode = \"none\"\nissuer = \"issuer\"\n",
            "mode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = \"service\"\nintrospection_endpoint = \"endpoint\"\n",
            "mode = \"oidc-introspection\"\nissuer = \"issuer\"\naudience = \"service\"\nintrospection_endpoint = \"endpoint\"\nintrospection_client_id = \"client\"\nintrospection_client_secret = \"secret\"\nalgorithms = [\"RS256\"]\n",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
        for mode in ["none", "oidc-jwt"] {
            let base = if mode == "none" {
                "mode = \"none\"\n"
            } else {
                "mode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = \"service\"\n"
            };
            for field in [
                "cache_enabled = false",
                "cache_capacity = 256",
                "cache_ttl = \"30s\"",
            ] {
                let source = format!("{base}{field}\n");
                assert!(parse(&source).is_err(), "{source}");
            }
        }
    }
}
