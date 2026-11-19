use std::fmt;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;

use crate::de::{blank_as_none, blank_secret_as_none};
use crate::listener_limits::{
    DEFAULT_ADMISSION_WAIT, DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_IN_FLIGHT,
    DEFAULT_REQUEST_TIMEOUT, RawListenerLimits,
};
use crate::validate::{Rejected, Violations, required};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GrpcSecurity {
    /// The operator supplies the enclosing network trust boundary.
    Plaintext,
    Tls,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct GrpcInput {
    #[serde(deserialize_with = "crate::de::boolean")]
    pub(crate) enabled: bool,
    #[serde(deserialize_with = "crate::de::boolean")]
    trust_trace_context: bool,
    /// An IP `host:port`, or `:port` for IPv4 all-interfaces (`0.0.0.0`).
    /// Hostnames are refused; load does not look them up. Blank is absent.
    #[serde(default, deserialize_with = "crate::de::optional_listen_addr")]
    addr: Option<SocketAddr>,
    #[serde(default, deserialize_with = "crate::de::optional_string_enum")]
    security: Option<GrpcSecurity>,
    #[serde(default, deserialize_with = "blank_as_none")]
    certificate: Option<String>,
    #[serde(default, deserialize_with = "blank_secret_as_none")]
    private_key: Option<SecretString>,
    #[serde(default, deserialize_with = "blank_as_none")]
    client_ca: Option<String>,
    #[serde(deserialize_with = "crate::de::duration")]
    request_timeout: Duration,
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    max_in_flight: u32,
    #[serde(deserialize_with = "crate::de::duration")]
    admission_wait: Duration,
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    max_connections: u32,
    #[serde(deserialize_with = "crate::de::duration")]
    max_connection_age: Duration,
}

impl Default for GrpcInput {
    fn default() -> Self {
        Self {
            enabled: false,
            trust_trace_context: false,
            addr: None,
            security: None,
            certificate: None,
            private_key: None,
            client_ca: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_in_flight: DEFAULT_MAX_IN_FLIGHT,
            admission_wait: DEFAULT_ADMISSION_WAIT,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_connection_age: Duration::from_mins(30),
        }
    }
}

impl GrpcInput {
    pub(crate) fn admit(
        self,
        drain_budget: Option<Duration>,
        violations: &mut Violations,
    ) -> Result<Option<GrpcConfig>, Rejected> {
        if !self.enabled {
            return Ok(None);
        }
        let addr = violations.take(required("grpc.addr", self.addr, ENABLED));
        let limits = RawListenerLimits {
            request_timeout: self.request_timeout,
            max_in_flight: self.max_in_flight,
            admission_wait: self.admission_wait,
            max_connections: self.max_connections,
            max_connection_age: self.max_connection_age,
        }
        .admit("grpc", drain_budget, "calls", violations);
        let security = Self::admit_security(
            self.security,
            self.certificate,
            self.private_key,
            self.client_ca,
            violations,
        );
        Ok(Some(GrpcConfig {
            trust_trace_context: self.trust_trace_context,
            addr: addr?,
            security: security?,
            request_timeout: limits.request_timeout,
            max_in_flight: limits.max_in_flight,
            admission_wait: limits.admission_wait,
            max_connections: limits.max_connections,
            max_connection_age: limits.max_connection_age,
        }))
    }

    fn admit_security(
        security: Option<GrpcSecurity>,
        certificate: Option<String>,
        private_key: Option<SecretString>,
        client_ca: Option<String>,
        violations: &mut Violations,
    ) -> Result<GrpcListenerSecurity, Rejected> {
        match violations.take(required("grpc.security", security, ENABLED))? {
            GrpcSecurity::Plaintext => match violations.reject_present(
                &[
                    ("grpc.certificate", certificate.is_some()),
                    ("grpc.private_key", private_key.is_some()),
                    ("grpc.client_ca", client_ca.is_some()),
                ],
                "is only valid when grpc.security is tls",
            ) {
                Some(rejected) => Err(rejected),
                None => Ok(GrpcListenerSecurity::Plaintext),
            },
            GrpcSecurity::Tls => {
                let certificate =
                    violations.take(required("grpc.certificate", certificate, TLS_SELECTED));
                let private_key =
                    violations.take(required("grpc.private_key", private_key, TLS_SELECTED));
                Ok(GrpcListenerSecurity::Tls(GrpcTlsMaterial {
                    certificate: certificate?,
                    private_key: private_key?,
                    client_ca,
                }))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct GrpcConfig {
    pub trust_trace_context: bool,
    pub addr: SocketAddr,
    pub security: GrpcListenerSecurity,
    pub request_timeout: Duration,
    pub max_in_flight: Option<NonZeroU32>,
    pub admission_wait: Duration,
    pub max_connections: Option<NonZeroU32>,
    pub max_connection_age: Option<Duration>,
}

#[derive(Clone)]
pub enum GrpcListenerSecurity {
    Plaintext,
    Tls(GrpcTlsMaterial),
}

impl GrpcListenerSecurity {
    #[must_use]
    pub const fn mode(&self) -> &'static str {
        match self {
            Self::Plaintext => "plaintext",
            Self::Tls(_) => "tls",
        }
    }
}

impl fmt::Debug for GrpcListenerSecurity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.mode())
    }
}

#[derive(Clone, derive_more::Debug)]
pub struct GrpcTlsMaterial {
    #[debug(skip)]
    pub certificate: String,
    #[debug(skip)]
    pub private_key: SecretString,
    #[debug("{:?}", client_ca.is_some())]
    pub client_ca: Option<String>,
}

const ENABLED: &str = "grpc.enabled is true";
const TLS_SELECTED: &str = "grpc.security is tls";

#[cfg(test)]
mod tests;
