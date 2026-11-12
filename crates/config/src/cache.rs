//! Optional cache profile: DSN, TLS escape hatches, and the command budget.
//!
//! The section is inert until `dsn` is set. The DSN is secret-like, so it
//! arrives only as its `APP__` variable. URL shape, TLS, and password
//! admission stay in `infra-cache`, which is the crate that parses what the
//! driver would connect to. `command_timeout` is the one budget an operator
//! sets; connect and keepalive ceilings are template constants there.

use std::path::{Path, PathBuf};
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;

use crate::app::local_development_only;
use crate::de::{blank_as_none, blank_secret_as_none};
use crate::validate::{ValidationError, duration_range, missing};

/// Optional Redis-compatible cache. Absent `dsn` keeps the profile inert.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CacheConfig {
    /// `redis://`, `rediss://`, `valkey://`, or `valkeys://`, password included
    /// unless `password_file` is set.
    /// Set `APP__CACHE__DSN` in the environment or via `--secrets-dir`.
    /// Missing, empty, or whitespace-only is absent (`None`).
    #[serde(default, deserialize_with = "blank_secret_as_none")]
    pub dsn: Option<SecretString>,
    /// A file that holds the password alone, for a platform that rotates it
    /// by rewriting the file; its trailing line breaks are not part of the
    /// password. The DSN then carries no password, and the running service
    /// follows the file. Missing, empty, or whitespace-only is unset (`None`),
    /// the default.
    #[serde(default, deserialize_with = "blank_as_none")]
    pub password_file: Option<PathBuf>,
    /// PEM root CA path for a private certificate. Missing, empty, or
    /// whitespace-only is unset (`None`).
    #[serde(default, deserialize_with = "blank_as_none")]
    pub root_ca_path: Option<PathBuf>,
    /// PEM certificate chain this client presents to a server that requires
    /// one (mutual TLS). Set together with `client_key_path`. Missing, empty,
    /// or whitespace-only is unset (`None`).
    #[serde(default, deserialize_with = "blank_as_none")]
    pub client_cert_path: Option<PathBuf>,
    /// PEM private key of that certificate. The key is a path, so a file may
    /// set it; the key itself stays in the file it names.
    #[serde(default, deserialize_with = "blank_as_none")]
    pub client_key_path: Option<PathBuf>,
    /// Permit a plaintext DSN. Local and development only.
    pub allow_plaintext: bool,
    /// Permit a DSN with no password. Local and development only.
    pub allow_unauthenticated: bool,
    /// Bound for one cache command, including a reconnect wait.
    #[serde(with = "humantime_serde")]
    pub command_timeout: Duration,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            dsn: None,
            password_file: None,
            root_ca_path: None,
            client_cert_path: None,
            client_key_path: None,
            allow_plaintext: false,
            allow_unauthenticated: false,
            // A hang guard, not a latency target: a slow reply is waited for.
            // Validation still keeps one degraded call inside half a request.
            command_timeout: Duration::from_secs(2),
        }
    }
}

impl CacheConfig {
    #[must_use]
    fn is_active(&self) -> bool {
        self.dsn.is_some()
    }

    /// The client certificate presented for mutual TLS, if one is set.
    ///
    /// # Errors
    ///
    /// Returns the path of the pair that is missing when only one is set: a
    /// certificate without its key, or a key without its certificate, would
    /// silently present nothing.
    pub fn client_certificate(
        &self,
    ) -> Result<Option<CacheClientCertificate<'_>>, ValidationError> {
        match (&self.client_cert_path, &self.client_key_path) {
            (Some(cert_path), Some(key_path)) => Ok(Some(CacheClientCertificate {
                cert_path,
                key_path,
            })),
            (Some(_), None) => Err(missing(
                "cache.client_key_path",
                "cache.client_cert_path is set",
            )),
            (None, Some(_)) => Err(missing(
                "cache.client_cert_path",
                "cache.client_key_path is set",
            )),
            (None, None) => Ok(None),
        }
    }

    /// `request_timeout` is `http.request_timeout`: one degraded call of an
    /// active cache must leave at least half of it for the source of truth.
    pub(crate) fn validate(
        &self,
        app_env: &str,
        request_timeout: Duration,
    ) -> Result<(), ValidationError> {
        local_development_only("cache.allow_plaintext", self.allow_plaintext, app_env)?;
        local_development_only(
            "cache.allow_unauthenticated",
            self.allow_unauthenticated,
            app_env,
        )?;
        self.client_certificate()?;
        duration_range(
            "cache.command_timeout",
            self.command_timeout,
            Duration::from_millis(1),
            Duration::from_secs(10),
        )?;
        // The range above keeps the doubling far from overflow.
        if self.is_active() && self.command_timeout * 2 > request_timeout {
            return Err(ValidationError::new(
                "cache.command_timeout",
                format!(
                    "must be at most half of http.request_timeout ({}), got {}",
                    humantime::format_duration(request_timeout / 2),
                    humantime::format_duration(self.command_timeout)
                ),
            ));
        }
        Ok(())
    }
}

/// The client certificate [`CacheConfig::client_certificate`] admitted.
#[derive(Clone, Copy, Debug)]
pub struct CacheClientCertificate<'a> {
    /// PEM certificate chain.
    pub cert_path: &'a Path,
    /// PEM private key of that certificate.
    pub key_path: &'a Path,
}

#[cfg(test)]
mod tests;
