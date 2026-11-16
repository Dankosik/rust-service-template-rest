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
use crate::de::{blank_secret_as_none, deserialize_scalar};
// template:end oidc-introspection:authn-occupied-secret-import
use crate::de::value_free;
use crate::validate::{Rejected, ValidationError, Violations, required_when};

pub(crate) const AUDIENCE_INPUT_FORM: &str =
    "`authn.audience` must be a string or a list of strings";

/// Exact audiences accepted by an authentication profile.
///
/// Configuration accepts either one string or a nonempty list. Values retain
/// their spelling; duplicate list entries collapse to their first occurrence.
/// `APP__AUTHN__AUDIENCE` takes one exact value. `APP__AUTHN__AUDIENCES`
/// takes a TOML array of strings and overrides the same key.
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

        let input = Input::deserialize(deserializer)
            .map_err(|_| value_free::<D::Error>(AUDIENCE_INPUT_FORM))?;
        let values = match input {
            Input::One(value) => vec![value],
            Input::Many(values) => values,
        };
        Self::try_from(values)
            .map_err(|message| value_free(format_args!("`authn.audience` {message}")))
    }
}

impl TryFrom<Vec<String>> for Audiences {
    type Error = &'static str;

    fn try_from(values: Vec<String>) -> Result<Self, Self::Error> {
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
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TokenProfile {
    #[default]
    ResourceServer,
    /// Require RFC 9068 access-token typing and claims.
    Rfc9068,
}
// template:end oidc-jwt:authn-token-profile

// template:begin oidc-jwt:authn-algorithm
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
pub enum JwtAlgorithm {
    #[default]
    #[serde(rename = "RS256")]
    Rs256,
    #[serde(rename = "ES256")]
    Es256,
    #[serde(rename = "PS256")]
    Ps256,
    #[serde(rename = "EdDSA")]
    EdDsa,
}
// template:end oidc-jwt:authn-algorithm

#[derive(Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum AuthnInput {
    /// No provider is configured. This is the omitted-section default.
    None {},
    // template:begin oidc-jwt:authn-config-jwt-variant
    OidcJwt {
        issuer: String,
        audience: Audiences,
        /// JWT access-token contract. Missing uses `resource-server`.
        #[serde(default)]
        token_profile: TokenProfile,
        /// Accepted signature algorithms. `APP__AUTHN__ALGORITHMS` takes a comma-separated list.
        /// Missing uses `[RS256]`.
        #[serde(
            default = "default_algorithms",
            deserialize_with = "deserialize_algorithms"
        )]
        algorithms: Vec<JwtAlgorithm>,
        /// Explicit JWKS endpoint. Missing uses OIDC discovery, then RFC 8414
        /// metadata.
        #[serde(default)]
        jwks_uri: Option<String>,
    },
    // template:end oidc-jwt:authn-config-jwt-variant
    // template:begin oidc-introspection:authn-config-introspection-variant
    OidcIntrospection {
        issuer: String,
        audience: Audiences,
        introspection_endpoint: String,
        introspection_client_id: String,
        /// Environment-only client secret. Missing or blank is rejected.
        /// Whitespace determines absence only; a present credential retains
        /// its exact bytes for the provider, including surrounding whitespace.
        #[serde(default, deserialize_with = "blank_secret_as_none")]
        introspection_client_secret: Option<SecretString>,
        /// Immediate provider-exchange capacity. Missing uses 32.
        #[serde(
            default = "default_provider_concurrency",
            deserialize_with = "deserialize_provider_concurrency"
        )]
        provider_concurrency: NonZeroU32,
        /// Reuse verified positive results until their fixed expiry. Default off.
        #[serde(default, deserialize_with = "deserialize_cache_enabled")]
        cache_enabled: bool,
        /// Maximum retained results, from 1 through 1024. Missing uses 256.
        #[serde(
            default = "default_cache_capacity",
            deserialize_with = "deserialize_cache_capacity"
        )]
        cache_capacity: usize,
        /// Maximum fixed retention, from 1s through 5m. Missing uses 30s.
        #[serde(default = "default_cache_ttl", with = "humantime_serde")]
        cache_ttl: Duration,
    },
    // template:end oidc-introspection:authn-config-introspection-variant
}

impl std::fmt::Debug for AuthnInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthnInput { .. }")
    }
}

impl Default for AuthnInput {
    fn default() -> Self {
        Self::None {}
    }
}

/// The admitted inbound authentication profile.
#[derive(Clone, Default)]
pub enum AuthnConfig {
    #[default]
    None,
    // template:begin oidc-jwt:authn-admitted-jwt-variant
    OidcJwt {
        issuer: String,
        audience: Audiences,
        token_profile: TokenProfile,
        /// Nonempty.
        algorithms: Vec<JwtAlgorithm>,
        /// `None` uses OIDC discovery, then RFC 8414 metadata.
        jwks_uri: Option<String>,
    },
    // template:end oidc-jwt:authn-admitted-jwt-variant
    // template:begin oidc-introspection:authn-admitted-introspection-variant
    OidcIntrospection {
        issuer: String,
        audience: Audiences,
        introspection_endpoint: String,
        introspection_client_id: String,
        introspection_client_secret: SecretString,
        provider_concurrency: NonZeroU32,
        cache_enabled: bool,
        cache_capacity: usize,
        cache_ttl: Duration,
    },
    // template:end oidc-introspection:authn-admitted-introspection-variant
}

impl std::fmt::Debug for AuthnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthnConfig")
            .field("mode", &self.mode())
            .finish_non_exhaustive()
    }
}

