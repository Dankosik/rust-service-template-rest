use std::fmt;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;

use crate::de::{blank_as_none, blank_secret_as_none};
use crate::listener_limits::{
    DEFAULT_ADMISSION_WAIT, DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_IN_FLIGHT,
    DEFAULT_REQUEST_TIMEOUT, admission_wait_cap, admission_wait_range, connection_age_range,
    connection_count_range, in_flight_range, request_budget_fits_drain, request_timeout_range,
};
use crate::validate::{Rejected, ValidationError, Violations, required};

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
        self.check_limits(drain_budget, violations);
        let security = self.admit_security(violations);
        Ok(Some(GrpcConfig {
            addr: addr?,
            security: security?,
            request_timeout: self.request_timeout,
            max_in_flight: self.max_in_flight,
            admission_wait: self.admission_wait,
            max_connections: self.max_connections,
            max_connection_age: self.max_connection_age,
        }))
    }

    fn check_limits(&self, drain_budget: Option<Duration>, violations: &mut Violations) {
        let timeout = request_timeout_range("grpc.request_timeout", self.request_timeout);
        let timeout_in_range = timeout.is_ok();
        violations.check(timeout);
        if timeout_in_range && let Some(drain_budget) = drain_budget {
            violations.check(request_budget_fits_drain(
                "grpc.request_timeout",
                self.request_timeout,
                drain_budget,
                "calls",
            ));
        }
        violations.check(admission_wait_range(
            "grpc.admission_wait",
            self.admission_wait,
        ));
        violations.check(in_flight_range("grpc.max_in_flight", self.max_in_flight));
        violations.check(connection_count_range(
            "grpc.max_connections",
            self.max_connections,
        ));
        violations.check(connection_age_range(
            "grpc.max_connection_age",
            self.max_connection_age,
        ));
    }

    fn admit_security(
        &self,
        violations: &mut Violations,
    ) -> Result<GrpcListenerSecurity, Rejected> {
        match violations.take(required("grpc.security", self.security, ENABLED))? {
            GrpcSecurity::Plaintext => {
                let mut refused = None;
                for (key, present) in [
                    ("grpc.certificate", self.certificate.is_some()),
                    ("grpc.private_key", self.private_key.is_some()),
                    ("grpc.client_ca", self.client_ca.is_some()),
                ] {
                    if present {
                        refused = Some(violations.reject(ValidationError::new(
                            key,
                            "is only valid when grpc.security is tls",
                        )));
                    }
                }
                refused.map_or(Ok(GrpcListenerSecurity::Plaintext), Err)
            }
            GrpcSecurity::Tls => {
                let certificate = violations.take(required(
                    "grpc.certificate",
                    self.certificate
                        .clone()
                        .filter(|certificate| !certificate.is_empty()),
                    TLS_SELECTED,
                ));
                let private_key = violations.take(required(
                    "grpc.private_key",
                    self.private_key.clone(),
                    TLS_SELECTED,
                ));
                Ok(GrpcListenerSecurity::Tls(GrpcTlsMaterial {
                    certificate: certificate?,
                    private_key: private_key?,
                    client_ca: self.client_ca.clone(),
                }))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct GrpcConfig {
    pub addr: SocketAddr,
    pub security: GrpcListenerSecurity,
    pub request_timeout: Duration,
    pub max_in_flight: u32,
    pub admission_wait: Duration,
    pub max_connections: u32,
    pub max_connection_age: Duration,
}

impl GrpcConfig {
    #[must_use]
    pub fn in_flight_cap(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.max_in_flight)
    }

    #[must_use]
    pub fn admission_wait_cap(&self) -> Duration {
        admission_wait_cap(self.admission_wait, self.request_timeout)
    }

    #[must_use]
    pub fn connection_cap(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.max_connections)
    }

    #[must_use]
    pub fn connection_age_cap(&self) -> Option<Duration> {
        (!self.max_connection_age.is_zero()).then_some(self.max_connection_age)
    }
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
