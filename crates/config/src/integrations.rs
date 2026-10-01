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
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
// template:begin outbound-auth:config-integration-oauth-url-import
use url::Url;
// template:end outbound-auth:config-integration-oauth-url-import

use crate::ValidationError;
use crate::de::VALUE_FREE;
// template:begin grpc:config-integration-grpc-import
use crate::GrpcSecurity;
// template:end grpc:config-integration-grpc-import

/// One named integration's optional client input.
#[derive(Clone, Debug, Default)]
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
#[derive(Clone)]
pub struct GrpcClientConfig {
    /// Tonic endpoint input, admitted by the transport before a channel exists.
    pub destination: String,
    /// Explicit client security mode.
    pub security: GrpcSecurity,
    /// Optional PEM trust anchor for TLS.
    pub ca_certificate: Option<String>,
    /// Optional PEM client certificate for mTLS.
    pub certificate: Option<String>,
    /// Environment-only PEM client key for mTLS.
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
#[derive(Clone)]
pub struct OAuthConfig {
    /// Fixed token endpoint, admitted before any adapter construction.
    pub token_url: String,
    /// Client identifier posted as a form field and asserted as the client
    /// assertion's `iss` and `sub`.
    pub client_id: String,
    /// Environment-only PEM private key used to sign the client assertion.
    pub private_key: SecretString,
    /// Key identifier carried in the client-assertion header as `kid`.
    pub key_id: String,
    /// Client-assertion signing algorithm; required, because it must match
    /// the key.
    pub algorithm: OAuthAlgorithm,
    /// Audience claim asserted in the signed client assertion.
    pub assertion_audience: String,
    /// Optional RFC 6749 scopes, retained in their configured order.
    pub scopes: Scopes,
    /// Optional OAuth audience parameter.
    pub audience: Option<String>,
    /// How many subjects keep a token exchanged on their behalf.
    pub exchange_cache_capacity: u32,
}

impl OAuthConfig {
    /// One retained token per user active within a token lifetime on one
    /// replica.
    pub const DEFAULT_EXCHANGE_CACHE_CAPACITY: u32 = 1024;
    const EXCHANGE_CACHE_CAPACITY: std::ops::RangeInclusive<u32> = 1..=65_536;
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
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            Text(String),
            List(Vec<String>),
        }

