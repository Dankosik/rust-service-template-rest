//! Standalone TCP only — no Sentinel, Cluster, or Unix socket. The connection
//! always speaks RESP3 (`HELLO 3`), so the server must be Redis-compatible at
//! 6.0 or later.

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

#[derive(Debug)]
pub struct CacheOptions {
    pub dsn: SecretString,
    pub password_file: Option<PathBuf>,
    pub root_ca_path: Option<PathBuf>,
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
    #[error("cache address is unsupported")]
    UnsupportedAddress,
    #[error("cache plaintext is refused")]
    PlaintextRefused,
    #[error("cache unauthenticated connection is refused")]
    UnauthenticatedRefused,
    #[error("cache DSN must not carry a password when cache.password_file is set")]
    PasswordInDsn,
    #[error("cache password file could not be read ({kind})")]
    PasswordFile { kind: std::io::ErrorKind },
    #[error("cache password file is empty")]
    PasswordFileEmpty,
    #[error("cache insecure TLS is refused")]
    InsecureTlsRefused,
    #[error("cache root CA requires TLS")]
    CaRequiresTls,
    #[error("cache root CA file could not be read ({kind})")]
    CaFile { kind: std::io::ErrorKind },
    #[error("cache root CA is invalid")]
    InvalidCa,
    #[error("cache client certificate requires TLS")]
    ClientCertificateRequiresTls,
    #[error("cache client certificate file could not be read ({kind})")]
    ClientCertificateFile { kind: std::io::ErrorKind },
    #[error("cache client certificate is invalid")]
    InvalidClientCertificate,
    #[error("cache client key file could not be read ({kind})")]
    ClientKeyFile { kind: std::io::ErrorKind },
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SetError {
    #[error("cache ttl must be between 1 and {} whole milliseconds", i64::MAX)]
    InvalidTtl,
    #[error(transparent)]
    Unavailable(#[from] Unavailable),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerEndpoint {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

/// Clones, namespaces, and probes share the connection and keep it alive; the
/// last one dropped stops its supervisor.
#[derive(Clone, Debug)]
pub struct Cache {
    link: Arc<Link>,
}

impl Cache {
    /// Must be called from a Tokio runtime with the time and I/O drivers enabled.
    /// Configured password and TLS files are read synchronously, so an async
    /// caller runs this on the blocking pool with the runtime entered.
    ///
    /// A returned handle may still be connecting. Commands and the [`CacheProbe`]
    /// report connection failures through their results.
    ///
    /// Install the intended metrics recorder first: the connection's counter
    /// and gauge handles bind to the recorder current here.
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
        })
    }

    /// Readiness probe. Not registered by default: a cache outage is degradation.
    #[must_use]
    pub fn probe(&self) -> CacheProbe {
        CacheProbe {
            cache: self.clone(),
        }
    }

    #[must_use]
    pub fn server(&self) -> &ServerEndpoint {
        &self.link.server
    }
}

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
        self.cache.link.probe().await.map_err(|error| {
            health::ProbeError::new(format!("cache ping failed: {}", error.label()))
        })
    }
}

/// Each operation/outcome histogram handle binds to the recorder current at
/// its first recorded result and remains shared by clones of this namespace.
#[derive(Clone, Debug)]
pub struct CacheNamespace {
    cache: Cache,
    name: &'static str,
    histograms: Arc<NamespaceHistograms>,
}
