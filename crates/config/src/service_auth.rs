use secrecy::SecretString;
use serde::Deserialize;

use crate::de::{BlankAsNone, BlankAsNoneVerbatim};
use crate::validate::{Rejected, ValidationError, Violations, optional_pair, required_non_blank};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum ServiceAuthMode {
    /// Calls carry and need no service token. With gRPC enabled this is
    /// accepted only with `app.allow_insecure_development`. This is the
    /// omitted-section default.
    #[default]
    None,
    Jwt,
}

#[serde_with::serde_as]
#[derive(Clone, derive_more::Debug, Default, Deserialize)]
#[debug("ServiceAuthInput {{ .. }}")]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ServiceAuthInput {
    mode: ServiceAuthMode,
    /// This service's name: the audience it accepts and its tokens'
    /// `iss` and `sub`.
    name: Option<String>,
    jwt_type: Option<String>,
    trust: Option<String>,
    /// The `kid` receivers hold this service's public key under.
    /// Required together with `private_key`.
    #[serde(default)]
    #[serde_as(as = "BlankAsNone")]
    key_id: Option<String>,
    /// Environment-only PKCS#8 PEM of this service's EC P-256 signing
    /// key. Without it the service verifies callers but calls no one.
    #[serde(default)]
    #[serde_as(as = "BlankAsNoneVerbatim")]
    private_key: Option<SecretString>,
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
        local_development: bool,
        grpc_enabled: bool,
        v: &mut Violations,
    ) -> Result<ServiceAuthConfig, Rejected> {
        match self.mode {
            ServiceAuthMode::None => {
                if grpc_enabled && !local_development {
                    v.reject(ValidationError::new(
                        "service_auth.mode",
                        "must be jwt when grpc.enabled is true unless app.allow_insecure_development is enabled",
                    ));
                }
                v.reject_present(
                    &[
                        ("service_auth.name", self.name.is_some()),
                        ("service_auth.jwt_type", self.jwt_type.is_some()),
                        ("service_auth.trust", self.trust.is_some()),
                        ("service_auth.key_id", self.key_id.is_some()),
                        ("service_auth.private_key", self.private_key.is_some()),
                    ],
                    "is only valid when service_auth.mode = jwt",
                );
                Ok(ServiceAuthConfig::None)
            }
            ServiceAuthMode::Jwt => {
                let condition = "service_auth.mode = jwt";
                let name = v.record(required_non_blank(
                    "service_auth.name",
                    self.name,
                    condition,
                ));
                let jwt_type = v.record(required_non_blank(
                    "service_auth.jwt_type",
                    self.jwt_type,
                    condition,
                ));
                let trust = v.record(required_non_blank(
                    "service_auth.trust",
                    self.trust,
                    condition,
                ));
                let signing_key = v
                    .record(optional_pair(
                        "service_auth.key_id",
                        self.key_id,
                        "service_auth.private_key",
                        self.private_key,
                    ))
                    .map(|pair| {
                        pair.map(|(key_id, private_key)| ServiceSigningKey {
                            key_id,
                            private_key,
                        })
                    });
                Ok(ServiceAuthConfig::Jwt {
                    name: name?,
                    jwt_type: jwt_type?,
                    trust: trust?,
                    signing_key: signing_key?,
                })
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServiceSigningKey {
    pub key_id: String,
    pub private_key: SecretString,
}

#[cfg(test)]
mod tests;
