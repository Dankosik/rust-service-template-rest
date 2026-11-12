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

use crate::HttpConfig;
use crate::de::{blank_as_none, blank_secret_as_none};
use crate::http::{
    DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_IN_FLIGHT, DEFAULT_REQUEST_TIMEOUT, connection_age_range,
    connection_count_range, in_flight_range, request_budget_fits_drain, request_timeout_range,
};
use crate::validate::{ValidationError, duration_range, required};

/// Explicit security mode for a native gRPC server or client.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GrpcSecurity {
    /// The operator supplies the enclosing network trust boundary.
    Plaintext,
    /// TLS 1.3 is constructed from the supplied material by the transport.
    Tls,
}

/// Immutable input for the optional native gRPC listener.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GrpcConfig {
    /// Whether bootstrap starts a native gRPC listener.
    pub enabled: bool,
    /// Listener address, required only when the listener is enabled.
    /// An IP `host:port`, or `:port` for IPv4 all-interfaces (`0.0.0.0`).
    /// Hostnames are refused; load does not look them up. Blank is absent.
    #[serde(default, deserialize_with = "crate::de::optional_listen_addr")]
    pub addr: Option<SocketAddr>,
    /// Explicit listener security mode, required only when enabled.
    pub security: Option<GrpcSecurity>,
    /// PEM certificate chain for TLS, never read while the listener is off.
    #[serde(default, deserialize_with = "blank_as_none")]
    pub certificate: Option<String>,
    /// Environment-only PEM private key for TLS.
    #[serde(default, deserialize_with = "blank_secret_as_none")]
    pub private_key: Option<SecretString>,
    /// Optional PEM client trust anchor; present means mTLS is required.
    #[serde(default, deserialize_with = "blank_as_none")]
    pub client_ca: Option<String>,
    /// Upper bound for a business call's time to response headers. A
    /// caller's shorter `grpc-timeout` wins.
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
    /// Independent bounds on business openings before authentication and calls
    /// through terminal status, shared by router clones. Either bound sheds with
    /// `RESOURCE_EXHAUSTED`. Zero disables both; health is outside both.
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    pub max_in_flight: u32,
    /// Longest a call over `max_in_flight` waits for a permit before it is
    /// refused; the call's own budget also bounds the wait. The default of one
    /// second suits a caller that fails over quickly: a permit frees within a
    /// second unless the process stays saturated, and then a refusal helps the
    /// caller more than a wait up to its whole deadline. Zero refuses at once.
    #[serde(with = "humantime_serde")]
    pub admission_wait: Duration,
    /// Accepted connections at once. At the cap the accept loop closes the
    /// socket with no response. Zero accepts without a bound.
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    pub max_connections: u32,
    /// Age after which a connection gets GOAWAY, so its client reconnects
    /// and is balanced again; open calls on it finish. Each connection's age
    /// is spread by up to 10% either way. Zero keeps a connection for as
    /// long as its peer does.
    #[serde(with = "humantime_serde")]
    pub max_connection_age: Duration,
}

impl Default for GrpcConfig {
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

/// Leaves trust and credential inputs out of `Debug` so they never reach logs.
impl fmt::Debug for GrpcConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrpcConfig")
            .field("enabled", &self.enabled)
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
    /// The listener address when one was configured.
    ///
    /// # Errors
    ///
    /// Returns an error when `grpc.addr` is absent.
    pub fn required_addr(&self) -> Result<SocketAddr, ValidationError> {
        required("grpc.addr", self.addr, "grpc.enabled is true")
    }

    /// Adapter form of `grpc.max_in_flight`: `None` means shedding is off.
    #[must_use]
    pub fn in_flight_cap(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.max_in_flight)
    }

    /// Adapter form of `grpc.max_connections`: `None` means unbounded.
    #[must_use]
    pub fn connection_cap(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.max_connections)
    }

    /// Adapter form of `grpc.max_connection_age`: `None` means no age.
    #[must_use]
    pub fn max_connection_age(&self) -> Option<Duration> {
        (!self.max_connection_age.is_zero()).then_some(self.max_connection_age)
    }

    /// The listener's security mode with the material it needs: one owner
    /// for the rule the validator and the transport conversion both apply.
    ///
    /// # Errors
    ///
    /// Returns the key that is missing for the selected mode, or the TLS
    /// key that plaintext must not carry.
    pub fn listener_security(&self) -> Result<GrpcListenerSecurity<'_>, ValidationError> {
        let security = required("grpc.security", self.security, "grpc.enabled is true")?;
        match security {
            GrpcSecurity::Plaintext => {
                for (key, present) in [
                    ("grpc.certificate", self.certificate.is_some()),
                    ("grpc.private_key", self.private_key.is_some()),
                    ("grpc.client_ca", self.client_ca.is_some()),
                ] {
                    if present {
                        return Err(ValidationError::new(
                            key,
                            "is only valid when grpc.security is tls",
                        ));
                    }
                }
                Ok(GrpcListenerSecurity::Plaintext)
            }
            GrpcSecurity::Tls => Ok(GrpcListenerSecurity::Tls {
                certificate: required(
                    "grpc.certificate",
                    self.certificate
                        .as_deref()
                        .filter(|certificate| !certificate.is_empty()),
                    TLS_SELECTED,
                )?,
                private_key: required("grpc.private_key", self.private_key.as_ref(), TLS_SELECTED)?,
                client_ca: self.client_ca.as_deref(),
            }),
        }
    }

    /// `http` owns the drain both listeners share.
    pub(crate) fn validate(&self, http: &HttpConfig) -> Result<(), ValidationError> {
        if !self.enabled {
            return Ok(());
        }

        self.required_addr()?;
        self.validate_limits(http)?;
        self.listener_security()?;
        Ok(())
    }

    fn validate_limits(&self, http: &HttpConfig) -> Result<(), ValidationError> {
        request_timeout_range("grpc.request_timeout", self.request_timeout)?;
        request_budget_fits_drain(
            "grpc.request_timeout",
            self.request_timeout,
            http.effective_drain_budget(),
            "calls",
        )?;
        in_flight_range("grpc.max_in_flight", self.max_in_flight)?;
        duration_range(
            "grpc.admission_wait",
            self.admission_wait,
            Duration::ZERO,
            self.request_timeout,
        )?;
        connection_count_range("grpc.max_connections", self.max_connections)?;
        connection_age_range("grpc.max_connection_age", self.max_connection_age)
    }
}

/// Admitted listener security. It has no `Debug`, so the TLS material it
/// borrows cannot reach a log through it.
#[expect(
    missing_debug_implementations,
    reason = "the view borrows TLS material that must not be formatted"
)]
pub enum GrpcListenerSecurity<'a> {
    /// No TLS material; the operator supplies the network trust boundary.
    Plaintext,
    /// TLS 1.3 material for the transport to parse.
    Tls {
        /// PEM certificate chain.
        certificate: &'a str,
        /// PEM private key.
        private_key: &'a SecretString,
        /// PEM client trust anchor; present means mTLS is required.
        client_ca: Option<&'a str>,
    },
}

const TLS_SELECTED: &str = "grpc.security is tls";

#[cfg(test)]
mod tests;
