//! Service-to-service authentication with caller-signed tokens.
//!
//! This section validates configuration shape only. Caller-name, token-type,
//! trust-registry and key admission belong to the service authentication
//! adapter, which bootstrap runs before any listener binds.

use secrecy::SecretString;
use serde::Deserialize;

use crate::app::is_local_development;
use crate::de::{blank_as_none, blank_secret_as_none};
use crate::validate::{ValidationError, missing, required_when};

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

/// Leaves trust and credential inputs out of `Debug` so they never reach logs.
impl std::fmt::Debug for ServiceAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAuthConfig")
            .field("mode", &self.mode())
            .finish_non_exhaustive()
    }
}

impl Default for ServiceAuthConfig {
    fn default() -> Self {
        Self::None {}
    }
}

impl ServiceAuthConfig {
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
                    format!(
                        "must be jwt when grpc.enabled is true outside app.env local or development, but app.env is \"{app_env}\""
                    ),
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
                    required_when(key, value, "service_auth.mode = jwt")?;
                }
                ServiceSigningKey::from_optional_parts(key_id.as_deref(), private_key.as_ref())?;
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ServiceSigningKey<'a> {
    /// The `kid` receivers hold the public key under.
    pub key_id: &'a str,
    /// PKCS#8 PEM of the EC P-256 signing key.
    pub private_key: &'a SecretString,
}

impl<'a> ServiceSigningKey<'a> {
    /// Returns `None` when both parts are absent and the service only verifies callers.
    ///
    /// # Errors
    ///
    /// Names the missing configuration key when only one part is set.
    pub fn from_optional_parts(
        key_id: Option<&'a str>,
        private_key: Option<&'a SecretString>,
    ) -> Result<Option<Self>, ValidationError> {
        match (key_id, private_key) {
            (Some(key_id), Some(private_key)) => Ok(Some(Self {
                key_id,
                private_key,
            })),
            (Some(_), None) => Err(missing(
                "service_auth.private_key",
                "service_auth.key_id is set",
            )),
            (None, Some(_)) => Err(missing(
                "service_auth.key_id",
                "service_auth.private_key is set",
            )),
            (None, None) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests;
