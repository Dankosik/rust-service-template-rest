//! Optional cache profile: DSN, TLS escape hatches, and the command budget.
//!
//! The section is inert until `dsn` is set. The DSN is secret-like, so it
//! arrives through the environment only. URL shape, TLS, and password
//! admission stay in `infra-cache`, which is the crate that parses what the
//! driver would connect to. `command_timeout` is the one budget an operator
//! sets; connect and keepalive ceilings are template constants there.

use std::path::PathBuf;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;

use crate::app::is_local_development;
use crate::de::{blank_as_none, blank_secret_as_none};
use crate::validate::{ValidationError, duration_range};

/// Optional Redis-compatible cache. Absent `dsn` keeps the profile inert.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CacheConfig {
    /// `redis://`, `rediss://`, `valkey://`, or `valkeys://`, password included.
    /// Environment only (`APP__CACHE__DSN`). Missing, empty, or whitespace-only
    /// is absent (`None`).
    #[serde(default, deserialize_with = "blank_secret_as_none")]
    pub dsn: Option<SecretString>,
    /// PEM root CA path for a private certificate. Missing, empty, or
    /// whitespace-only is unset (`None`).
    #[serde(default, deserialize_with = "blank_as_none")]
    pub root_ca_path: Option<PathBuf>,
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
            root_ca_path: None,
            allow_plaintext: false,
            allow_unauthenticated: false,
            // A degraded call must still leave most of an HTTP request for the source of truth.
            command_timeout: Duration::from_millis(100),
        }
    }
}

impl CacheConfig {
    /// Whether a DSN is present. Shape is validated by `infra-cache`.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.dsn.is_some()
    }

    /// `request_timeout` is `http.request_timeout`: one degraded call of an
    /// active cache must leave at least half of it for the source of truth.
    pub(crate) fn validate(
        &self,
        app_env: &str,
        request_timeout: Duration,
    ) -> Result<(), ValidationError> {
        let local_development = is_local_development(app_env);
        if self.allow_plaintext && !local_development {
            return Err(ValidationError::new(
                "cache.allow_plaintext",
                "is local/development-only",
            ));
        }
        if self.allow_unauthenticated && !local_development {
            return Err(ValidationError::new(
                "cache.allow_unauthenticated",
                "is local/development-only",
            ));
        }
        duration_range(
            "cache.command_timeout",
            self.command_timeout,
            Duration::from_millis(1),
            Duration::from_secs(1),
        )?;
        // The range above keeps the doubling far from overflow.
        if self.is_active() && self.command_timeout * 2 > request_timeout {
            return Err(ValidationError::new(
                "cache.command_timeout",
                "must be at most half of http.request_timeout",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

    #[test]
    fn defaults_are_inert() {
        let config = CacheConfig::default();
        assert!(!config.is_active());
        assert_eq!(config.command_timeout, Duration::from_millis(100));
        config.validate("production", REQUEST_TIMEOUT).unwrap();
    }

    #[test]
    fn debug_output_redacts_the_dsn() {
        let config = CacheConfig {
            dsn: Some(SecretString::from(
                "redis://:hunter2@127.0.0.1:6379".to_owned(),
            )),
            ..CacheConfig::default()
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
    }

    #[test]
    fn allow_flags_are_refused_outside_local_development() {
        for app_env in ["production", "staging"] {
            let plaintext = CacheConfig {
                allow_plaintext: true,
                ..CacheConfig::default()
            };
            assert_eq!(
                plaintext
                    .validate(app_env, REQUEST_TIMEOUT)
                    .unwrap_err()
                    .key,
                "cache.allow_plaintext"
            );
            let unauthenticated = CacheConfig {
                allow_unauthenticated: true,
                ..CacheConfig::default()
            };
            assert_eq!(
                unauthenticated
                    .validate(app_env, REQUEST_TIMEOUT)
                    .unwrap_err()
                    .key,
                "cache.allow_unauthenticated"
            );
        }
        let local = CacheConfig {
            allow_plaintext: true,
            allow_unauthenticated: true,
            ..CacheConfig::default()
        };
        local.validate("local", REQUEST_TIMEOUT).unwrap();
        local.validate("development", REQUEST_TIMEOUT).unwrap();
    }

    #[test]
    fn command_timeout_is_bounded() {
        for command_timeout in [Duration::ZERO, Duration::from_millis(1_001)] {
            let config = CacheConfig {
                command_timeout,
                ..CacheConfig::default()
            };
            assert_eq!(
                config
                    .validate("production", REQUEST_TIMEOUT)
                    .unwrap_err()
                    .key,
                "cache.command_timeout"
            );
        }
        let edges = CacheConfig {
            command_timeout: Duration::from_secs(1),
            ..CacheConfig::default()
        };
        edges.validate("production", REQUEST_TIMEOUT).unwrap();
    }

    #[test]
    fn an_active_cache_must_leave_half_the_request_budget() {
        let mut config = Config::default();
        config.app.version = "0.0.0".to_owned();
        config.app.commit = "test".to_owned();
        config.cache.dsn = Some(SecretString::from(
            "rediss://:secret@cache.example:6379".to_owned(),
        ));
        config.cache.command_timeout = Duration::from_millis(100);
        config.http.request_timeout = Duration::from_millis(150);
        let err = config.validate().unwrap_err();
        assert_eq!(err.key, "cache.command_timeout");

        config.http.request_timeout = Duration::from_millis(200);
        config.validate().unwrap();
    }
}
