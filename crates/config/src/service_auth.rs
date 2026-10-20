//! Service-to-service authentication with caller-signed tokens.
//!
//! This section validates configuration shape only. Caller-name, token-type,
//! trust-registry and key admission belong to the service authentication
//! adapter, which bootstrap runs before any listener binds.

use secrecy::SecretString;
use serde::Deserialize;

use crate::ValidationError;
use crate::app::is_local_development;
use crate::de::{blank_as_none, blank_secret_as_none};

/// How gRPC calls between services are authenticated.
#[derive(Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ServiceAuthConfig {
    /// Calls carry and need no service token. With gRPC enabled this is
    /// accepted only in a local or development environment. This is the
    /// omitted-section default.
    None {},
    /// Every gRPC business call carries a caller-signed ES256 token.
    Jwt {
        /// This service's name: the audience it accepts and its tokens'
        /// `iss` and `sub`.
        #[serde(default)]
        name: String,
        /// The `typ` of this trust domain's service tokens.
        #[serde(default)]
        jwt_type: String,
        /// The trust registry: a JSON object of each caller's JWK Set.
        #[serde(default)]
        trust: String,
        /// The `kid` receivers hold this service's public key under.
        /// Required together with `private_key`.
        #[serde(default, deserialize_with = "blank_as_none")]
        key_id: Option<String>,
        /// Environment-only PKCS#8 PEM of this service's EC P-256 signing
        /// key. Without it the service verifies callers but calls no one.
        #[serde(default, deserialize_with = "blank_secret_as_none")]
        private_key: Option<SecretString>,
    },
}

impl std::fmt::Debug for ServiceAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode = match self {
            Self::None {} => "none",
            Self::Jwt { .. } => "jwt",
        };
        f.debug_struct("ServiceAuthConfig")
            .field("mode", &mode)
            .finish_non_exhaustive()
    }
}

impl Default for ServiceAuthConfig {
    fn default() -> Self {
        Self::None {}
    }
}

impl ServiceAuthConfig {
    /// The mode's spelling, for the startup record.
    #[must_use]
    pub const fn mode(&self) -> &'static str {
        match self {
            Self::None {} => "none",
            Self::Jwt { .. } => "jwt",
        }
    }

    pub(crate) fn validate(
        &self,
        app_env: &str,
        grpc_enabled: bool,
    ) -> Result<(), ValidationError> {
        match self {
            Self::None {} if grpc_enabled && !is_local_development(app_env) => {
                Err(ValidationError::new(
                    "service_auth.mode",
                    "must be jwt when grpc.enabled is true outside app.env local or development",
                ))
            }
            Self::None {} => Ok(()),
            Self::Jwt {
                name,
                jwt_type,
                trust,
                key_id,
                private_key,
            } => {
                for (key, value) in [
                    ("service_auth.name", name),
                    ("service_auth.jwt_type", jwt_type),
                    ("service_auth.trust", trust),
                ] {
                    if value.trim().is_empty() {
                        return Err(ValidationError::new(
                            key,
                            "is required when service_auth.mode = jwt",
                        ));
                    }
                }
                match (key_id, private_key) {
                    (Some(_), None) => Err(ValidationError::new(
                        "service_auth.private_key",
                        "is required when service_auth.key_id is set",
                    )),
                    (None, Some(_)) => Err(ValidationError::new(
                        "service_auth.key_id",
                        "is required when service_auth.private_key is set",
                    )),
                    _ => Ok(()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A jwt-mode section with the required keys, then `extra` lines.
    fn jwt(extra: &str) -> ServiceAuthConfig {
        toml::from_str(&format!(
            "mode = \"jwt\"\nname = \"catalog\"\njwt_type = \"example-svc+jwt\"\ntrust = \"{{}}\"\n{extra}"
        ))
        .unwrap()
    }

    #[test]
    fn none_admits_unauthenticated_grpc_only_in_development() {
        let none = ServiceAuthConfig::default();
        for env in ["local", "development"] {
            assert_eq!(none.validate(env, true), Ok(()));
        }
        for env in ["production", "staging", "Production", "dev"] {
            let error = none.validate(env, true).unwrap_err();
            assert_eq!(error.key, "service_auth.mode", "{env}");
        }
        // Without a gRPC listener there is nothing to authenticate.
        assert_eq!(none.validate("production", false), Ok(()));
    }

    #[test]
    fn jwt_needs_name_type_and_trust_and_a_key_with_its_id() {
        assert_eq!(jwt("").validate("production", true), Ok(()));
        let signing = jwt("key_id = \"catalog/1\"\nprivate_key = \"pem\"");
        assert_eq!(signing.validate("production", true), Ok(()));
        for (body, key) in [
            (
                "name = \" \"\njwt_type = \"t\"\ntrust = \"{}\"",
                "service_auth.name",
            ),
            (
                "name = \"catalog\"\njwt_type = \" \"\ntrust = \"{}\"",
                "service_auth.jwt_type",
            ),
            (
                "name = \"catalog\"\njwt_type = \"t\"\ntrust = \" \"",
                "service_auth.trust",
            ),
        ] {
            let config: ServiceAuthConfig =
                toml::from_str(&format!("mode = \"jwt\"\n{body}")).unwrap();
            assert_eq!(config.validate("production", true).unwrap_err().key, key);
        }
        let config = jwt("key_id = \"catalog/1\"");
        assert_eq!(
            config.validate("local", true).unwrap_err().key,
            "service_auth.private_key"
        );
        let config = jwt("private_key = \"pem\"\nkey_id = \" \"");
        assert_eq!(
            config.validate("local", true).unwrap_err().key,
            "service_auth.key_id"
        );
    }

    #[test]
    fn none_accepts_no_dormant_input_and_debug_names_only_the_mode() {
        assert!(toml::from_str::<ServiceAuthConfig>("mode = \"none\"\nname = \"x\"\n").is_err());
        let config = jwt("key_id = \"catalog/1\"\nprivate_key = \"-----BEGIN PRIVATE KEY-----\"");
        assert_eq!(
            format!("{config:?}"),
            r#"ServiceAuthConfig { mode: "jwt", .. }"#
        );
        assert_eq!(config.mode(), "jwt");
    }
}
