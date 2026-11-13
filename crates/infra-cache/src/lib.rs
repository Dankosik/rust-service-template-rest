//! Optional Redis-compatible cache client.
//!
//! The contract is bytes in and bytes out. The calling feature owns key
//! shape, serialization, TTL policy, and invalidation. A miss returns `Ok(None)`;
//! an outage or timeout returns [`Unavailable`] (wrapped by [`SetError`] for SET).
//! SET rejects an invalid TTL through [`SetError::InvalidTtl`]. The caller chooses how to
//! degrade; this crate does not gate readiness.
//! Standalone TCP only — no Sentinel, Cluster, or Unix socket. The connection
//! always speaks RESP3 (`HELLO 3`), so the server must be Redis-compatible at
//! 6.0 or later.
//!
//! Each namespace stores keys as `{namespace}:{key}` so two features sharing
//! one server do not collide. The namespace name is also the bounded `cache`
//! metric label.
//!
//! In a service/composition adapter, a hit returns the bytes; a miss or outage takes
//! the source of truth, and the write back is best effort.
//!
//! ```no_run
//! # use std::time::Duration;
//! # use infra_cache::{CacheNamespace, SetError, Unavailable};
//! # async fn load_from_source_of_truth(_key: &str) -> Vec<u8> { Vec::new() }
//! # async fn user_profile(profiles: &CacheNamespace, key: &str) -> Result<Vec<u8>, SetError> {
//! // The adapter built this view once with `cache.namespace("user_profile")`.
//! // The feature owns the behavior and never depends on this provider crate.
//! match profiles.get(key).await {
//!     Ok(Some(bytes)) => return Ok(bytes),
//!     Ok(None) | Err(Unavailable) => {}
//! }
//! let bytes = load_from_source_of_truth(key).await;
//! match profiles.set(key, &bytes, Duration::from_mins(1)).await {
//!     Ok(()) | Err(SetError::Unavailable(_)) => {}
//!     Err(error @ SetError::InvalidTtl) => return Err(error),
//! }
//! Ok(bytes)
//! # }
//! ```

mod admission;
mod connection;
mod credentials;
mod namespace;
mod observe;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;

use self::admission::Admitted;
use self::connection::Link;
pub use self::observe::HISTOGRAMS;
use self::observe::NamespaceHistograms;

/// Bound of one whole connection setup attempt. A startup check that waits
/// for the first connection should allow at least this long.
pub const STARTUP_TIMEOUT: Duration = connection::SETUP_TIMEOUT;

/// Admission input. The DSN is secret; [`Debug`] redacts it.
#[derive(Debug)]
pub struct CacheOptions {
    /// `redis://`, `rediss://`, `valkey://`, or `valkeys://` URL. Supply
    /// the password here or through [`Self::password_file`]. If neither
    /// supplies a password, [`Self::allow_unauthenticated`] must be set.
    pub dsn: SecretString,
    /// A file that holds the password alone, for a platform that rotates it
    /// by rewriting the file; its trailing line breaks are not part of the
    /// password. A nonempty DSN password is incompatible with this file.
    pub password_file: Option<PathBuf>,
    /// PEM file of a private root CA. Absent uses the process trust store.
    pub root_ca_path: Option<PathBuf>,
    /// The certificate this client presents to a server that requires one
    /// (mutual TLS). Absent presents none.
    pub client_certificate: Option<ClientCertificate>,
    /// Permit a non-TLS address. Callers must already have applied the local-only policy.
    pub allow_plaintext: bool,
    /// Permit a DSN with no password and no password file. Callers must
    /// already have applied the local-only policy.
    pub allow_unauthenticated: bool,
    /// Bound for one command, including the wait for a reconnect.
    pub command_timeout: Duration,
}

/// The certificate and key a client presents on a TLS address. Both files are
/// read once, at admission.
#[derive(Clone, Debug)]
pub struct ClientCertificate {
    /// PEM file of the certificate chain, leaf first.
    pub cert_path: PathBuf,
    /// PEM file of the leaf's private key.
    pub key_path: PathBuf,
}

/// Local startup errors from [`Cache::start`]. Display never includes the DSN,
/// a password, or Redis error text.
///
/// The password-file variants also describe reads during connection setup and
/// refresh. Refresh counts failed reads and warns on the first failure in
/// each consecutive run of read failures.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CacheError {
    #[error("cache DSN is invalid")]
    InvalidDsn,
    /// Unix socket or any other non-TCP address class.
    #[error("cache address is unsupported")]
    UnsupportedAddress,
    /// `redis://` or `valkey://` without [`CacheOptions::allow_plaintext`].
    #[error("cache plaintext is refused")]
    PlaintextRefused,
    /// No password in the DSN, no password file, and unauthenticated access
    /// was not allowed.
    #[error("cache unauthenticated connection is refused")]
    UnauthenticatedRefused,
    /// A password in the DSN while [`CacheOptions::password_file`] is set.
    #[error("cache DSN must not carry a password when cache.password_file is set")]
    PasswordInDsn,
    /// The password file could not be read. `kind` is the I/O class, not the path.
    #[error("cache password file could not be read ({kind})")]
    PasswordFile {
        /// [`std::io::ErrorKind`] of the read, without the path or a message.
        kind: std::io::ErrorKind,
    },
    #[error("cache password file is empty")]
    PasswordFileEmpty,
    /// The URL selected certificate verification skip (`#insecure`).
    #[error("cache insecure TLS is refused")]
    InsecureTlsRefused,
    #[error("cache root CA requires TLS")]
    CaRequiresTls,
    /// The root CA path could not be read. `kind` is the I/O class, not the path.
    #[error("cache root CA file could not be read ({kind})")]
    CaFile {
        /// [`std::io::ErrorKind`] of the read, without the path or a message.
        kind: std::io::ErrorKind,
    },
    #[error("cache root CA is invalid")]
    InvalidCa,
    #[error("cache client certificate requires TLS")]
    ClientCertificateRequiresTls,
    /// The client certificate could not be read. `kind` is the I/O class, not the path.
    #[error("cache client certificate file could not be read ({kind})")]
    ClientCertificateFile {
        /// [`std::io::ErrorKind`] of the read, without the path or a message.
        kind: std::io::ErrorKind,
    },
    #[error("cache client certificate is invalid")]
    InvalidClientCertificate,
    /// The client key could not be read. `kind` is the I/O class, not the path.
    #[error("cache client key file could not be read ({kind})")]
    ClientKeyFile {
        /// [`std::io::ErrorKind`] of the read, without the path or a message.
        kind: std::io::ErrorKind,
    },
    /// The file held no usable PEM private key, or the key does not belong
    /// to the client certificate.
    #[error("cache client key is invalid or does not match the client certificate")]
    InvalidClientKey,
    #[error("cache requires a Tokio runtime")]
    NoRuntime,
    #[error("cache requires the Tokio time driver")]
    NoTimeDriver,
    #[error("cache requires the Tokio I/O driver")]
    NoIoDriver,
    #[error("cache could not check the Tokio I/O driver ({kind})")]
    IoDriverCheck { kind: std::io::ErrorKind },
    #[error("cache client could not be built")]
    ClientBuild,
}

