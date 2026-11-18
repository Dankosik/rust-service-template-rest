use std::path::PathBuf;
use std::time::Duration;

use bytesize::ByteSize;
use secrecy::SecretString;
use serde::Deserialize;

use crate::app::local_development_only;
use crate::de::{blank_as_none, blank_secret_as_none};
use crate::validate::{Rejected, Violations, duration_range, int_range, missing};

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
    /// Permit a plaintext DSN with `app.allow_insecure_development`.
    #[serde(deserialize_with = "crate::de::boolean")]
    allow_plaintext: bool,
    /// Permit a DSN with no password with `app.allow_insecure_development`.
    #[serde(deserialize_with = "crate::de::boolean")]
    allow_unauthenticated: bool,
    /// Bound for one cache command, including a reconnect wait.
    #[serde(deserialize_with = "crate::de::duration")]
    command_timeout: Duration,
    /// Maximum simultaneous application commands per cache client.
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    max_concurrency: usize,
    /// Maximum wire key including namespace and separator.
    max_key_bytes: ByteSize,
    /// Largest value the service will write or return from a lookup.
    max_value_bytes: ByteSize,
}

impl Default for CacheInput {
    fn default() -> Self {
        Self {
            dsn: None,
            password_file: None,
            root_ca_path: None,
            client_cert_path: None,
            client_key_path: None,
            allow_plaintext: false,
            allow_unauthenticated: false,
            command_timeout: Duration::from_secs(2),
            max_concurrency: 256,
            max_key_bytes: ByteSize::kib(1),
            max_value_bytes: ByteSize::kib(64),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CacheConfig {
    pub dsn: SecretString,
    pub password_file: Option<PathBuf>,
    pub root_ca_path: Option<PathBuf>,
    pub client_certificate: Option<CacheClientCertificate>,
    pub allow_plaintext: bool,
    pub allow_unauthenticated: bool,
    pub command_timeout: Duration,
    pub max_concurrency: usize,
    pub max_key_bytes: ByteSize,
    pub max_value_bytes: ByteSize,
}

impl CacheInput {
    pub(crate) fn admit(
        self,
        insecure_development: bool,
        v: &mut Violations,
    ) -> Result<Option<CacheConfig>, Rejected> {
        v.check(local_development_only(
            "cache.allow_plaintext",
            self.allow_plaintext,
            insecure_development,
        ));
        v.check(local_development_only(
            "cache.allow_unauthenticated",
            self.allow_unauthenticated,
            insecure_development,
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
            Duration::from_millis(1)..=Duration::from_secs(10),
        ));
        v.check(int_range(
            "cache.max_concurrency",
            self.max_concurrency,
            1..=usize::MAX,
        ));
        for (key, size) in [
            ("cache.max_key_bytes", self.max_key_bytes),
            ("cache.max_value_bytes", self.max_value_bytes),
        ] {
            v.check(int_range(
                key,
                size.as_u64(),
                1..=shared_rules::cache::MAX_ENTRY_BYTES as u64,
            ));
        }
        let client_certificate = client_certificate?;
        command_timeout?;
        Ok(self.dsn.map(|dsn| CacheConfig {
            dsn,
            password_file: self.password_file,
            root_ca_path: self.root_ca_path,
            client_certificate,
            allow_plaintext: self.allow_plaintext,
            allow_unauthenticated: self.allow_unauthenticated,
            command_timeout: self.command_timeout,
            max_concurrency: self.max_concurrency,
            max_key_bytes: self.max_key_bytes,
            max_value_bytes: self.max_value_bytes,
        }))
    }
}

#[derive(Clone, Debug)]
pub struct CacheClientCertificate {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

#[cfg(test)]
mod tests;
