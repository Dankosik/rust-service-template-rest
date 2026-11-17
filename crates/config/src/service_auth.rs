use secrecy::SecretString;
use serde::Deserialize;

use crate::app::is_local_development;
use crate::de::{blank_as_none, blank_secret_as_none};
use crate::validate::{Rejected, ValidationError, Violations, missing, required_when};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum ServiceAuthMode {
    /// Calls carry and need no service token. With gRPC enabled this is
    /// accepted only in a local or development environment. This is the
    /// omitted-section default.
    #[default]
    None,
    Jwt,
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ServiceAuthInput {
    #[serde(deserialize_with = "crate::de::string_enum")]
    mode: ServiceAuthMode,
    /// This service's name: the audience it accepts and its tokens'
    /// `iss` and `sub`.
    #[serde(default, deserialize_with = "crate::de::optional_string")]
    name: Option<String>,
    #[serde(default, deserialize_with = "crate::de::optional_string")]
    jwt_type: Option<String>,
    #[serde(default, deserialize_with = "crate::de::optional_string")]
    trust: Option<String>,
    /// The `kid` receivers hold this service's public key under.
    /// Required together with `private_key`.
    #[serde(default, deserialize_with = "blank_as_none")]
    key_id: Option<String>,
    /// Environment-only PKCS#8 PEM of this service's EC P-256 signing
    /// key. Without it the service verifies callers but calls no one.
    #[serde(default, deserialize_with = "blank_secret_as_none")]
    private_key: Option<SecretString>,
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
        match self.mode {
            ServiceAuthMode::None => {
                if grpc_enabled && !is_local_development(app_env) {
                    v.check(Err(ValidationError::new(
                        "service_auth.mode",
                        format!(
                            "must be jwt when grpc.enabled is true outside app.env local or development, but app.env is \"{app_env}\""
                        ),
                    )));
                }
                for (key, present) in [
                    ("service_auth.name", self.name.is_some()),
                    ("service_auth.jwt_type", self.jwt_type.is_some()),
                    ("service_auth.trust", self.trust.is_some()),
                    ("service_auth.key_id", self.key_id.is_some()),
                    ("service_auth.private_key", self.private_key.is_some()),
                ] {
                    if present {
                        v.check(Err(ValidationError::new(
                            key,
                            "is only valid when service_auth.mode = jwt",
                        )));
                    }
                }
                Ok(ServiceAuthConfig::None)
            }
            ServiceAuthMode::Jwt => {
                let condition = "service_auth.mode = jwt";
                let named = v.take(required_when("service_auth.name", self.name, condition));
                let typed = v.take(required_when(
                    "service_auth.jwt_type",
                    self.jwt_type,
                    condition,
                ));
                let trusted = v.take(required_when("service_auth.trust", self.trust, condition));
                let signing_key = v.take(signing_key(self.key_id, self.private_key));
                Ok(ServiceAuthConfig::Jwt {
                    name: named?,
                    jwt_type: typed?,
                    trust: trusted?,
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
