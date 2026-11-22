use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use bytesize::ByteSize;
use secrecy::SecretString;
use serde::Deserialize;

use crate::app::local_development_only;
use crate::de::{BlankAsNone, BlankAsNoneVerbatim, StrictBool, StrictCount, StrictDuration};
use crate::validate::{Rejected, Violations, duration_range, nonzero_usize_range, optional_pair};

#[serde_with::serde_as]
#[derive(Clone, Debug, Deserialize, educe::Educe)]
#[educe(Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct CacheInput {
    /// `redis://`, `rediss://`, `valkey://`, or `valkeys://`, password included
    /// unless `password_file` is set.
    /// Set `APP__CACHE__DSN` in the environment or via `--secrets-dir`.
    /// Missing, empty, or whitespace-only is absent (`None`).
    #[serde_as(as = "BlankAsNoneVerbatim")]
    dsn: Option<SecretString>,
    /// A file that holds the password alone, for a platform that rotates it
    /// by rewriting the file; its trailing line breaks are not part of the
    /// password. The DSN then carries no password, and the running service
    /// follows the file. Missing, empty, or whitespace-only is unset (`None`),
    /// the default.
    #[serde_as(as = "BlankAsNone")]
    password_file: Option<PathBuf>,
    /// PEM root CA path for a private certificate. Missing, empty, or
    /// whitespace-only is unset (`None`).
    #[serde_as(as = "BlankAsNone")]
    root_ca_path: Option<PathBuf>,
    /// PEM certificate chain this client presents to a server that requires
    /// one (mutual TLS). Set together with `client_key_path`. Missing, empty,
    /// or whitespace-only is unset (`None`).
    #[serde_as(as = "BlankAsNone")]
    client_cert_path: Option<PathBuf>,
    /// PEM private key of that certificate. The key is a path, so a file may
    /// set it; the key itself stays in the file it names.
    #[serde_as(as = "BlankAsNone")]
    client_key_path: Option<PathBuf>,
    /// Permit a plaintext DSN with `app.allow_insecure_development`.
    #[serde_as(as = "StrictBool")]
    allow_plaintext: bool,
    /// Permit a DSN with no password with `app.allow_insecure_development`.
    #[serde_as(as = "StrictBool")]
    allow_unauthenticated: bool,
    /// Bound for one cache command, including a reconnect wait.
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(2)))]
    command_timeout: Duration,
    /// Maximum simultaneous application commands per cache client.
    #[serde_as(as = "StrictCount")]
    #[educe(Default = 256)]
    max_concurrency: usize,
    /// Maximum wire key including namespace and separator.
    #[educe(Default(expression = ByteSize::kib(1)))]
    max_key_bytes: ByteSize,
    /// Largest value the service will write or return from a lookup.
    #[educe(Default(expression = ByteSize::kib(64)))]
    max_value_bytes: ByteSize,
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
    pub max_concurrency: NonZeroUsize,
    pub max_key_bytes: NonZeroUsize,
    pub max_value_bytes: NonZeroUsize,
}

impl CacheInput {
    pub(crate) fn admit(
        self,
        local_development: bool,
        v: &mut Violations,
    ) -> Result<Option<CacheConfig>, Rejected> {
        v.check(local_development_only(
            "cache.allow_plaintext",
            self.allow_plaintext,
            local_development,
        ));
        v.check(local_development_only(
            "cache.allow_unauthenticated",
            self.allow_unauthenticated,
            local_development,
        ));
        let client_certificate = v.record(optional_pair(
            "cache.client_cert_path",
            self.client_cert_path,
            "cache.client_key_path",
            self.client_key_path,
        ));
        v.check(duration_range(
            "cache.command_timeout",
            self.command_timeout,
            Duration::from_millis(1)..=Duration::from_secs(10),
        ));
        let max_concurrency = v.record(nonzero_usize_range(
            "cache.max_concurrency",
            self.max_concurrency,
            1..=usize::MAX,
        ));
        let max_key_bytes = v.record(nonzero_usize_range(
            "cache.max_key_bytes",
            self.max_key_bytes.as_u64(),
            1..=shared_rules::cache::MAX_ENTRY_BYTES,
        ));
        let max_value_bytes = v.record(nonzero_usize_range(
            "cache.max_value_bytes",
            self.max_value_bytes.as_u64(),
            1..=shared_rules::cache::MAX_ENTRY_BYTES,
        ));
        let client_certificate =
            client_certificate?.map(|(cert_path, key_path)| CacheClientCertificate {
                cert_path,
                key_path,
            });
        let max_concurrency = max_concurrency?;
        let max_key_bytes = max_key_bytes?;
        let max_value_bytes = max_value_bytes?;
        Ok(self.dsn.map(|dsn| CacheConfig {
            dsn,
            password_file: self.password_file,
            root_ca_path: self.root_ca_path,
            client_certificate,
            allow_plaintext: self.allow_plaintext,
            allow_unauthenticated: self.allow_unauthenticated,
            command_timeout: self.command_timeout,
            max_concurrency,
            max_key_bytes,
            max_value_bytes,
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
