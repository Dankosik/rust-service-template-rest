// template:begin oidc-introspection:authn-nonzero-import
use std::num::{NonZeroU32, NonZeroUsize};
use std::time::Duration;
// template:end oidc-introspection:authn-nonzero-import

// template:begin oidc-introspection:authn-secrecy-import
use secrecy::SecretString;
// template:end oidc-introspection:authn-secrecy-import
use itertools::Itertools as _;
use serde::de::Unexpected;
use serde::{Deserialize, Deserializer};
use serde_with::DeserializeAs;

use crate::validate::{
    Rejected, ValidationError, Violations, missing, required, required_non_blank,
};

/// Exact audiences accepted by an authentication profile.
///
/// `authn.audience` takes one string or a nonempty list. `APP__AUTHN__AUDIENCES`
/// takes a TOML array of strings written as one value. Setting both keys is
/// rejected. Values retain their spelling; duplicate list entries collapse to
/// their first occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Audiences(Vec<String>);

impl Audiences {
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    fn from_values(values: impl IntoIterator<Item = String>) -> Result<Self, &'static str> {
        let deduplicated: Vec<String> = values
            .into_iter()
            .map(|value| {
                if value.trim().is_empty() {
                    Err("values must be nonblank")
                } else {
                    Ok(value)
                }
            })
            .process_results(|iter| iter.unique().collect())?;
        if deduplicated.is_empty() {
            return Err("must be a nonempty string or list of nonblank exact values");
        }
        Ok(Self(deduplicated))
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

        let input = Input::deserialize(deserializer).map_err(|_| {
            serde::de::Error::invalid_value(
                Unexpected::Other("a non-string audience"),
                &"a string or a list of strings",
            )
        })?;
        match input {
            Input::One(value) => Self::from_values([value]),
            Input::Many(values) => Self::from_values(values),
        }
        .map_err(|_| {
            serde::de::Error::invalid_value(
                Unexpected::Other("an empty or blank audience"),
                &"a nonempty string or list of nonblank exact values",
            )
        })
    }
}

impl TryFrom<Vec<String>> for Audiences {
    type Error = &'static str;

    fn try_from(values: Vec<String>) -> Result<Self, Self::Error> {
        Self::from_values(values)
    }
}

struct AudienceList;

impl<'de> DeserializeAs<'de, Audiences> for AudienceList {
    fn deserialize_as<D>(deserializer: D) -> Result<Audiences, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        let invalid = || {
            serde::de::Error::invalid_value(
                Unexpected::Other("a malformed audience list"),
                &"a TOML array of nonblank strings",
            )
        };
        let value = toml::de::ValueDeserializer::parse(&raw).map_err(|_| invalid())?;
        let values: Vec<String> = Vec::deserialize(value).map_err(|_| invalid())?;
        Audiences::from_values(values).map_err(|_| invalid())
    }
}

