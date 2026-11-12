//! Immutable named outbound integration configuration.
//!
//! This section validates static operator input only. It neither constructs a
//! credential owner nor performs token or resource I/O.

use std::collections::BTreeMap;

use secrecy::SecretString;
use serde::Deserialize;
// template:begin outbound-auth:config-integration-oauth-url-import
use url::Url;
// template:end outbound-auth:config-integration-oauth-url-import

use crate::ValidationError;
// template:begin grpc:config-integration-grpc-import
use crate::GrpcSecurity;
use crate::validate::missing;
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
    /// `http` or `https` URI, admitted by the transport before a channel exists.
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

impl std::fmt::Debug for GrpcClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
#[derive(Clone, Deserialize, derive_more::Debug)]
#[serde(deny_unknown_fields)]
#[debug("OAuthConfig {{ .. }}")]
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
    #[serde(default, deserialize_with = "crate::de::blank_secret_as_none")]
    pub private_key: Option<SecretString>,
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
    #[serde(
        default = "default_exchange_cache_capacity",
        deserialize_with = "crate::de::unsigned_count"
    )]
    pub exchange_cache_capacity: u32,
    /// Maximum simultaneous token acquisitions across both grant types.
    #[serde(
        default = "default_provider_concurrency",
        deserialize_with = "deserialize_provider_concurrency"
    )]
    pub provider_concurrency: std::num::NonZeroU32,
}

impl OAuthConfig {
    /// One retained token per user active within a token lifetime on one
    /// replica.
    const DEFAULT_EXCHANGE_CACHE_CAPACITY: u32 = 1024;
    // Keep in sync with direct Options admission in
    // crates/infra-oauth2-client-credentials/src/options.rs.
    const EXCHANGE_CACHE_CAPACITY_RANGE: std::ops::RangeInclusive<u32> = 1..=65_536;
    /// Matches introspection's default while bounding distinct token attempts.
    const DEFAULT_PROVIDER_CONCURRENCY: std::num::NonZeroU32 =
        const { std::num::NonZeroU32::new(32).expect("32 is nonzero") };
}

const fn default_exchange_cache_capacity() -> u32 {
    OAuthConfig::DEFAULT_EXCHANGE_CACHE_CAPACITY
}

const fn default_provider_concurrency() -> std::num::NonZeroU32 {
    OAuthConfig::DEFAULT_PROVIDER_CONCURRENCY
}

fn deserialize_provider_concurrency<'de, D>(
    deserializer: D,
) -> Result<std::num::NonZeroU32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    crate::de::deserialize_scalar(deserializer, "must be an integer from 1 to 4294967295")
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
            _ => Err(crate::de::value_free("must be RS256, PS256 or ES256")),
        }
    }
}

/// RFC 6749 scope tokens supplied as a list or an environment string.
#[derive(Clone, Default, PartialEq, Eq, derive_more::Debug)]
#[debug("Scopes([REDACTED])")]
pub struct Scopes(Vec<String>);

impl Scopes {
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
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
            crate::de::value_free("must be a list of strings or a space-separated string")
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
        crate::validate::non_blank(&format!("{prefix}.destination"), &self.destination)?;
        if shared_rules::uri::has_whitespace_or_control(&self.destination) {
            return Err(ValidationError::new(
                format!("{prefix}.destination"),
                "must not contain whitespace or controls",
            ));
        }
        match self.security {
            GrpcSecurity::Plaintext => {
                for (name, present) in [
                    ("ca_certificate", self.ca_certificate.is_some()),
                    ("certificate", self.certificate.is_some()),
                    ("private_key", self.private_key.is_some()),
                ] {
                    if present {
                        return Err(ValidationError::new(
                            format!("{prefix}.{name}"),
                            "is only valid when security is tls",
                        ));
                    }
                }
            }
            GrpcSecurity::Tls => match (&self.certificate, &self.private_key) {
                (Some(_), None) => {
                    return Err(missing(
                        &format!("{prefix}.private_key"),
                        &format!("{prefix}.certificate is set"),
                    ));
                }
                (None, Some(_)) => {
                    return Err(missing(
                        &format!("{prefix}.certificate"),
                        &format!("{prefix}.private_key is set"),
                    ));
                }
                (Some(_), Some(_)) | (None, None) => {}
            },
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

        crate::validate::non_blank(&format!("{prefix}.client_id"), &self.client_id)?;
        crate::validate::required(
            &format!("{prefix}.private_key"),
            self.private_key.as_ref(),
            &format!("{prefix} is set"),
        )?;
        crate::validate::non_blank(&format!("{prefix}.key_id"), &self.key_id)?;
        crate::validate::non_blank(
            &format!("{prefix}.assertion_audience"),
            &self.assertion_audience,
        )?;
        if self
            .audience
            .as_deref()
            .is_some_and(|audience| audience.trim().is_empty())
        {
            return Err(ValidationError::new(
                format!("{prefix}.audience"),
                "cannot be empty when configured",
            ));
        }
        if self
            .scopes
            .0
            .iter()
            .any(|scope| !shared_rules::rfc6749::is_scope_token(scope))
        {
            return Err(ValidationError::new(
                format!("{prefix}.scopes"),
                "must contain RFC 6749 scope tokens",
            ));
        }
        let capacity = Self::EXCHANGE_CACHE_CAPACITY_RANGE;
        crate::validate::int_range(
            &format!("{prefix}.exchange_cache_capacity"),
            u64::from(self.exchange_cache_capacity),
            u64::from(*capacity.start()),
            u64::from(*capacity.end()),
        )?;
        Ok(())
    }
}

fn validate_token_url(key: &str, token_url: &str) -> Result<(), ValidationError> {
    if shared_rules::uri::has_whitespace_or_control(token_url) {
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
        || shared_rules::uri::authority_has_userinfo(token_url)
    {
        return Err(ValidationError::new(key, "must not include userinfo"));
    }
    if url.fragment().is_some() {
        return Err(ValidationError::new(key, "must not include a fragment"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
// template:end outbound-auth:config-integration-oauth-validation

// template:begin grpc:config-integration-grpc-tests
#[cfg(test)]
mod grpc_tests;
// template:end grpc:config-integration-grpc-tests
