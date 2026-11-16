use std::path::PathBuf;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;

use crate::app::local_development_only;
use crate::de::{blank_as_none, blank_secret_as_none};
use crate::validate::{Rejected, ValidationError, Violations, duration_range, missing};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct CacheInput {
    /// `redis://`, `rediss://`, `valkey://`, or `valkeys://`, password included
    /// unless `password_file` is set.
    /// Set `APP__CACHE__DSN` in the environment or via `--secrets-dir`.
    /// Missing, empty, or whitespace-only is absent (`None`).
    #[serde(default, deserialize_with = "blank_secret_as_none")]
    dsn: Option<SecretString>,
    /// A file that holds the password alone, for a platform that rotates it
    /// by rewriting the file; its trailing line breaks are not part of the
    /// password. The DSN then carries no password, and the running service
    /// follows the file. Missing, empty, or whitespace-only is unset (`None`),
    /// the default.
    #[serde(default, deserialize_with = "blank_as_none")]
    password_file: Option<PathBuf>,
    /// PEM root CA path for a private certificate. Missing, empty, or
    /// whitespace-only is unset (`None`).
    #[serde(default, deserialize_with = "blank_as_none")]
    root_ca_path: Option<PathBuf>,
    /// PEM certificate chain this client presents to a server that requires
    /// one (mutual TLS). Set together with `client_key_path`. Missing, empty,
    /// or whitespace-only is unset (`None`).
    #[serde(default, deserialize_with = "blank_as_none")]
    client_cert_path: Option<PathBuf>,
    /// PEM private key of that certificate. The key is a path, so a file may
    /// set it; the key itself stays in the file it names.
    #[serde(default, deserialize_with = "blank_as_none")]
    client_key_path: Option<PathBuf>,
    /// Permit a plaintext DSN. Local and development only.
    allow_plaintext: bool,
    /// Permit a DSN with no password. Local and development only.
    allow_unauthenticated: bool,
    /// Bound for one cache command, including a reconnect wait.
    #[serde(with = "humantime_serde")]
    command_timeout: Duration,
}

impl Default for CacheInput {
    fn default() -> Self {
        let CacheConfig {
            dsn,
            password_file,
            root_ca_path,
            client_certificate: _,
            allow_plaintext,
            allow_unauthenticated,
            command_timeout,
        } = CacheConfig::default();
        Self {
            dsn,
            password_file,
            root_ca_path,
            client_cert_path: None,
            client_key_path: None,
            allow_plaintext,
            allow_unauthenticated,
            command_timeout,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CacheConfig {
    pub dsn: Option<SecretString>,
    pub password_file: Option<PathBuf>,
    pub root_ca_path: Option<PathBuf>,
    pub client_certificate: Option<CacheClientCertificate>,
    pub allow_plaintext: bool,
    pub allow_unauthenticated: bool,
    pub command_timeout: Duration,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            dsn: None,
            password_file: None,
            root_ca_path: None,
            client_certificate: None,
            allow_plaintext: false,
            allow_unauthenticated: false,
            command_timeout: Duration::from_secs(2),
        }
    }
}

impl CacheInput {
    pub(crate) fn admit(
        self,
        app_env: &str,
        request_timeout: Option<Duration>,
        v: &mut Violations,
    ) -> Result<CacheConfig, Rejected> {
        v.check(local_development_only(
            "cache.allow_plaintext",
            self.allow_plaintext,
            app_env,
        ));
        v.check(local_development_only(
            "cache.allow_unauthenticated",
            self.allow_unauthenticated,
            app_env,
        ));
        let client_certificate = match (self.client_cert_path, self.client_key_path) {
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
        };
        let client_certificate = v.take(client_certificate);
        let command_timeout = v.take(duration_range(
            "cache.command_timeout",
            self.command_timeout,
            Duration::from_millis(1),
            Duration::from_secs(10),
        ));
        let command_timeout = match (command_timeout, request_timeout) {
            (Ok(()), Some(limit)) if self.dsn.is_some() && self.command_timeout * 2 > limit => {
                Err(v.reject(ValidationError::new(
                    "cache.command_timeout",
                    format!(
                        "must be at most half of http.request_timeout ({}), got {}",
                        humantime::format_duration(limit / 2),
                        humantime::format_duration(self.command_timeout)
                    ),
                )))
            }
            (other, _) => other,
        };
        let client_certificate = client_certificate?;
        command_timeout?;
        Ok(CacheConfig {
            dsn: self.dsn,
            password_file: self.password_file,
            root_ca_path: self.root_ca_path,
            client_certificate,
            allow_plaintext: self.allow_plaintext,
            allow_unauthenticated: self.allow_unauthenticated,
            command_timeout: self.command_timeout,
        })
    }
}

#[derive(Clone, Debug)]
pub struct CacheClientCertificate {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

#[cfg(test)]
mod tests;
