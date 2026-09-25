//! Optional inbound authentication profile and its static trust inputs.
//!
//! This section validates configuration shape only. Provider discovery, DNS,
//! credentials use, and verification belong to the authentication adapter.

// template:begin oidc-introspection:authn-secrecy-import
use secrecy::{ExposeSecret, SecretString};
// template:end oidc-introspection:authn-secrecy-import
use serde::Deserialize;
use url::Url;

use crate::ValidationError;

/// Inbound authentication mechanism selected for this process.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthnMode {
    /// No provider is contacted and protected operations fail closed.
    #[default]
    None,
    // template:begin oidc-jwt:authn-mode-oidc-jwt
    /// Verify signed OIDC access tokens against discovered keys.
    OidcJwt,
    // template:end oidc-jwt:authn-mode-oidc-jwt
    // template:begin oidc-introspection:authn-mode-oidc-introspection
    /// Verify opaque access tokens with the configured RFC 7662 endpoint.
    OidcIntrospection,
    // template:end oidc-introspection:authn-mode-oidc-introspection
}

// template:begin oidc-jwt:authn-token-profile
/// Additional JWT access-token requirements selected by an OIDC JWT profile.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TokenProfile {
    /// Accept the service's resource-server access-token contract.
    ResourceServer,
    /// Require RFC 9068 access-token typing and claims.
    Rfc9068,
}
// template:end oidc-jwt:authn-token-profile

/// Static configuration for the optional inbound authentication profiles.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AuthnConfig {
    /// Selected inbound authentication mechanism.
    pub mode: AuthnMode,
    /// Exact OIDC issuer used for discovery and verified-claim comparison.
    pub issuer: String,
    /// Exact accepted audience.
    pub audience: String,
    // template:begin oidc-jwt:authn-token-profile-field
    /// Optional supplied JWT token profile. Its absence keeps input presence
    /// distinct from the resource-server effective default.
    pub token_profile: Option<TokenProfile>,
    // template:end oidc-jwt:authn-token-profile-field
    // template:begin oidc-introspection:authn-introspection-fields
    /// Fixed RFC 7662 endpoint, used only by the introspection profile.
    pub introspection_endpoint: String,
    /// Client identifier for the fixed introspection credential.
    pub introspection_client_id: String,
    /// Client secret, supplied only through `APP__AUTHN__INTROSPECTION_CLIENT_SECRET`.
    pub introspection_client_secret: SecretString,
    // template:end oidc-introspection:authn-introspection-fields
}

impl Default for AuthnConfig {
    fn default() -> Self {
        Self {
            mode: AuthnMode::None,
            issuer: String::new(),
            audience: String::new(),
            // template:begin oidc-jwt:authn-token-profile-default
            token_profile: None,
            // template:end oidc-jwt:authn-token-profile-default
            // template:begin oidc-introspection:authn-introspection-defaults
            introspection_endpoint: String::new(),
            introspection_client_id: String::new(),
            introspection_client_secret: SecretString::from(String::new()),
            // template:end oidc-introspection:authn-introspection-defaults
        }
    }
}

impl AuthnConfig {
    // template:begin oidc-jwt:authn-token-profile-accessor
    /// The JWT token profile after applying its omitted-value default.
    #[must_use]
    pub fn token_profile(&self) -> TokenProfile {
        self.token_profile.unwrap_or(TokenProfile::ResourceServer)
    }
    // template:end oidc-jwt:authn-token-profile-accessor

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        validate_optional_provider_url("authn.issuer", &self.issuer)?;
        validate_optional_audience(&self.audience)?;

