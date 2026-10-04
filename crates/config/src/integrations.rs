//! Immutable named outbound integration configuration.
//!
//! This section validates static operator input only. It neither constructs a
//! credential owner nor performs token or resource I/O.

use std::collections::BTreeMap;
use std::fmt;

use secrecy::SecretString;
// template:begin outbound-auth:config-integration-oauth-imports
use secrecy::ExposeSecret as _;
// template:end outbound-auth:config-integration-oauth-imports
use serde::Deserialize;
// template:begin outbound-auth:config-integration-oauth-url-import
use url::Url;
// template:end outbound-auth:config-integration-oauth-url-import

use crate::ValidationError;
// template:begin grpc:config-integration-grpc-import
use crate::GrpcSecurity;
// template:end grpc:config-integration-grpc-import

/// One named integration's optional client input.
///
/// The loader keeps this section's values out of a decode failure whichever
/// source set them, as `Debug` does below.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IntegrationConfig {
    // template:begin outbound-auth:config-integration-oauth-field
    /// OAuth client-credentials input. Absent entries do not create clients.
    pub oauth: Option<OAuthConfig>,
    // template:end outbound-auth:config-integration-oauth-field
    // template:begin grpc:config-integration-grpc-field
    /// Native gRPC client input. Absent entries do not create a channel.
    pub grpc: Option<GrpcClientConfig>,
    // template:end grpc:config-integration-grpc-field
}

// template:begin grpc:config-integration-grpc-type
/// Immutable native gRPC client input for one named integration.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrpcClientConfig {
    /// Tonic endpoint input, admitted by the transport before a channel exists.
    #[serde(default)]
    pub destination: String,
    /// Explicit client security mode; required.
    pub security: GrpcSecurity,
    /// Optional PEM trust anchor for TLS. Blank is absent.
    #[serde(default, deserialize_with = "crate::de::blank_as_none")]
    pub ca_certificate: Option<String>,
    /// Optional PEM client certificate for mTLS. Blank is absent.
    #[serde(default, deserialize_with = "crate::de::blank_as_none")]
    pub certificate: Option<String>,
    /// PEM client key for mTLS, never in a config file. Blank is absent.
    #[serde(default, deserialize_with = "crate::de::blank_secret_as_none")]
    pub private_key: Option<SecretString>,
}

impl fmt::Debug for GrpcClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrpcClientConfig")
            .field("destination", &self.destination)
            .field("security", &self.security)
            .finish_non_exhaustive()
    }
}
// template:end grpc:config-integration-grpc-type

// template:begin outbound-auth:config-integration-oauth-types
/// One immutable OAuth client-credentials tuple.
///
/// A missing text key decodes empty and is refused by validation, which
/// names the key; `algorithm` has no empty form and is refused when decoded.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthConfig {
    /// Fixed token endpoint, admitted before any adapter construction.
    #[serde(default)]
    pub token_url: String,
    /// Client identifier posted as a form field and asserted as the client
    /// assertion's `iss` and `sub`.
    #[serde(default)]
    pub client_id: String,
    /// PEM private key used to sign the client assertion, never in a config
    /// file.
    #[serde(default = "empty_secret")]
    pub private_key: SecretString,
    /// Key identifier carried in the client-assertion header as `kid`.
    #[serde(default)]
    pub key_id: String,
    /// Client-assertion signing algorithm; required, because it must match
    /// the key.
    pub algorithm: OAuthAlgorithm,
    /// Audience claim asserted in the signed client assertion.
    #[serde(default)]
    pub assertion_audience: String,
    /// Optional RFC 6749 scopes, retained in their configured order.
    #[serde(default)]
    pub scopes: Scopes,
    /// Optional OAuth audience parameter.
    pub audience: Option<String>,
    /// How many subjects keep a token exchanged on their behalf.
    #[serde(default = "default_exchange_cache_capacity")]
    pub exchange_cache_capacity: u32,
    /// Maximum simultaneous token acquisitions across both grant types.
    #[serde(
        default = "default_provider_concurrency",
        deserialize_with = "deserialize_provider_concurrency"
    )]
    pub provider_concurrency: u32,
}

impl OAuthConfig {
    /// One retained token per user active within a token lifetime on one
    /// replica.
    pub const DEFAULT_EXCHANGE_CACHE_CAPACITY: u32 = 1024;
    const EXCHANGE_CACHE_CAPACITY: std::ops::RangeInclusive<u32> = 1..=65_536;
    /// Matches introspection's default while bounding distinct token attempts.
    pub const DEFAULT_PROVIDER_CONCURRENCY: u32 = 32;
}

