mod admission;
mod connection;
mod credentials;
mod dial;
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

pub const STARTUP_TIMEOUT: Duration = connection::SETUP_TIMEOUT;

#[derive(Debug)]
pub struct CacheOptions {
    pub dsn: SecretString,
    pub password_file: Option<PathBuf>,
    pub root_ca_path: Option<PathBuf>,
    pub client_certificate: Option<ClientCertificate>,
    pub allow_plaintext: bool,
    pub allow_unauthenticated: bool,
    pub command_timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct ClientCertificate {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

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
    #[error("cache client could not be built")]
    ClientBuild,
}

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

#[derive(Clone, Debug)]
pub struct Cache {
    link: Arc<Link>,
}

impl Cache {
    pub fn start(options: CacheOptions) -> Result<Self, CacheError> {
        let command_timeout = options.command_timeout;
        let Admitted {
            server,
            dial,
            settings,
            password_file,
        } = admission::admit(options)?;
        admission::check_runtime()?;
        observe::describe();
        Ok(Self {
            link: Arc::new(Link::start(
                server,
                dial,
                settings,
                password_file,
                command_timeout,
            )),
        })
    }

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
            health::ProbeError::format(format_args!("cache ping failed: {}", error.label()))
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Namespace(&'static str);

impl Namespace {
    #[must_use]
    pub const fn new(name: &'static str) -> Option<Self> {
        if namespace::valid_namespace(name) {
            Some(Self(name))
        } else {
            None
        }
    }
}

/// ```
/// const SESSIONS: infra_cache::Namespace = infra_cache::namespace!("sessions");
/// ```
/// ```compile_fail,E0080
/// let _ = infra_cache::namespace!("Sessions");
/// ```
#[macro_export]
macro_rules! namespace {
    ($name:literal) => {
        const {
            $crate::Namespace::new($name)
                .expect("cache namespace must match ^[a-z][a-z0-9_]{0,63}$")
        }
    };
}

#[derive(Clone, Debug)]
pub struct CacheNamespace {
    cache: Cache,
    name: &'static str,
    histograms: Arc<NamespaceHistograms>,
}