        // template:begin oidc-jwt:authn-foreign-token-profile
        if self.mode != AuthnMode::None
            && self.mode != AuthnMode::OidcJwt
            && self.token_profile.is_some()
        {
            return Err(ValidationError::new(
                "authn.token_profile",
                "must be absent when authn.mode selects another authentication mode",
            ));
        }
        // template:end oidc-jwt:authn-foreign-token-profile
        // template:begin oidc-introspection:authn-foreign-introspection-inputs
        if self.mode != AuthnMode::None && self.mode != AuthnMode::OidcIntrospection {
            reject_foreign_nonblank(
                "authn.introspection_endpoint",
                &self.introspection_endpoint,
                "oidc-jwt",
            )?;
            reject_foreign_nonblank(
                "authn.introspection_client_id",
                &self.introspection_client_id,
                "oidc-jwt",
            )?;
            if has_nonblank_secret(&self.introspection_client_secret) {
                return Err(ValidationError::new(
                    "authn.introspection_client_secret",
                    "must be empty when authn.mode selects another authentication mode",
                ));
            }
        }
        // template:end oidc-introspection:authn-foreign-introspection-inputs
        // template:begin oidc-introspection:authn-dormant-introspection-syntax
        if self.mode == AuthnMode::None {
            validate_optional_provider_url(
                "authn.introspection_endpoint",
                &self.introspection_endpoint,
            )?;
        }
        // template:end oidc-introspection:authn-dormant-introspection-syntax
        match self.mode {
            // template:begin oidc-jwt:authn-jwt-validation
            AuthnMode::OidcJwt => {
                require_nonblank("authn.issuer", &self.issuer, "oidc-jwt")?;
                require_nonblank("authn.audience", &self.audience, "oidc-jwt")?;
            }
            // template:end oidc-jwt:authn-jwt-validation
            // template:begin oidc-introspection:authn-introspection-validation
            AuthnMode::OidcIntrospection => {
                require_nonblank("authn.issuer", &self.issuer, "oidc-introspection")?;
                require_nonblank("authn.audience", &self.audience, "oidc-introspection")?;
                validate_required_provider_url(
                    "authn.introspection_endpoint",
                    &self.introspection_endpoint,
                    "oidc-introspection",
                )?;
                require_nonblank(
                    "authn.introspection_client_id",
                    &self.introspection_client_id,
                    "oidc-introspection",
                )?;
                if !has_nonempty_secret(&self.introspection_client_secret) {
                    return Err(ValidationError::new(
                        "authn.introspection_client_secret",
                        "is required when authn.mode = oidc-introspection",
                    ));
                }
            }
            // template:end oidc-introspection:authn-introspection-validation
            AuthnMode::None => {}
        }
        Ok(())
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

// template:begin oidc-introspection:authn-foreign-input-helper
fn reject_foreign_nonblank(key: &str, value: &str, mode: &str) -> Result<(), ValidationError> {
    if !value.trim().is_empty() {
        return Err(ValidationError::new(
            key,
            format!("must be empty when authn.mode = {mode}"),
        ));
    }
    Ok(())
}
// template:end oidc-introspection:authn-foreign-input-helper

// template:begin oidc-introspection:authn-secret-presence-helper
fn has_nonempty_secret(value: &SecretString) -> bool {
    !value.expose_secret().is_empty()
}

fn has_nonblank_secret(value: &SecretString) -> bool {
    !value.expose_secret().trim().is_empty()
}
// template:end oidc-introspection:authn-secret-presence-helper

fn validate_optional_audience(value: &str) -> Result<(), ValidationError> {
    if !value.trim().is_empty() && value.trim() != value {
        return Err(ValidationError::new(
            "authn.audience",
            "must not have surrounding whitespace",
        ));
    }
    Ok(())
}

// template:begin oidc-introspection:authn-required-provider-url-helper
fn validate_required_provider_url(
    key: &str,
    value: &str,
    mode: &str,
) -> Result<(), ValidationError> {
    require_nonblank(key, value, mode)?;
    validate_provider_url(key, value)
}
// template:end oidc-introspection:authn-required-provider-url-helper

fn validate_optional_provider_url(key: &str, value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Ok(());
    }
    validate_provider_url(key, value)
}

fn validate_provider_url(key: &str, value: &str) -> Result<(), ValidationError> {
    // Keep the URL grammar aligned with infra-bearerauthn's runtime parser.
    // This side names the invalid config key; runtime also admits discovered URLs.
    if value.trim() != value || value.chars().any(|character| character.is_ascii_control()) {
        return Err(invalid_provider_url(key));
    }
    let url = Url::parse(value).map_err(|_| invalid_provider_url(key))?;
    if url.scheme() != "https"
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid_provider_url(key));
    }
    Ok(())
}

