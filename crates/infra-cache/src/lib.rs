mod admission;
mod connection;
mod credentials;
mod dial;
mod error;
mod exchange;
mod namespace;
mod observe;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;

use self::connection::Link;
pub use self::error::{MutationError, MutationFailure, OperationError, StartError, UnknownOutcome};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseOutcome {
    Complete,
    TimedOut,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServerEndpoint {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) database_index: i64,
}

#[derive(Clone, Debug)]
pub struct Cache {
    link: Arc<Link>,
    max_key_bytes: usize,
    max_value_bytes: usize,
}

impl Cache {
    pub async fn start(options: CacheOptions) -> Result<Self, StartError> {
        use tokio_util::task::AbortOnDropHandle;

        const ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);

        let command_timeout = options.command_timeout;
        let max_concurrency = options.max_concurrency;
        if !(1..tokio::sync::Semaphore::MAX_PERMITS).contains(&max_concurrency) {
            return Err(StartError::InvalidConcurrency);
        }
        let max_key_bytes = options.max_key_bytes;
        let max_value_bytes = options.max_value_bytes;
        let supported = 1..=shared_rules::cache::MAX_ENTRY_BYTES;
        if !supported.contains(&max_key_bytes) || !supported.contains(&max_value_bytes) {
            return Err(StartError::InvalidSizeLimit);
        }
        let admission = AbortOnDropHandle::new(tokio::task::spawn_blocking(move || {
            admission::admit(options)
        }));
        let admitted = tokio::time::timeout(ADMISSION_TIMEOUT, admission)
            .await
            .map_err(|_| StartError::AdmissionTimedOut)?
            .map_err(|_| StartError::ClientBuild)??;
        observe::describe();
        Ok(Self {
            max_key_bytes,
            max_value_bytes,
            link: Arc::new(Link::start(admitted, command_timeout, max_concurrency)),
        })
    }

    pub async fn close(&self, budget: Duration) -> CloseOutcome {
        self.link.close(budget).await
    }
}