fn empty_secret() -> SecretString {
    SecretString::from(String::new())
}

const fn default_exchange_cache_capacity() -> u32 {
    OAuthConfig::DEFAULT_EXCHANGE_CACHE_CAPACITY
}

const fn default_provider_concurrency() -> u32 {
    OAuthConfig::DEFAULT_PROVIDER_CONCURRENCY
}

// Buffer the scalar kind before decoding: config-rs otherwise coerces floats
// and booleans to integers. Environment values arrive as strings.
fn deserialize_provider_concurrency<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Input {
        Typed(u32),
        Text(String),
    }

    let invalid = || {
        serde::de::Error::custom(format_args!(
            "{}must be an integer from 0 to 4294967295",
            crate::de::VALUE_FREE
        ))
    };
    match Input::deserialize(deserializer).map_err(|_| invalid())? {
        Input::Typed(value) => Ok(value),
        Input::Text(value) => value.parse().map_err(|_| invalid()),
    }
}

impl fmt::Debug for OAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthConfig").finish_non_exhaustive()
    }
}

/// Client-assertion signing algorithm, decoded from its RFC 7518 identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthAlgorithm {
    /// RSASSA-PKCS1-v1_5 using SHA-256.
    Rs256,
    /// RSASSA-PSS using SHA-256.
    Ps256,
    /// ECDSA using the P-256 curve and SHA-256.
    Es256,
}

/// Decoded by hand so a refused value is answered with the accepted ones:
/// config-rs reports a derived enum's mismatch without naming them, and this
/// key has no default to fall back on.
impl<'de> Deserialize<'de> for OAuthAlgorithm {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match String::deserialize(deserializer)?.as_str() {
            "RS256" => Ok(Self::Rs256),
            "PS256" => Ok(Self::Ps256),
            "ES256" => Ok(Self::Es256),
            _ => Err(serde::de::Error::custom(format_args!(
                "{}must be RS256, PS256 or ES256",
                crate::de::VALUE_FREE
            ))),
        }
    }
}

/// RFC 6749 scope tokens supplied as a list or an environment string.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Scopes(Vec<String>);

impl Scopes {
    /// Borrow the configured scope tokens in their wire order.
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    /// Consume the configured scope tokens in their wire order.
    #[must_use]
    pub fn into_inner(self) -> Vec<String> {
        self.0
    }
}

impl fmt::Debug for Scopes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Scopes([REDACTED])")
    }
}

impl<'de> Deserialize<'de> for Scopes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            Text(String),
            List(Vec<String>),
        }

        let input = Input::deserialize(deserializer).map_err(|_| {
            serde::de::Error::custom(format_args!(
                "{}must be a list of strings or a space-separated string",
                crate::de::VALUE_FREE
            ))
        })?;
        let scopes = match input {
            Input::Text(value) => value
                .split(' ')
                .filter(|scope| !scope.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
            Input::List(scopes) => scopes,
        };
        Ok(Self(scopes))
    }
}

// template:end outbound-auth:config-integration-oauth-types

pub(crate) fn validate_integrations(
    integrations: &BTreeMap<String, IntegrationConfig>,
) -> Result<(), ValidationError> {
    for (name, integration) in integrations {
        // template:begin outbound-auth:config-integration-oauth-validate
        if let Some(oauth) = &integration.oauth {
            oauth.validate(&format!("integrations.{name}.oauth"))?;
        }
        // template:end outbound-auth:config-integration-oauth-validate
        // template:begin grpc:config-integration-grpc-validate
        if let Some(grpc) = &integration.grpc {
            grpc.validate(&format!("integrations.{name}.grpc"))?;
        }
        // template:end grpc:config-integration-grpc-validate
    }
    Ok(())
}

// template:begin grpc:config-integration-grpc-validation
impl GrpcClientConfig {
    fn validate(&self, prefix: &str) -> Result<(), ValidationError> {
        if self.destination.trim().is_empty() {
            return Err(ValidationError::new(
                &format!("{prefix}.destination"),
                "cannot be empty",
            ));
        }
        if self
            .destination
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(ValidationError::new(
                &format!("{prefix}.destination"),
                "must not contain whitespace or controls",
            ));
        }
        match self.security {
            GrpcSecurity::Plaintext => {
                if self.ca_certificate.is_some() {
                    return Err(ValidationError::new(
                        &format!("{prefix}.ca_certificate"),
                        "is only valid when security is tls",
                    ));
                }
                if self.certificate.is_some() {
                    return Err(ValidationError::new(
                        &format!("{prefix}.certificate"),
                        "is only valid when security is tls",
                    ));
                }
                if self.private_key.is_some() {
                    return Err(ValidationError::new(
                        &format!("{prefix}.private_key"),
                        "is only valid when security is tls",
                    ));
                }
            }
            GrpcSecurity::Tls => {
                if self.certificate.is_some() != self.private_key.is_some() {
                    return Err(ValidationError::new(
                        &format!("{prefix}.certificate/{prefix}.private_key"),
                        "must be configured together for mTLS",
                    ));
                }
            }
        }
        Ok(())
    }
}
// template:end grpc:config-integration-grpc-validation

