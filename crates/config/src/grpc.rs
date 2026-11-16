//! Optional native gRPC listener configuration.
//!
//! This section contains immutable operator input only. TLS parsing and the
//! listener remain transport work, so disabled configuration performs no file
//! or network I/O.

use std::fmt;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;

use crate::de::{blank_as_none, blank_secret_as_none};
use crate::listener_limits::{
    DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_IN_FLIGHT, DEFAULT_REQUEST_TIMEOUT, connection_age_range,
    connection_count_range, in_flight_range, request_budget_fits_drain, request_timeout_range,
};
use crate::validate::{Rejected, ValidationError, Violations, duration_range, required};

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
    pub(crate) enabled: bool,
    /// An IP `host:port`, or `:port` for IPv4 all-interfaces (`0.0.0.0`).
    /// Hostnames are refused; load does not look them up. Blank is absent.
    #[serde(default, deserialize_with = "crate::de::optional_listen_addr")]
    addr: Option<SocketAddr>,
    security: Option<GrpcSecurity>,
    #[serde(default, deserialize_with = "blank_as_none")]
    certificate: Option<String>,
    #[serde(default, deserialize_with = "blank_secret_as_none")]
    private_key: Option<SecretString>,
    #[serde(default, deserialize_with = "blank_as_none")]
    client_ca: Option<String>,
    #[serde(with = "humantime_serde")]
    request_timeout: Duration,
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    max_in_flight: u32,
    #[serde(with = "humantime_serde")]
    admission_wait: Duration,
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    max_connections: u32,
    #[serde(with = "humantime_serde")]
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
            // A permit frees within a second unless the process stays
            // saturated, and then a refusal helps a caller that fails over
            // quickly more than a wait up to its whole deadline.
            admission_wait: Duration::from_secs(1),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            // A gRPC channel keeps one connection for the life of its
            // process, so behind a connection-level balancer replicas added
            // later get no calls until something closes it. Thirty minutes
            // bounds that at a reconnect cost no caller notices.
            max_connection_age: Duration::from_mins(30),
        }
    }
}

impl GrpcInput {
    /// The listener when `grpc.enabled`, after every listener rule passed.
    /// A disabled section is inert: its keys are neither checked nor read.
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
        // Both rules below compare against a timeout that passed its own range.
        if timeout_in_range {
            if let Some(drain_budget) = drain_budget {
                violations.check(request_budget_fits_drain(
                    "grpc.request_timeout",
                    self.request_timeout,
                    drain_budget,
                    "calls",
                ));
            }
            violations.check(duration_range(
                "grpc.admission_wait",
                self.admission_wait,
                Duration::ZERO,
                self.request_timeout,
            ));
        }
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

/// The admitted native gRPC listener.
#[derive(Clone)]
pub struct GrpcConfig {
    pub addr: SocketAddr,
    pub security: GrpcListenerSecurity,
    /// Upper bound for a business call's time to response headers. A
    /// caller's shorter `grpc-timeout` wins.
    pub request_timeout: Duration,
    /// Independent bounds on business openings before authentication and calls
    /// through terminal status, shared by router clones. Either bound sheds with
    /// `RESOURCE_EXHAUSTED`. Zero disables both; health is outside both.
    pub max_in_flight: u32,
    /// Longest a call over `max_in_flight` waits for a permit before it is
    /// refused; the call's own budget also bounds the wait. Zero refuses at once.
    pub admission_wait: Duration,
    /// Accepted connections at once. At the cap the accept loop closes the
    /// socket with no response. Zero accepts without a bound.
    pub max_connections: u32,
    /// Age after which a connection gets GOAWAY, so its client reconnects
    /// and is balanced again; open calls on it finish. Each connection's age
    /// is spread by up to 10% either way. Zero keeps a connection for as
    /// long as its peer does.
    pub max_connection_age: Duration,
}

impl fmt::Debug for GrpcConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrpcConfig")
            .field("addr", &self.addr)
            .field("security", &self.security)
            .field("request_timeout", &self.request_timeout)
            .field("max_in_flight", &self.max_in_flight)
            .field("admission_wait", &self.admission_wait)
            .field("max_connections", &self.max_connections)
            .field("max_connection_age", &self.max_connection_age)
            .finish_non_exhaustive()
    }
}

impl GrpcConfig {
    #[must_use]
    pub fn in_flight_cap(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.max_in_flight)
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

/// Admitted listener security. `Debug` names the mode, never the material.
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

/// PEM material for the TLS listener; a present `client_ca` requires mTLS.
#[derive(Clone)]
pub struct GrpcTlsMaterial {
    pub certificate: String,
    pub private_key: SecretString,
    pub client_ca: Option<String>,
}

impl fmt::Debug for GrpcTlsMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrpcTlsMaterial")
            .field("client_ca", &self.client_ca.is_some())
            .finish_non_exhaustive()
    }
}

const ENABLED: &str = "grpc.enabled is true";
const TLS_SELECTED: &str = "grpc.security is tls";

#[cfg(test)]
mod tests;
