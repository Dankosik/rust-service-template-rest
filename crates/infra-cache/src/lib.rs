mod admission;
mod connection;
mod credentials;
mod dial;
mod error;
mod namespace;
mod observe;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;

use self::connection::Link;
pub use self::error::{MutationError, OperationError, SetError};
pub use self::namespace::{CacheLookup, CacheNamespace, Namespace};
pub use self::observe::HISTOGRAMS;

#[derive(Debug)]
pub struct CacheOptions {
    pub dsn: SecretString,
    pub password_file: Option<PathBuf>,
    pub root_ca_path: Option<PathBuf>,
    pub client_certificate: Option<ClientCertificate>,
    pub allow_plaintext: bool,
    pub allow_unauthenticated: bool,
    pub command_timeout: Duration,
    pub max_concurrency: usize,
    pub max_key_bytes: usize,
    pub max_value_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct ClientCertificate {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CacheError {
    #[error("cache.max_concurrency must allow application commands and leave one maintenance slot")]
    InvalidConcurrency,
    #[error("cache key or value size limit is outside the supported range")]
    InvalidSizeLimit,
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
    #[error("cache client could not be built")]
    ClientBuild,
    #[error("cache startup admission timed out")]
    AdmissionTimedOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseOutcome {
    Complete,
    TimedOut,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServerEndpoint {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) tls: bool,
    pub(crate) database_index: i64,
}

#[derive(Clone, Debug)]
pub struct Cache {
    link: Arc<Link>,
    max_key_bytes: usize,
    max_value_bytes: usize,
}

impl Cache {
    pub async fn start(options: CacheOptions) -> Result<Self, CacheError> {
        use tokio_util::task::AbortOnDropHandle;

        const ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);

        let deadline = tokio::time::Instant::now() + ADMISSION_TIMEOUT;
        let command_timeout = options.command_timeout;
        let max_concurrency = options.max_concurrency;
        if !(1..tokio::sync::Semaphore::MAX_PERMITS).contains(&max_concurrency) {
            return Err(CacheError::InvalidConcurrency);
        }
        let max_key_bytes = options.max_key_bytes;
        let max_value_bytes = options.max_value_bytes;
        let supported = 1..=shared_rules::cache::MAX_ENTRY_BYTES;
        if !supported.contains(&max_key_bytes) || !supported.contains(&max_value_bytes) {
            return Err(CacheError::InvalidSizeLimit);
        }
        let admission = AbortOnDropHandle::new(tokio::task::spawn_blocking(move || {
            admission::admit(options)
        }));
        let admitted = tokio::time::timeout_at(deadline, admission)
            .await
            .map_err(|_| CacheError::AdmissionTimedOut)?
            .map_err(|_| CacheError::ClientBuild)??;
        observe::describe();
        Ok(Self {
            max_key_bytes,
            max_value_bytes,
            link: Arc::new(Link::start(admitted, command_timeout, max_concurrency)),
        })
    }

    pub async fn close(&self, deadline: tokio::time::Instant) -> CloseOutcome {
        self.link.close(deadline).await
    }
}
