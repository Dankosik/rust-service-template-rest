//! Optional native gRPC listener configuration.
//!
//! This section contains immutable operator input only. TLS parsing and the
//! listener remain transport work, so disabled configuration performs no file
//! or network I/O.

use std::fmt;
use std::net::SocketAddr;

use secrecy::SecretString;
use serde::Deserialize;

use crate::de::{blank_as_none, blank_secret_as_none};
use crate::validate::ValidationError;

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
#[derive(Clone, Default, Deserialize)]
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
}

impl fmt::Debug for GrpcConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrpcConfig")
            .field("enabled", &self.enabled)
            .field("security", &self.security)
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

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if !self.enabled {
            return Ok(());
        }

        self.listen_addr()?;
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
        config.validate().unwrap();
        let debug = format!("{config:?}");
        assert!(debug.contains("enabled"));
        assert!(!debug.contains("certificate"));
        assert!(!debug.contains("private_key"));
        assert!(!debug.contains("client_ca"));
    }

    #[test]
    fn enabled_plaintext_requires_an_address_and_explicit_security() {
        let missing = parse("enabled = true").unwrap();
        assert_eq!(missing.validate().unwrap_err().key, "grpc.addr");

        let missing = parse("enabled = true\naddr = \"127.0.0.1:50051\"").unwrap();
        assert_eq!(missing.validate().unwrap_err().key, "grpc.security");

        let config =
            parse("enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"plaintext\"").unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn tls_requires_certificate_and_environment_only_private_key() {
        let config = parse("enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"tls\"").unwrap();
        assert_eq!(config.validate().unwrap_err().key, "grpc.certificate");

        let config = parse(
            "enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"tls\"\ncertificate = \"cert\"",
        )
        .unwrap();
        assert_eq!(config.validate().unwrap_err().key, "grpc.private_key");
    }

    #[test]
    fn plaintext_refuses_tls_material() {
        let config = parse(
            "enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"plaintext\"\ncertificate = \"cert\"",
        )
        .unwrap();
        assert_eq!(config.validate().unwrap_err().key, "grpc.certificate");
    }
}