// template:begin oidc-jwt:authn-token-profile
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TokenProfile {
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

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum AuthnMode {
    /// No provider is configured. This is the omitted-section default.
    #[default]
    None,
    // template:begin oidc-jwt:authn-mode-jwt
    OidcJwt,
    // template:end oidc-jwt:authn-mode-jwt
    // template:begin oidc-introspection:authn-mode-introspection
    OidcIntrospection,
    // template:end oidc-introspection:authn-mode-introspection
}

#[serde_with::serde_as]
#[derive(Clone, derive_more::Debug, Default, Deserialize)]
#[debug("AuthnInput {{ .. }}")]
#[serde(deny_unknown_fields, default)]
pub(crate) struct AuthnInput {
    mode: AuthnMode,
    issuer: Option<String>,
    audience: Option<Audiences>,
    #[serde(default)]
    #[serde_as(as = "Option<AudienceList>")]
    audiences: Option<Audiences>,
    // template:begin oidc-jwt:authn-config-jwt-variant
    /// Required JWT access-token contract.
    token_profile: Option<TokenProfile>,
    /// Accepted signature algorithms. `APP__AUTHN__ALGORITHMS` takes a comma-separated list.
    /// Missing uses `[RS256]`.
    #[serde(default)]
    #[serde_as(as = "Option<AlgorithmList>")]
    algorithms: Option<Vec<JwtAlgorithm>>,
    /// Explicit JWKS endpoint. Missing uses OIDC discovery, then RFC 8414
    /// metadata.
    jwks_uri: Option<String>,
    /// Maximum age since the verifier last acquired usable keys.
    #[serde(default)]
    #[serde_as(as = "Option<crate::de::StrictDuration>")]
    jwks_max_age: Option<std::time::Duration>,
    // template:end oidc-jwt:authn-config-jwt-variant
    // template:begin oidc-introspection:authn-config-introspection-variant
    introspection_endpoint: Option<String>,
    introspection_client_id: Option<String>,
    /// Environment-only client secret. Missing or blank is rejected.
    /// Whitespace determines absence only; a present credential retains
    /// its exact bytes for the provider, including surrounding whitespace.
    #[serde(default)]
    #[serde_as(as = "crate::de::BlankAsNoneVerbatim")]
    introspection_client_secret: Option<SecretString>,
    /// Immediate provider-exchange capacity. Missing uses 32.
    #[serde(default)]
    #[serde_as(as = "Option<crate::de::StrictCount>")]
    provider_concurrency: Option<NonZeroU32>,
    /// Reuse verified positive results until their fixed expiry. Default off.
    #[serde(default)]
    #[serde_as(as = "Option<crate::de::StrictBool>")]
    cache_enabled: Option<bool>,
    /// Maximum retained results, from 1 through 1024. Missing uses 256.
    #[serde(default)]
    #[serde_as(as = "Option<crate::de::StrictCount>")]
    cache_capacity: Option<usize>,
    /// Maximum fixed retention, from 1s through 5m. Missing uses 30s.
    #[serde(default)]
    #[serde_as(as = "Option<crate::de::StrictDuration>")]
    cache_ttl: Option<Duration>,
    // template:end oidc-introspection:authn-config-introspection-variant
}

// template:begin oidc-introspection:authn-introspection-cache-config
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntrospectionCacheConfig {
    pub capacity: NonZeroUsize,
    pub ttl: Duration,
}
// template:end oidc-introspection:authn-introspection-cache-config

#[derive(Clone, Default)]
pub enum AuthnConfig {
    #[default]
    None,
    // template:begin oidc-jwt:authn-admitted-jwt-variant
    OidcJwt {
        issuer: String,
        audience: Audiences,
        token_profile: TokenProfile,
        algorithms: Vec<JwtAlgorithm>,
        jwks_uri: Option<String>,
        jwks_max_age: std::time::Duration,
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
        cache: Option<IntrospectionCacheConfig>,
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
fn required_jwks_max_age(
    value: Option<std::time::Duration>,
    condition: &str,
) -> Result<std::time::Duration, ValidationError> {
    let key = "authn.jwks_max_age";
    let age = required(key, value, condition)?;
    let minimum = shared_rules::oidc_jwt::MIN_JWKS_MAX_AGE;
    if age < minimum {
        return Err(ValidationError::new(
            key,
            format!("must be at least {}", crate::de::display_duration(minimum)),
        ));
    }
    Ok(age)
}

fn default_algorithms() -> Vec<JwtAlgorithm> {
    vec![JwtAlgorithm::Rs256]
}

struct AlgorithmList;

impl<'de> DeserializeAs<'de, Vec<JwtAlgorithm>> for AlgorithmList {
    fn deserialize_as<D>(deserializer: D) -> Result<Vec<JwtAlgorithm>, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::IntoDeserializer as _;

        const EXPECTED: &str = "a list of supported JWT algorithms or a comma-separated string";

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            List(Vec<JwtAlgorithm>),
            Text(String),
        }

        let input = Input::deserialize(deserializer).map_err(|_| {
            serde::de::Error::invalid_value(
                Unexpected::Other("an unsupported algorithm list"),
                &EXPECTED,
            )
        })?;
        match input {
            Input::List(values) => Ok(values),
            Input::Text(value) => value
                .split(',')
                .map(|item| JwtAlgorithm::deserialize(item.trim().into_deserializer()))
                .collect::<Result<Vec<_>, D::Error>>()
                .map_err(|_| {
                    serde::de::Error::invalid_value(
                        Unexpected::Other("an unsupported JWT algorithm"),
                        &EXPECTED,
                    )
                }),
        }
    }
}
// template:end oidc-jwt:authn-default-algorithms

// template:begin oidc-introspection:authn-default-provider-concurrency
fn default_provider_concurrency() -> NonZeroU32 {
    const { NonZeroU32::new(32).expect("32 is nonzero") }
}

const DEFAULT_CACHE_CAPACITY: usize = 256;
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(30);
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

fn admit_audience(
    audience: Option<Audiences>,
    audiences: Option<Audiences>,
    condition: &str,
    v: &mut Violations,
) -> Result<Audiences, Rejected> {
    match (audience, audiences) {
        (Some(_), Some(_)) => Err(v.reject(ValidationError::new(
            "authn.audiences",
            "cannot be set together with authn.audience",
        ))),
        (Some(audience), None) | (None, Some(audience)) => Ok(audience),
        (None, None) => Err(v.reject(missing("authn.audience", condition))),
    }
}

impl AuthnInput {
    #[allow(
        clippy::unused_self,
        unused_variables,
        reason = "no JWT fields exist when the oidc-jwt profile is unselected"
    )]
    fn reject_jwt_fields(&self, v: &mut Violations) {
        // template:begin oidc-jwt:authn-reject-jwt-fields
        v.reject_present(
            &[
                ("authn.token_profile", self.token_profile.is_some()),
                ("authn.algorithms", self.algorithms.is_some()),
                ("authn.jwks_uri", self.jwks_uri.is_some()),
                ("authn.jwks_max_age", self.jwks_max_age.is_some()),
            ],
            "is only valid when authn.mode = oidc-jwt",
        );
        // template:end oidc-jwt:authn-reject-jwt-fields
    }

    #[allow(
        clippy::unused_self,
        unused_variables,
        reason = "no introspection fields exist when the oidc-introspection profile is unselected"
    )]
    fn reject_introspection_fields(&self, v: &mut Violations) {
        // template:begin oidc-introspection:authn-reject-introspection-fields
        v.reject_present(
            &[
                (
                    "authn.introspection_endpoint",
                    self.introspection_endpoint.is_some(),
                ),
                (
                    "authn.introspection_client_id",
                    self.introspection_client_id.is_some(),
                ),
                (
                    "authn.introspection_client_secret",
                    self.introspection_client_secret.is_some(),
                ),
                (
                    "authn.provider_concurrency",
                    self.provider_concurrency.is_some(),
                ),
                ("authn.cache_enabled", self.cache_enabled.is_some()),
                ("authn.cache_capacity", self.cache_capacity.is_some()),
                ("authn.cache_ttl", self.cache_ttl.is_some()),
            ],
            "is only valid when authn.mode = oidc-introspection",
        );
        // template:end oidc-introspection:authn-reject-introspection-fields
    }

    // template:begin oidc-jwt:authn-admit-jwt
    fn admit_jwt(self, v: &mut Violations) -> Result<AuthnConfig, Rejected> {
        self.reject_introspection_fields(v);
        let condition = "authn.mode = oidc-jwt";
        let issuer = v.record(required_non_blank("authn.issuer", self.issuer, condition));
        let audience = admit_audience(self.audience, self.audiences, condition, v);
        let token_profile = v.record(required(
            "authn.token_profile",
            self.token_profile,
            condition,
        ));
        let jwks_max_age = v.record(required_jwks_max_age(self.jwks_max_age, condition));
        let algorithms = self.algorithms.unwrap_or_else(default_algorithms);
        if self
            .jwks_uri
            .as_deref()
            .is_some_and(|uri| uri.trim().is_empty())
        {
            v.reject(ValidationError::new(
                "authn.jwks_uri",
                "must not be blank when set",
            ));
        }
        if algorithms.is_empty() {
            v.reject(ValidationError::new(
                "authn.algorithms",
                "must contain at least one supported algorithm",
            ));
        }
        Ok(AuthnConfig::OidcJwt {
            issuer: issuer?,
            audience: audience?,
            token_profile: token_profile?,
            jwks_max_age: jwks_max_age?,
            algorithms,
            jwks_uri: self.jwks_uri,
        })
    }
    // template:end oidc-jwt:authn-admit-jwt

    // template:begin oidc-introspection:authn-admit-introspection
    fn admit_introspection(self, v: &mut Violations) -> Result<AuthnConfig, Rejected> {
        use shared_rules::oidc_introspection::{CACHE_CAPACITY, CACHE_TTL};

        self.reject_jwt_fields(v);
        let condition = "authn.mode = oidc-introspection";
        let issuer = v.record(required_non_blank("authn.issuer", self.issuer, condition));
        let audience = admit_audience(self.audience, self.audiences, condition, v);
        let introspection_endpoint = v.record(required_non_blank(
            "authn.introspection_endpoint",
            self.introspection_endpoint,
            condition,
        ));
        let introspection_client_id = v.record(required_non_blank(
            "authn.introspection_client_id",
            self.introspection_client_id,
            condition,
        ));
        let secret = v.record(required(
            "authn.introspection_client_secret",
            self.introspection_client_secret,
            condition,
        ));
        let provider_concurrency = self
            .provider_concurrency
            .unwrap_or_else(default_provider_concurrency);
        let cache_enabled = self.cache_enabled.unwrap_or(false);
        let cache_capacity = self.cache_capacity.unwrap_or(DEFAULT_CACHE_CAPACITY);
        let cache_ttl = self.cache_ttl.unwrap_or(DEFAULT_CACHE_TTL);
        let cache_capacity = v.record(
            crate::validate::int_range("authn.cache_capacity", cache_capacity, CACHE_CAPACITY)
                .and_then(|()| {
                    NonZeroUsize::new(cache_capacity).ok_or_else(|| {
                        ValidationError::new("authn.cache_capacity", "must be at least 1")
                    })
                }),
        );
        v.check(crate::validate::duration_range(
            "authn.cache_ttl",
            cache_ttl,
            CACHE_TTL,
        ));
        let cache_capacity = cache_capacity?;
        Ok(AuthnConfig::OidcIntrospection {
            issuer: issuer?,
            audience: audience?,
            introspection_endpoint: introspection_endpoint?,
            introspection_client_id: introspection_client_id?,
            introspection_client_secret: secret?,
            provider_concurrency,
            cache: cache_enabled.then_some(IntrospectionCacheConfig {
                capacity: cache_capacity,
                ttl: cache_ttl,
            }),
        })
    }
    // template:end oidc-introspection:authn-admit-introspection

    pub(crate) fn admit(self, v: &mut Violations) -> Result<AuthnConfig, Rejected> {
        match self.mode {
            // template:begin oidc-jwt:authn-config-jwt-validation
            AuthnMode::OidcJwt => self.admit_jwt(v),
            // template:end oidc-jwt:authn-config-jwt-validation
            // template:begin oidc-introspection:authn-config-introspection-validation
            AuthnMode::OidcIntrospection => self.admit_introspection(v),
            // template:end oidc-introspection:authn-config-introspection-validation
            AuthnMode::None => {
                v.reject_present(
                    &[
                        ("authn.issuer", self.issuer.is_some()),
                        ("authn.audience", self.audience.is_some()),
                        ("authn.audiences", self.audiences.is_some()),
                    ],
                    "is only valid when authn.mode is not none",
                );
                self.reject_jwt_fields(v);
                self.reject_introspection_fields(v);
                Ok(AuthnConfig::None)
            }
        }
    }
}

#[cfg(test)]
mod tests;
