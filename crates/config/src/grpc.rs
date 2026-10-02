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
use crate::validate::{ValidationError, duration_range, int_range};

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
    /// Business calls running at once before shedding with
    /// `RESOURCE_EXHAUSTED`. Zero disables shedding. Health is outside it.
    pub max_in_flight: u32,
    /// Accepted connections at once. At the cap the accept loop closes the
    /// socket with no response. Zero accepts without a bound.
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
            // The HTTP request budget and capacity bounds, for the same reasons.
            request_timeout: Duration::from_secs(8),
            max_in_flight: 256,
            max_connections: 4096,
            // A gRPC channel keeps one connection for the life of its
            // process, so behind a connection-level balancer replicas added
            // later get no calls until something closes it. Thirty minutes
            // bounds that at a reconnect cost no caller notices.
            max_connection_age: Duration::from_mins(30),
        }
    }
}

impl fmt::Debug for GrpcConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrpcConfig")
            .field("enabled", &self.enabled)
            .field("security", &self.security)
            .field("request_timeout", &self.request_timeout)
            .field("max_in_flight", &self.max_in_flight)
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
    pub fn listen_addr(&self) -> Result<SocketAddr, ValidationError> {
        self.addr.ok_or_else(|| {
            ValidationError::new("grpc.addr", "is required when grpc.enabled is true")
        })
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
    pub fn connection_age(&self) -> Option<Duration> {
        (!self.max_connection_age.is_zero()).then_some(self.max_connection_age)
    }

    /// `http` owns the drain both listeners share.
    pub(crate) fn validate(&self, http: &HttpConfig) -> Result<(), ValidationError> {
        if !self.enabled {
            return Ok(());
        }

        self.listen_addr()?;
        self.validate_limits(http)?;
        let security = self.security.ok_or_else(|| {
            ValidationError::new("grpc.security", "is required when grpc.enabled is true")
        })?;
        match security {
            GrpcSecurity::Plaintext => {
                if self.certificate.is_some() {
                    return Err(ValidationError::new(
                        "grpc.certificate",
                        "is only valid when grpc.security is tls",
                    ));
                }
                if self.private_key.is_some() {
                    return Err(ValidationError::new(
                        "grpc.private_key",
                        "is only valid when grpc.security is tls",
                    ));
                }
                if self.client_ca.is_some() {
                    return Err(ValidationError::new(
                        "grpc.client_ca",
                        "is only valid when grpc.security is tls",
                    ));
                }
            }
            GrpcSecurity::Tls => {
                required("grpc.certificate", self.certificate.as_deref())?;
                if self.private_key.is_none() {
                    return Err(ValidationError::new(
                        "grpc.private_key",
                        "is required when grpc.security is tls",
                    ));
                }
            }
        }
        Ok(())
    }
}

impl GrpcConfig {
    fn validate_limits(&self, http: &HttpConfig) -> Result<(), ValidationError> {
        duration_range(
            "grpc.request_timeout",
            self.request_timeout,
            Duration::from_millis(100),
            Duration::from_secs(600),
        )?;
        let drain = http.effective_drain_budget();
        if self.request_timeout > drain {
            return Err(ValidationError::new(
                "grpc.request_timeout",
                format!(
                    "must be <= the drain budget after readiness propagation ({}) so in-flight calls can finish",
                    humantime::format_duration(drain)
                ),
            ));
        }
        int_range(
            "grpc.max_in_flight",
            u64::from(self.max_in_flight),
            0,
            100_000,
        )?;
        int_range(
            "grpc.max_connections",
            u64::from(self.max_connections),
            0,
            1_000_000,
        )?;
        crate::http::connection_age_range("grpc.max_connection_age", self.max_connection_age)
    }
}

fn required(key: &'static str, value: Option<&str>) -> Result<(), ValidationError> {
    if value.is_none_or(str::is_empty) {
        return Err(ValidationError::new(
            key,
            "is required when grpc.security is tls",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Result<GrpcConfig, toml::de::Error> {
        toml::from_str(toml)
    }

    #[test]
    fn disabled_listener_is_inert_by_default_and_redacts_all_material() {
        let config = GrpcConfig::default();
        config.validate(&HttpConfig::default()).unwrap();
        let debug = format!("{config:?}");
        assert!(debug.contains("enabled"));
        assert!(!debug.contains("certificate"));
        assert!(!debug.contains("private_key"));
        assert!(!debug.contains("client_ca"));
    }

    #[test]
    fn enabled_plaintext_requires_an_address_and_explicit_security() {
        let missing = parse("enabled = true").unwrap();
        assert_eq!(
            missing.validate(&HttpConfig::default()).unwrap_err().key,
            "grpc.addr"
        );

        let missing = parse("enabled = true\naddr = \"127.0.0.1:50051\"").unwrap();
        assert_eq!(
            missing.validate(&HttpConfig::default()).unwrap_err().key,
            "grpc.security"
        );

        let config =
            parse("enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"plaintext\"").unwrap();
        config.validate(&HttpConfig::default()).unwrap();
    }

    #[test]
    fn tls_requires_certificate_and_environment_only_private_key() {
        let config = parse("enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"tls\"").unwrap();
        assert_eq!(
            config.validate(&HttpConfig::default()).unwrap_err().key,
            "grpc.certificate"
        );

        let config = parse(
            "enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"tls\"\ncertificate = \"cert\"",
        )
        .unwrap();
        assert_eq!(
            config.validate(&HttpConfig::default()).unwrap_err().key,
            "grpc.private_key"
        );
    }

    fn enabled() -> GrpcConfig {
        parse("enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"plaintext\"").unwrap()
    }

    #[test]
    fn limits_default_to_the_http_bounds_and_zero_turns_one_off() {
        let config = enabled();
        assert_eq!(config.request_timeout, Duration::from_secs(8));
        assert_eq!(config.in_flight_cap().map(NonZeroU32::get), Some(256));
        assert_eq!(config.connection_cap().map(NonZeroU32::get), Some(4096));
        assert_eq!(config.connection_age(), Some(Duration::from_mins(30)));

        let unbounded = GrpcConfig {
            max_in_flight: 0,
            max_connections: 0,
            max_connection_age: Duration::ZERO,
            ..enabled()
        };
        unbounded.validate(&HttpConfig::default()).unwrap();
        assert_eq!(unbounded.in_flight_cap(), None);
        assert_eq!(unbounded.connection_cap(), None);
        assert_eq!(unbounded.connection_age(), None);
    }

    #[test]
    fn a_call_budget_must_fit_inside_the_shared_drain() {
        let config = GrpcConfig {
            request_timeout: Duration::from_secs(11),
            ..enabled()
        };
        let err = config.validate(&HttpConfig::default()).unwrap_err();
        assert_eq!(err.key, "grpc.request_timeout");

        let http = HttpConfig {
            drain_timeout: Duration::from_secs(40),
            ..HttpConfig::default()
        };
        config.validate(&http).unwrap();
    }

    #[test]
    fn a_connection_age_under_a_second_is_refused() {
        let config = GrpcConfig {
            max_connection_age: Duration::from_millis(10),
            ..enabled()
        };
        let err = config.validate(&HttpConfig::default()).unwrap_err();
        assert_eq!(err.key, "grpc.max_connection_age");
    }

    #[test]
    fn plaintext_refuses_tls_material() {
        let config = parse(
            "enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"plaintext\"\ncertificate = \"cert\"",
        )
        .unwrap();
        assert_eq!(
            config.validate(&HttpConfig::default()).unwrap_err().key,
            "grpc.certificate"
        );
    }
}