        let input = Input::deserialize(deserializer).map_err(|_| {
            D::Error::custom("must be a list of strings or a space-separated string")
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

/// Decode through config-rs's value representation so its rejected-value
/// diagnostics never escape this sensitive section. Every message written
/// here names a key and no value, from a file or the environment alike.
pub(crate) fn deserialize_integrations<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, IntegrationConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = config::Value::deserialize(deserializer)
        .map_err(|_| value_free("integrations: invalid configuration value"))?;
    decode_integrations(value).map_err(value_free)
}

fn value_free<E: serde::de::Error>(message: impl fmt::Display) -> E {
    E::custom(format_args!("{VALUE_FREE}{message}"))
}

fn decode_integrations(
    value: config::Value,
) -> Result<BTreeMap<String, IntegrationConfig>, String> {
    let mut integrations = BTreeMap::new();
    for (name, value) in into_table(value, "integrations")? {
        let prefix = format!("integrations.{name}");
        let mut fields = into_table(value, &prefix)?;
        // template:begin outbound-auth:config-integration-oauth-decode
        let oauth = fields
            .remove("oauth")
            .map(|value| decode_oauth(value, &format!("{prefix}.oauth")))
            .transpose()?;
        // template:end outbound-auth:config-integration-oauth-decode
        // template:begin grpc:config-integration-grpc-decode
        let grpc = fields
            .remove("grpc")
            .map(|value| decode_grpc(value, &format!("{prefix}.grpc")))
            .transpose()?;
        // template:end grpc:config-integration-grpc-decode
        refuse_unknown(fields, &prefix)?;
        integrations.insert(
            name,
            IntegrationConfig {
                // template:begin outbound-auth:config-integration-oauth-value
                oauth,
                // template:end outbound-auth:config-integration-oauth-value
                // template:begin grpc:config-integration-grpc-value
                grpc,
                // template:end grpc:config-integration-grpc-value
            },
        );
    }
    Ok(integrations)
}

// template:begin outbound-auth:config-integration-oauth-parser
fn decode_oauth(value: config::Value, prefix: &str) -> Result<OAuthConfig, String> {
    let mut fields = into_table(value, prefix)?;
    let token_url = take_text(&mut fields, "token_url", prefix)?.unwrap_or_default();
    let client_id = take_text(&mut fields, "client_id", prefix)?.unwrap_or_default();
    let private_key =
        SecretString::from(take_text(&mut fields, "private_key", prefix)?.unwrap_or_default());
    let key_id = take_text(&mut fields, "key_id", prefix)?.unwrap_or_default();
    let algorithm = take_text(&mut fields, "algorithm", prefix)?
        .map(|value| parse_oauth_algorithm(&value, prefix))
        .transpose()?;
    let assertion_audience =
        take_text(&mut fields, "assertion_audience", prefix)?.unwrap_or_default();
    let audience = take_text(&mut fields, "audience", prefix)?;
    let scopes = fields
        .remove("scopes")
        .map(|value| {
            value.try_deserialize::<Scopes>().map_err(|_| {
                format!("{prefix}.scopes: must be a list of strings or a space-separated string")
            })
        })
        .transpose()?
        .unwrap_or_default();
    let exchange_cache_capacity = take_count(&mut fields, "exchange_cache_capacity", prefix)?
        .unwrap_or(OAuthConfig::DEFAULT_EXCHANGE_CACHE_CAPACITY);
    refuse_unknown(fields, prefix)?;
    // After the unknown-key check, so a misspelt or unsupported key is named
    // first.
    let algorithm = algorithm.ok_or_else(|| format!("{prefix}.algorithm: is required"))?;
    Ok(OAuthConfig {
        token_url,
        client_id,
        private_key,
        key_id,
        algorithm,
        assertion_audience,
        scopes,
        audience,
        exchange_cache_capacity,
    })
}

/// A whole number from a file, or its decimal text from the environment.
fn take_count(
    fields: &mut config::Map<String, config::Value>,
    field: &str,
    prefix: &str,
) -> Result<Option<u32>, String> {
    fields
        .remove(field)
        .map(|value| {
            match value.kind {
                config::ValueKind::I64(count) => u32::try_from(count).ok(),
                config::ValueKind::U64(count) => u32::try_from(count).ok(),
                config::ValueKind::String(count) => count.parse().ok(),
                _ => None,
            }
            .ok_or_else(|| format!("{prefix}.{field}: must be a whole number"))
        })
        .transpose()
}

fn parse_oauth_algorithm(value: &str, prefix: &str) -> Result<OAuthAlgorithm, String> {
    match value {
        "RS256" => Ok(OAuthAlgorithm::Rs256),
        "PS256" => Ok(OAuthAlgorithm::Ps256),
        "ES256" => Ok(OAuthAlgorithm::Es256),
        _ => Err(format!("{prefix}.algorithm: must be RS256, PS256 or ES256")),
    }
}
// template:end outbound-auth:config-integration-oauth-parser

// template:begin grpc:config-integration-grpc-parser
fn decode_grpc(value: config::Value, prefix: &str) -> Result<GrpcClientConfig, String> {
    let mut fields = into_table(value, prefix)?;
    let destination = take_text(&mut fields, "destination", prefix)?.unwrap_or_default();
    let security = take_text(&mut fields, "security", prefix)?
        .ok_or_else(|| format!("{prefix}.security: is required"))
        .and_then(|value| parse_grpc_security(&value, prefix))?;
    let ca_certificate = take_nonblank_text(&mut fields, "ca_certificate", prefix)?;
    let certificate = take_nonblank_text(&mut fields, "certificate", prefix)?;
    let private_key = take_nonblank_secret(&mut fields, "private_key", prefix)?;
    refuse_unknown(fields, prefix)?;
    Ok(GrpcClientConfig {
        destination,
        security,
        ca_certificate,
        certificate,
        private_key,
    })
}

fn parse_grpc_security(value: &str, prefix: &str) -> Result<GrpcSecurity, String> {
    match value {
        "plaintext" => Ok(GrpcSecurity::Plaintext),
        "tls" => Ok(GrpcSecurity::Tls),
        _ => Err(format!("{prefix}.security: must be plaintext or tls")),
    }
}
// template:end grpc:config-integration-grpc-parser

fn into_table(
    value: config::Value,
    key: &str,
) -> Result<config::Map<String, config::Value>, String> {
    value
        .into_table()
        .map_err(|_| format!("{key}: must be an object"))
}

fn take_text(
    fields: &mut config::Map<String, config::Value>,
    field: &str,
    prefix: &str,
) -> Result<Option<String>, String> {
    fields
        .remove(field)
        .map(|value| match value.kind {
            config::ValueKind::String(value) => Ok(value),
            _ => Err(format!("{prefix}.{field}: must be a string")),
        })
        .transpose()
}

// template:begin grpc:config-integration-grpc-material-parser
fn take_nonblank_text(
    fields: &mut config::Map<String, config::Value>,
    field: &str,
    prefix: &str,
) -> Result<Option<String>, String> {
    Ok(take_text(fields, field, prefix)?.filter(|value| !value.trim().is_empty()))
}

fn take_nonblank_secret(
    fields: &mut config::Map<String, config::Value>,
    field: &str,
    prefix: &str,
) -> Result<Option<SecretString>, String> {
    Ok(take_nonblank_text(fields, field, prefix)?.map(SecretString::from))
}
// template:end grpc:config-integration-grpc-material-parser

fn refuse_unknown(fields: config::Map<String, config::Value>, prefix: &str) -> Result<(), String> {
    if let Some(field) = fields.into_keys().next() {
        return Err(format!("{prefix}.{field}: unknown key"));
    }
    Ok(())
}

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
        };
        let err = config.validate("integrations.billing.oauth").unwrap_err();
        assert_eq!(err.key, "integrations.billing.oauth.private_key");
        assert_eq!(err.message, "cannot be empty");
    }
}
// template:end outbound-auth:config-integration-oauth-validation