fn invalid_provider_url(key: &str) -> ValidationError {
    ValidationError::new(
        key,
        "must be an absolute HTTPS URL without userinfo, query, or fragment",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // template:begin oidc-jwt:authn-jwt-test-config
    fn jwt_config() -> AuthnConfig {
        AuthnConfig {
            mode: AuthnMode::OidcJwt,
            issuer: "https://issuer.example/tenant".to_owned(),
            audience: "service".to_owned(),
            ..AuthnConfig::default()
        }
    }
    // template:end oidc-jwt:authn-jwt-test-config

    // template:begin oidc-introspection:authn-introspection-test-config
    fn introspection_config() -> AuthnConfig {
        let mut config = AuthnConfig {
            mode: AuthnMode::OidcIntrospection,
            issuer: "https://issuer.example/tenant".to_owned(),
            audience: "service".to_owned(),
            introspection_endpoint: "https://issuer.example/introspect".to_owned(),
            introspection_client_id: "service-client".to_owned(),
            ..AuthnConfig::default()
        };
        config.introspection_client_secret = SecretString::from("secret".to_owned());
        config
    }
    // template:end oidc-introspection:authn-introspection-test-config

    // template:begin oidc-jwt:authn-jwt-default-tests
    #[test]
    fn defaults_are_inert_and_keep_token_profile_absent() {
        let config = AuthnConfig::default();
        assert_eq!(config.mode, AuthnMode::None);
        assert_eq!(config.token_profile, None);
        assert_eq!(config.token_profile(), TokenProfile::ResourceServer);
        config.validate().unwrap();
    }
    // template:end oidc-jwt:authn-jwt-default-tests

    // template:begin oidc-jwt:authn-jwt-tests
    #[test]
    fn jwt_requires_shared_trust_and_uses_the_effective_default() {
        let config = jwt_config();
        config.validate().unwrap();
        assert_eq!(config.token_profile(), TokenProfile::ResourceServer);

        for (key, config) in [
            (
                "authn.issuer",
                AuthnConfig {
                    issuer: " ".to_owned(),
                    ..jwt_config()
                },
            ),
            (
                "authn.audience",
                AuthnConfig {
                    audience: String::new(),
                    ..jwt_config()
                },
            ),
        ] {
            assert_eq!(config.validate().unwrap_err().key, key);
        }
    }

    // template:end oidc-jwt:authn-jwt-tests

    // template:begin oidc-introspection:authn-introspection-tests
    #[test]
    fn introspection_requires_its_complete_tuple() {
        let config = introspection_config();
        config.validate().unwrap();

        for (key, config) in [
            (
                "authn.introspection_endpoint",
                AuthnConfig {
                    introspection_endpoint: String::new(),
                    ..introspection_config()
                },
            ),
            (
                "authn.introspection_client_id",
                AuthnConfig {
                    introspection_client_id: " ".to_owned(),
                    ..introspection_config()
                },
            ),
            (
                "authn.introspection_client_secret",
                AuthnConfig {
                    introspection_client_secret: SecretString::from(String::new()),
                    ..introspection_config()
                },
            ),
        ] {
            assert_eq!(config.validate().unwrap_err().key, key);
        }
    }

    #[test]
    fn introspection_secret_is_redacted() {
        let confidential = "fixture-private-value-not-a-field-name";
        let config = AuthnConfig {
            introspection_client_secret: SecretString::from(confidential),
            ..introspection_config()
        };
        let debug = format!("{config:?}");
        assert!(debug.contains("introspection_client_secret"));
        assert!(!debug.contains(confidential));
    }
    // template:end oidc-introspection:authn-introspection-tests

    // template:begin oidc-introspection:authn-dormant-introspection-tests
    #[test]
    fn dormant_nonblank_provider_inputs_still_require_valid_syntax() {
        let accepted = AuthnConfig {
            issuer: "https://issuer.example/tenant".to_owned(),
            audience: "service".to_owned(),
            introspection_endpoint: "https://issuer.example/introspect".to_owned(),
            ..AuthnConfig::default()
        };
        accepted.validate().unwrap();
        AuthnConfig {
            introspection_endpoint: "https://@issuer.example".to_owned(),
            ..AuthnConfig::default()
        }
        .validate()
        .expect("the URL parser canonicalizes empty userinfo");

        for (key, value) in [
            ("authn.issuer", "http://issuer.example"),
            ("authn.issuer", " https://issuer.example"),
            ("authn.issuer", "https://issuer.example/\u{0001}"),
            ("authn.introspection_endpoint", "https://u:p@issuer.example"),
            (
                "authn.introspection_endpoint",
                "https://issuer.example/?x=1",
            ),
            (
                "authn.introspection_endpoint",
                "https://issuer.example/#fragment",
            ),
        ] {
            let config = if key == "authn.issuer" {
                AuthnConfig {
                    issuer: value.to_owned(),
                    ..AuthnConfig::default()
                }
            } else {
                AuthnConfig {
                    introspection_endpoint: value.to_owned(),
                    ..AuthnConfig::default()
                }
            };
            assert_eq!(config.validate().unwrap_err().key, key);
        }
    }
    // template:end oidc-introspection:authn-dormant-introspection-tests

    #[test]
    fn audience_preserves_exact_input_and_rejects_surrounding_whitespace() {
        let exact = AuthnConfig {
            audience: "service".to_owned(),
            ..AuthnConfig::default()
        };
        exact.validate().unwrap();
        assert_eq!(exact.audience, "service");

        let padded = AuthnConfig {
            audience: " service ".to_owned(),
            ..AuthnConfig::default()
        };
        assert_eq!(padded.validate().unwrap_err().key, "authn.audience");
    }
}