/// A command that did not return a usable success, distinct from a cache miss
/// (`Ok(None)`).
///
/// This covers an outage, a timeout, caller cancellation, a local command slot
/// refusal, a retired connection generation, and a server error reply such as
/// `WRONGTYPE`, `OOM`, or `READONLY`. A command refused locally because every
/// command slot is taken returns the same error without contacting the
/// server; `cache_command_admission_refused_total` tells the two apart.
/// The caller chooses a fallback or rejects an operation that requires the cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("cache unavailable")]
pub struct Unavailable;

/// A SET argument error or an unavailable cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SetError {
    /// The floored TTL is outside Redis's positive signed 64-bit millisecond range.
    #[error("cache ttl must be between 1 and {} whole milliseconds", i64::MAX)]
    InvalidTtl,
    /// The command timed out, was stopped, or the server refused it.
    #[error(transparent)]
    Unavailable(#[from] Unavailable),
}

/// Host, port, and whether the address uses TLS. Safe to log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerEndpoint {
    pub host: String,
    pub port: u16,
    /// `true` when the address is TLS (`rediss://` or `valkeys://`).
    pub tls: bool,
}

/// A shared handle to one standalone Redis-compatible connection, dialed in
/// the background.
///
/// Clones, namespaces, and probes share the connection and keep it alive; the
/// last one dropped stops its supervisor.
///
/// [`Debug`] prints the server identity and the command timeout, never the DSN.
#[derive(Clone, Debug)]
pub struct Cache {
    link: Arc<Link>,
    command_timeout: Duration,
}

impl Cache {
    /// Validate the options, build the client, and spawn the connection supervisor.
    ///
    /// Must be called from a Tokio runtime with the time and I/O drivers enabled.
    /// Configured password and TLS files are read synchronously, so an async
    /// caller runs this on the blocking pool with the runtime entered.
    ///
    /// A returned handle may still be connecting. Commands and the [`CacheProbe`]
    /// report connection failures through their results.
    ///
    /// Install the intended metrics recorder first: the connection's counter
    /// and gauge handles bind to the recorder current here.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError`] when the DSN, address policy, password file, CA
    /// file, or client certificate is refused, when no Tokio runtime is
    /// current, when a required Tokio driver is disabled in an unwinding build,
    /// when checking the I/O driver fails, or when the client cannot be built.
    /// The message does not include the DSN.
    pub fn start(options: CacheOptions) -> Result<Self, CacheError> {
        let command_timeout = options.command_timeout;
        let Admitted {
            server,
            client,
            password_file,
        } = admission::admit(options)?;
        admission::check_runtime()?;
        observe::describe();
        Ok(Self {
            link: Arc::new(Link::start(server, client, password_file, command_timeout)),
            command_timeout,
        })
    }

    /// Readiness probe. Not registered by default: a cache outage is degradation.
    #[must_use]
    pub fn probe(&self) -> CacheProbe {
        CacheProbe {
            cache: self.clone(),
        }
    }

    /// Admitted server identity for logs.
    #[must_use]
    pub fn server(&self) -> &ServerEndpoint {
        &self.link.server
    }
}

/// `PING` probe. The name is `cache`. Failure text is `cache ping failed: <error.type>` only.
///
/// The caller's startup/readiness budget can cancel the probe. Cancelling
/// while PING is still pending before its reply deadline leaves the connection
/// available to other commands. A probe that reaches its own reply deadline
/// with PING still pending retires the connection.
#[derive(Clone, Debug)]
pub struct CacheProbe {
    cache: Cache,
}

#[async_trait::async_trait]
impl health::Probe for CacheProbe {
    fn name(&self) -> &'static str {
        "cache"
    }

    async fn check(&self) -> Result<(), health::ProbeError> {
        self.cache
            .link
            .probe(self.cache.command_timeout)
            .await
            .map_err(|error| {
                health::ProbeError::new(format!("cache ping failed: {}", error.label()))
            })
    }
}

/// One feature's keys on a shared connection.
///
/// Each operation/outcome histogram handle binds to the recorder current at
/// its first recorded result and remains shared by clones of this namespace.
#[derive(Clone, Debug)]
pub struct CacheNamespace {
    cache: Cache,
    name: &'static str,
    histograms: Arc<NamespaceHistograms>,
}