// template:begin oidc-jwt:authn-default-algorithms
fn default_algorithms() -> Vec<JwtAlgorithm> {
    vec![JwtAlgorithm::Rs256]
}

fn deserialize_algorithms<'de, D>(deserializer: D) -> Result<Vec<JwtAlgorithm>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Input {
        List(Vec<JwtAlgorithm>),
        Text(String),
    }

    let input = Input::deserialize(deserializer).map_err(|_| {
        value_free::<D::Error>(
            "`authn.algorithms` must be a list of supported JWT algorithms or a comma-separated string",
        )
    })?;
    match input {
        Input::List(values) => Ok(values),
        Input::Text(value) => value
            .split(',')
            .map(|value| {
                JwtAlgorithm::deserialize(serde::de::value::StrDeserializer::new(value.trim()))
            })
            .collect::<Result<_, _>>()
            .map_err(|_: D::Error| {
                value_free("`authn.algorithms` must contain only supported JWT algorithms")
            }),
    }
}
// template:end oidc-jwt:authn-default-algorithms

// template:begin oidc-introspection:authn-default-provider-concurrency
fn default_provider_concurrency() -> NonZeroU32 {
    const { NonZeroU32::new(32).expect("32 is nonzero") }
}

const fn default_cache_capacity() -> usize {
    256
}

const fn default_cache_ttl() -> Duration {
    Duration::from_secs(30)
}

// The tagged enum buffers field values, so config-rs reports failures at
// `authn` alone. These messages name the rejected field.
fn deserialize_provider_concurrency<'de, D>(deserializer: D) -> Result<NonZeroU32, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_scalar(
        deserializer,
        "`authn.provider_concurrency` must be an integer from 1 to 4294967295",
    )
}

fn deserialize_cache_enabled<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_scalar(deserializer, "`authn.cache_enabled` must be true or false")
}

fn deserialize_cache_capacity<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_scalar(
        deserializer,
        "`authn.cache_capacity` must be an integer from 1 to 1024",
    )
}

// template:end oidc-introspection:authn-default-provider-concurrency

impl AuthnConfig {
    #[must_use]
    pub const fn mode(&self) -> &'static str {
        match self {
            Self::None => "none",
            // template:begin oidc-jwt:authn-config-jwt-mode
            Self::OidcJwt { .. } => "oidc-jwt",
            // template:end oidc-jwt:authn-config-jwt-mode
            // template:begin oidc-introspection:authn-config-introspection-mode
            Self::OidcIntrospection { .. } => "oidc-introspection",
            // template:end oidc-introspection:authn-config-introspection-mode
        }
    }
}

impl AuthnInput {
    pub(crate) fn admit(self, v: &mut Violations) -> Result<AuthnConfig, Rejected> {
        match self {
            // template:begin oidc-jwt:authn-config-jwt-validation
            Self::OidcJwt {
                issuer,
                audience,
                token_profile,
                algorithms,
                jwks_uri,
            } => {
                let named = v.take(required_when(
                    "authn.issuer",
                    &issuer,
                    "authn.mode = oidc-jwt",
                ));
                let located = if jwks_uri.as_deref().is_some_and(|uri| uri.trim().is_empty()) {
                    Err(v.reject(ValidationError::new(
                        "authn.jwks_uri",
                        "must not be blank when set",
                    )))
                } else {
                    Ok(())
                };
                let signed = if algorithms.is_empty() {
                    Err(v.reject(ValidationError::new(
                        "authn.algorithms",
                        "must contain at least one supported algorithm",
                    )))
                } else {
                    Ok(())
                };
                named?;
                located?;
                signed?;
                Ok(AuthnConfig::OidcJwt {
                    issuer,
                    audience,
                    token_profile,
                    algorithms,
                    jwks_uri,
                })
            }
            // template:end oidc-jwt:authn-config-jwt-validation
            // template:begin oidc-introspection:authn-config-introspection-validation
            Self::OidcIntrospection {
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
                use shared_rules::oidc_introspection::{CACHE_CAPACITY, CACHE_TTL};

                let condition = "authn.mode = oidc-introspection";
                let named = v.take(required_when("authn.issuer", &issuer, condition));
                let located = v.take(required_when(
                    "authn.introspection_endpoint",
                    &introspection_endpoint,
                    condition,
                ));
                let identified = v.take(required_when(
                    "authn.introspection_client_id",
                    &introspection_client_id,
                    condition,
                ));
                let secret = v.take(crate::validate::required(
                    "authn.introspection_client_secret",
                    introspection_client_secret,
                    condition,
                ));
                let capacity = v.take(crate::validate::int_range(
                    "authn.cache_capacity",
                    cache_capacity as u64,
                    *CACHE_CAPACITY.start() as u64,
                    *CACHE_CAPACITY.end() as u64,
                ));
                let retention = v.take(crate::validate::duration_range(
                    "authn.cache_ttl",
                    cache_ttl,
                    *CACHE_TTL.start(),
                    *CACHE_TTL.end(),
                ));
                named?;
                located?;
                identified?;
                capacity?;
                retention?;
                Ok(AuthnConfig::OidcIntrospection {
                    issuer,
                    audience,
                    introspection_endpoint,
                    introspection_client_id,
                    introspection_client_secret: secret?,
                    provider_concurrency,
                    cache_enabled,
                    cache_capacity,
                    cache_ttl,
                })
            }
            // template:end oidc-introspection:authn-config-introspection-validation
            Self::None {} => Ok(AuthnConfig::None),
        }
    }
}

#[cfg(test)]
mod tests;
