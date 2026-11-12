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
use crate::de::blank_secret_as_none;
// template:end oidc-introspection:authn-occupied-secret-import
use crate::de::value_free;
use crate::validate::{ValidationError, required_when};

/// Exact audiences accepted by an authentication profile.
///
/// Configuration accepts either one string or a nonempty list. Values retain
/// their spelling; duplicate list entries collapse to their first occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Audiences(Vec<String>);

impl Audiences {
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
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
        Self::try_new(values).map_err(value_free)
    }
}

impl Audiences {
    fn try_new(values: Vec<String>) -> Result<Self, &'static str> {
        if values.is_empty() {
            return Err("must be a nonempty string or list of nonblank exact values");
        }
        if values.iter().any(|value| value.trim().is_empty()) {
            return Err("values must be nonblank");
        }

        let mut deduplicated = Vec::with_capacity(values.len());
        for value in values {
            if !deduplicated.contains(&value) {
                deduplicated.push(value);
            }
        }
        Ok(Self(deduplicated))
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
#[derive(Clone, Deserialize)]
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
        /// Explicit JWKS endpoint. Missing uses OIDC discovery, then RFC 8414
        /// metadata.
        #[serde(default)]
        jwks_uri: Option<String>,
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
        /// Whitespace determines absence only; a present credential retains
        /// its exact bytes for the provider, including surrounding whitespace.
        #[serde(default, deserialize_with = "blank_secret_as_none")]
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

/// Leaves trust and credential inputs out of `Debug` so they never reach logs.
impl std::fmt::Debug for AuthnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthnConfig")
            .field("mode", &self.mode())
            .finish_non_exhaustive()
    }
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
    /// The `authn.mode` value that selected this variant.
    #[must_use]
    pub const fn mode(&self) -> &'static str {
        match self {
            Self::None {} => "none",
            // template:begin oidc-jwt:authn-config-jwt-mode
            Self::OidcJwt { .. } => "oidc-jwt",
            // template:end oidc-jwt:authn-config-jwt-mode
            // template:begin oidc-introspection:authn-config-introspection-mode
            Self::OidcIntrospection { .. } => "oidc-introspection",
            // template:end oidc-introspection:authn-config-introspection-mode
        }
    }

    // template:begin oidc-introspection:authn-required-introspection-secret
    /// The client secret that `oidc-introspection` requires.
    ///
    /// # Errors
    ///
    /// Returns an error when the mode is not `oidc-introspection` or the
    /// secret is absent.
    pub fn required_introspection_client_secret(&self) -> Result<&SecretString, ValidationError> {
        let secret = if let Self::OidcIntrospection {
            introspection_client_secret,
            ..
        } = self
        {
            introspection_client_secret.as_ref()
        } else {
            None
        };
        crate::validate::required(
            "authn.introspection_client_secret",
            secret,
            "authn.mode = oidc-introspection",
        )
    }
    // template:end oidc-introspection:authn-required-introspection-secret

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        match self {
            // template:begin oidc-jwt:authn-config-jwt-validation
            Self::OidcJwt {
                issuer,
                algorithms,
                jwks_uri,
                ..
            } => {
                required_when("authn.issuer", issuer, "authn.mode = oidc-jwt")?;
                if jwks_uri.as_deref().is_some_and(|uri| uri.trim().is_empty()) {
                    return Err(ValidationError::new(
                        "authn.jwks_uri",
                        "must not be blank when set",
                    ));
                }
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
                introspection_endpoint,
                introspection_client_id,
                ..
            } => {
                required_when("authn.issuer", issuer, "authn.mode = oidc-introspection")?;
                required_when(
                    "authn.introspection_endpoint",
                    introspection_endpoint,
                    "authn.mode = oidc-introspection",
                )?;
                required_when(
                    "authn.introspection_client_id",
                    introspection_client_id,
                    "authn.mode = oidc-introspection",
                )?;
                self.required_introspection_client_secret()?;
                Ok(())
            }
            // template:end oidc-introspection:authn-config-introspection-validation
            Self::None {} => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