// template:begin outbound-auth:config-integration-oauth-validation
impl OAuthConfig {
    fn validate(&self, prefix: &str) -> Result<(), ValidationError> {
        let token_url_key = format!("{prefix}.token_url");
        validate_token_url(&token_url_key, &self.token_url)?;

        if self.client_id.trim().is_empty() {
            return Err(ValidationError::new(
                &format!("{prefix}.client_id"),
                "cannot be empty",
            ));
        }
        if self.private_key.expose_secret().trim().is_empty() {
            return Err(ValidationError::new(
                &format!("{prefix}.private_key"),
                "cannot be empty",
            ));
        }
        if self.key_id.trim().is_empty() {
            return Err(ValidationError::new(
                &format!("{prefix}.key_id"),
                "cannot be empty",
            ));
        }
        if self.assertion_audience.trim().is_empty() {
            return Err(ValidationError::new(
                &format!("{prefix}.assertion_audience"),
                "cannot be empty",
            ));
        }
        if self.audience.as_deref().is_some_and(str::is_empty) {
            return Err(ValidationError::new(
                &format!("{prefix}.audience"),
                "cannot be empty when configured",
            ));
        }
        if self.scopes.0.iter().any(|scope| !is_scope_token(scope)) {
            return Err(ValidationError::new(
                &format!("{prefix}.scopes"),
                "must contain RFC 6749 scope tokens",
            ));
        }
        if !Self::EXCHANGE_CACHE_CAPACITY.contains(&self.exchange_cache_capacity) {
            return Err(ValidationError::new(
                &format!("{prefix}.exchange_cache_capacity"),
                "must be from 1 to 65536",
            ));
        }
        if self.provider_concurrency == 0 {
            return Err(ValidationError::new(
                &format!("{prefix}.provider_concurrency"),
                "must be greater than zero",
            ));
        }
        Ok(())
    }
}

fn validate_token_url(key: &str, token_url: &str) -> Result<(), ValidationError> {
    if token_url
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(ValidationError::new(
            key,
            "must not contain whitespace or controls",
        ));
    }

    let url = Url::parse(token_url)
        .map_err(|_| ValidationError::new(key, "must be a valid HTTPS URL"))?;
    if url.scheme() != "https" {
        return Err(ValidationError::new(key, "must use HTTPS"));
    }
    if url.host().is_none() {
        return Err(ValidationError::new(key, "must have a host"));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || raw_authority_has_userinfo(token_url)
    {
        return Err(ValidationError::new(key, "must not include userinfo"));
    }
    if url.fragment().is_some() {
        return Err(ValidationError::new(key, "must not include a fragment"));
    }
    Ok(())
}

fn raw_authority_has_userinfo(token_url: &str) -> bool {
    let Some(authority) = token_url.split_once("://").map(|(_, authority)| authority) else {
        return false;
    };
    let authority_end = authority.find(['/', '?', '#']).unwrap_or(authority.len());
    authority[..authority_end].contains('@')
}

fn is_scope_token(scope: &str) -> bool {
    !scope.is_empty()
        && scope
            .bytes()
            .all(|byte| matches!(byte, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;

    use super::*;

    #[test]
    fn whitespace_only_private_key_is_refused() {
        let config = OAuthConfig {
            token_url: "https://identity.example/token".to_owned(),
            client_id: "billing-service".to_owned(),
            private_key: SecretString::from("   "),
            key_id: "key-1".to_owned(),
            algorithm: OAuthAlgorithm::Es256,
            assertion_audience: "https://identity.example".to_owned(),
            scopes: Scopes::default(),
            audience: None,
            exchange_cache_capacity: OAuthConfig::DEFAULT_EXCHANGE_CACHE_CAPACITY,
            provider_concurrency: OAuthConfig::DEFAULT_PROVIDER_CONCURRENCY,
        };
        let err = config.validate("integrations.billing.oauth").unwrap_err();
        assert_eq!(err.key, "integrations.billing.oauth.private_key");
        assert_eq!(err.message, "cannot be empty");
    }
}
// template:end outbound-auth:config-integration-oauth-validation
