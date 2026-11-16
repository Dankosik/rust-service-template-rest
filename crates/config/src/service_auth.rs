use secrecy::SecretString;
use serde::Deserialize;

use crate::app::is_local_development;
use crate::de::{blank_as_none, blank_secret_as_none};
use crate::validate::{Rejected, ValidationError, Violations, missing, required_when};

#[derive(Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum ServiceAuthInput {
    /// Calls carry and need no service token. With gRPC enabled this is
    /// accepted only in a local or development environment. This is the
    /// omitted-section default.
    None {},
    Jwt {
        /// This service's name: the audience it accepts and its tokens'
        /// `iss` and `sub`.
        #[serde(default)]
        name: String,
        #[serde(default)]
        jwt_type: String,
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

impl Default for ServiceAuthInput {
    fn default() -> Self {
        Self::None {}
    }
}

impl std::fmt::Debug for ServiceAuthInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServiceAuthInput { .. }")
    }
}

#[derive(Clone, Default)]
pub enum ServiceAuthConfig {
    #[default]
    None,
    Jwt {
        name: String,
        jwt_type: String,
        trust: String,
        signing_key: Option<ServiceSigningKey>,
    },
}

impl std::fmt::Debug for ServiceAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAuthConfig")
            .field("mode", &self.mode())
            .finish_non_exhaustive()
    }
}

impl ServiceAuthConfig {
    #[must_use]
    pub const fn mode(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Jwt { .. } => "jwt",
        }
    }
}

impl ServiceAuthInput {
    pub(crate) fn admit(
        self,
        app_env: &str,
        grpc_enabled: bool,
        v: &mut Violations,
    ) -> Result<ServiceAuthConfig, Rejected> {
        match self {
            Self::None {} if grpc_enabled && !is_local_development(app_env) => {
                Err(v.reject(ValidationError::new(
                    "service_auth.mode",
                    format!(
                        "must be jwt when grpc.enabled is true outside app.env local or development, but app.env is \"{app_env}\""
                    ),
                )))
            }
            Self::None {} => Ok(ServiceAuthConfig::None),
            Self::Jwt {
                name,
                jwt_type,
                trust,
                key_id,
                private_key,
            } => {
                let condition = "service_auth.mode = jwt";
                let named = v.take(required_when("service_auth.name", &name, condition));
                let typed = v.take(required_when("service_auth.jwt_type", &jwt_type, condition));
                let trusted = v.take(required_when("service_auth.trust", &trust, condition));
                let signing_key = v.take(signing_key(key_id, private_key));
                named?;
                typed?;
                trusted?;
                Ok(ServiceAuthConfig::Jwt {
                    name,
                    jwt_type,
                    trust,
                    signing_key: signing_key?,
                })
            }
        }
    }
}

fn signing_key(
    key_id: Option<String>,
    private_key: Option<SecretString>,
) -> Result<Option<ServiceSigningKey>, ValidationError> {
    match (key_id, private_key) {
        (Some(key_id), Some(private_key)) => Ok(Some(ServiceSigningKey {
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

#[derive(Clone, Debug)]
pub struct ServiceSigningKey {
    pub key_id: String,
    pub private_key: SecretString,
}

#[cfg(test)]
mod tests;
