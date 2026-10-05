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
//! # use infra_cache::{Cache, SetError, Unavailable};
//! # async fn load_from_source_of_truth(_key: &str) -> Vec<u8> { Vec::new() }
//! # async fn user_profile(cache: &Cache, key: &str) -> Result<Vec<u8>, SetError> {
//! // Build the namespace once and keep it in the adapter's state.
//! // The feature owns the behavior and never depends on this provider crate.
//! let profiles = cache.namespace("user_profile");
//! match profiles.get(key).await {
//!     Ok(Some(bytes)) => return Ok(bytes),
//!     Ok(None) | Err(Unavailable) => {}
//! }
//! let bytes = load_from_source_of_truth(key).await;
//! match profiles.set(key, &bytes, Duration::from_secs(60)).await {
//!     Ok(()) | Err(SetError::Unavailable(_)) => {}
//!     Err(error @ SetError::InvalidTtl) => return Err(error),
//! }
//! Ok(bytes)
//! # }
//! ```

mod connection;
mod credentials;
mod observe;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use redis::{IntoConnectionInfo, SetExpiry, SetOptions};
use secrecy::{ExposeSecret, SecretString};

use self::connection::Link;
use self::credentials::PasswordFile;
use self::observe::{Histograms, Operation, OperationGuard, Outcome};
pub use self::observe::{OPERATION_DURATION_BUCKETS, OPERATION_DURATION_METRIC};

/// Detect a dead peer before the platform's connection idle timeout.
const KEEPALIVE_TIME: Duration = Duration::from_secs(30);
/// Space between keepalive probes once the idle time has elapsed.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// Give up on a silent peer after a few probes, where the platform allows it.
const KEEPALIVE_RETRIES: u32 = 3;
/// Linux `TCP_USER_TIMEOUT`: a half-open connection is failed and reconnected.
#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
const USER_TIMEOUT: Duration = Duration::from_secs(10);

/// Admission input. The DSN is secret; [`Debug`] redacts it.
#[derive(Debug)]
pub struct CacheOptions {
    /// `redis://`, `rediss://`, `valkey://`, or `valkeys://` URL, password
    /// included unless [`Self::password_file`] is set.
    pub dsn: SecretString,
    /// A file that holds the password alone, for a platform that rotates it
    /// by rewriting the file. The DSN then carries no password.
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

/// Why admission refused to build a client. Display never includes the DSN,
/// a password, or Redis error text.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CacheError {
    /// The DSN could not be parsed as a Redis URL.
    #[error("cache DSN is invalid")]
    InvalidDsn,
    /// Unix socket, Sentinel, Cluster, or any address that is not standalone TCP.
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
    /// The password file is empty.
    #[error("cache password file is empty")]
    PasswordFileEmpty,
    /// The URL selected certificate verification skip (`#insecure`).
    #[error("cache insecure TLS is refused")]
    InsecureTlsRefused,
    /// A root CA was set on a plaintext address.
    #[error("cache root CA requires TLS")]
    CaRequiresTls,
    /// The root CA path could not be read. `kind` is the I/O class, not the path.
    #[error("cache root CA file could not be read ({kind})")]
    CaFile {
        /// [`std::io::ErrorKind`] of the read, without the path or a message.
        kind: std::io::ErrorKind,
    },
    /// The file did not contain a PEM certificate.
    #[error("cache root CA is invalid")]
    InvalidCa,
    /// A client certificate was set on a plaintext address.
    #[error("cache client certificate requires TLS")]
    ClientCertificateRequiresTls,
    /// The client certificate could not be read. `kind` is the I/O class, not the path.
    #[error("cache client certificate file could not be read ({kind})")]
    ClientCertificateFile {
        /// [`std::io::ErrorKind`] of the read, without the path or a message.
        kind: std::io::ErrorKind,
    },
    /// The file did not contain a PEM certificate.
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
    /// [`Cache::connect_lazy`] was called outside a Tokio runtime.
    #[error("cache requires a Tokio runtime")]
    NoRuntime,
    /// The client or the lazy connection manager could not be built.
    #[error("cache client could not be built")]
    Client,
}

/// An outage or timeout, distinct from a cache miss (`Ok(None)`).
/// The caller chooses a fallback or rejects an operation that requires the cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("cache unavailable")]
pub struct Unavailable;

/// A SET argument error or an unavailable cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SetError {
    /// The floored TTL is outside Redis's positive signed 64-bit millisecond range.
    #[error("cache ttl must be between 1 and 9223372036854775807 whole milliseconds")]
    InvalidTtl,
    /// The command timed out or the server could not be used.
    #[error(transparent)]
    Unavailable(#[from] Unavailable),
}

/// Host, port, and whether the address uses TLS. Safe to log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerIdentity {
    /// Hostname or IP from the admitted address.
    pub host: String,
    /// TCP port from the admitted address.
    pub port: u16,
    /// `true` when the address is TLS (`rediss://` or `valkeys://`).
    pub tls: bool,
}

/// A standalone Redis-compatible connection, dialed in the background.
///
/// [`Debug`] prints the server identity and the command timeout, never the DSN.
#[derive(Clone, Debug)]
pub struct Cache {
    link: Arc<Link>,
    command_timeout: Duration,
}

impl Cache {
    /// Admit the DSN and build the connection without waiting for the network.
    ///
    /// Must be called from a Tokio runtime. The connection is dialed in the
    /// background from here on, so a server that is down now does not leave
    /// the first calls to drive the dial; an outage is reported by the calls
    /// and the probe, never by this function.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError`] when the DSN, address policy, password file, CA
    /// file, or client certificate is refused, when no Tokio runtime is
    /// current, or when the client cannot be built. The message does not
    /// include the DSN.
    pub fn connect_lazy(options: CacheOptions) -> Result<Self, CacheError> {
        let CacheOptions {
            dsn,
            password_file,
            root_ca_path,
            client_certificate,
            allow_plaintext,
            allow_unauthenticated,
            command_timeout,
        } = options;
        let info = dsn
            .expose_secret()
            .into_connection_info()
            .map_err(|_| CacheError::InvalidDsn)?;
        let server = admit_address(
            &info,
            allow_plaintext,
            root_ca_path.is_some(),
            client_certificate.is_some(),
        )?;
        let has_password = info
            .redis_settings()
            .password()
            .is_some_and(|password| !password.is_empty());
        let password_file = match password_file {
            Some(_) if has_password => return Err(CacheError::PasswordInDsn),
            Some(path) => Some(PasswordFile::admit(path, info.redis_settings().username())?),
            None if has_password || allow_unauthenticated => None,
            None => return Err(CacheError::UnauthenticatedRefused),
        };
        let root_cert = root_ca_path.as_deref().map(read_root_ca).transpose()?;
        let client_tls = client_certificate
            .as_ref()
            .map(read_client_certificate)
            .transpose()?;
        // RESP3 supplies an idle disconnection notification to the supervisor.
        let settings = info
            .redis_settings()
            .clone()
            .set_protocol(redis::ProtocolVersion::RESP3);
        let info = info
            .set_redis_settings(settings)
            .set_tcp_settings(tcp_settings());
        if server.tls {
            install_tls_provider();
        }
        let client = if root_cert.is_some() || client_tls.is_some() {
            redis::Client::build_with_tls(
                info,
                redis::TlsCertificates {
                    client_tls,
                    root_cert,
                },
            )
        } else {
            redis::Client::open(info)
        }
        .map_err(|_| CacheError::Client)?;
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(CacheError::NoRuntime);
        }
        observe::describe();
        Ok(Self {
            link: Arc::new(Link::start(server, client, password_file, command_timeout)),
            command_timeout,
        })
    }

    /// A named view of this connection.
    ///
    /// Keys are stored as `{name}:{key}`. `name` is the `cache` metric label.
    /// Reuse or clone this view to retain its lazily registered histogram handles.
    ///
    /// # Panics
    ///
    /// Panics if `name` does not match `^[a-z][a-z0-9_]{0,63}$`. A namespace
    /// name is chosen by the feature author, not by a caller.
    #[must_use]
    pub fn namespace(&self, name: &'static str) -> CacheNamespace {
        assert!(
            valid_namespace(name),
            "cache namespace {name:?} must match ^[a-z][a-z0-9_]{{0,63}}$"
        );
        CacheNamespace {
            cache: self.clone(),
            name,
            histograms: Arc::default(),
        }
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
    pub fn server(&self) -> ServerIdentity {
        self.link.server.clone()
    }
}

/// One feature's keys on a shared connection.
///
/// Install the intended metrics recorder before using namespace operations.
/// Each operation/outcome histogram handle binds to the recorder on its first
/// recorded result and remains shared by clones of this namespace.
#[derive(Clone, Debug)]
pub struct CacheNamespace {
    cache: Cache,
    name: &'static str,
    histograms: Arc<Histograms>,
}

impl CacheNamespace {
    /// `GET`. `Ok(None)` is a miss. [`Unavailable`] means the caller should degrade.
    ///
    /// # Errors
    ///
    /// Returns [`Unavailable`] when the command times out or the server cannot be used.
    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Unavailable> {
        // Cmd reserves argument slots and payload bytes, excluding RESP framing.
        // Reserving both avoids buffer growth while writing GET and the prefixed key.
        let mut command = redis::Cmd::with_capacity(2, 3 + self.name.len() + 1 + key.len());
        command.arg("GET").arg(self.stored_key(key));
        self.run(Operation::Get, command, |value: &Option<Vec<u8>>| {
            if value.is_some() {
                Outcome::Hit
            } else {
                Outcome::Miss
            }
        })
        .await
    }

    /// `SET key value PX milliseconds`.
    ///
    /// The TTL is floored to whole milliseconds, which must be in `1..=i64::MAX`.
    /// Redis owns absolute-expiry arithmetic and can still refuse an admitted TTL.
    ///
    /// # Errors
    ///
    /// Returns [`SetError::InvalidTtl`] before dispatch or observation for an
    /// out-of-range TTL. Returns [`SetError::Unavailable`] when the command times
    /// out or the server cannot be used, including an absolute-expiry refusal.
    /// A timed-out `SET` is not retried: the write may have landed, and the TTL
    /// bounds staleness.
    pub async fn set(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), SetError> {
        let milliseconds = ttl.as_millis();
        if !(1..=i64::MAX as u128).contains(&milliseconds) {
            return Err(SetError::InvalidTtl);
        }
        let milliseconds = u64::try_from(milliseconds).map_err(|_| SetError::InvalidTtl)?;
        let options = SetOptions::default().with_expiration(SetExpiry::PX(milliseconds));
        // Five arguments: SET, prefixed key, value, PX, and expiry. Reserve their
        // payload bytes; the u64 expiry fits in at most 20 decimal digits.
        let mut command = redis::Cmd::with_capacity(
            5,
            3 + self.name.len() + 1 + key.len() + value.len() + 2 + 20,
        );
        command
            .arg("SET")
            .arg(self.stored_key(key))
            .arg(value)
            .arg(options);
        self.run(Operation::Set, command, |(): &()| Outcome::Ok)
            .await
            .map_err(SetError::Unavailable)
    }

    /// `DEL`. The deleted-count is ignored; a missing key is still success.
    ///
    /// # Errors
    ///
    /// Returns [`Unavailable`] when the command times out or the server cannot be used.
    pub async fn delete(&self, key: &str) -> Result<(), Unavailable> {
        let mut command = redis::Cmd::with_capacity(2, 3 + self.name.len() + 1 + key.len());
        command.arg("DEL").arg(self.stored_key(key));
        self.run(Operation::Delete, command, |(): &()| Outcome::Ok)
            .await
    }

    /// One observed command under `command_timeout`, which also bounds the
    /// wait for a (re)connect. `outcome` names a successful reply.
    async fn run<T: redis::FromRedisValue>(
        &self,
        operation: Operation,
        command: redis::Cmd,
        outcome: fn(&T) -> Outcome,
    ) -> Result<T, Unavailable> {
        let mut guard = OperationGuard::start(
            self.name,
            &self.histograms,
            operation,
            &self.cache.link.server,
        );
        // One deadline includes connection wait and exactly one dispatch.
        let deadline = tokio::time::Instant::now() + self.cache.command_timeout;
        let reply = self.cache.link.command(&command, deadline).await;
        match reply {
            Ok(value) => {
                guard.succeed(outcome(&value));
                Ok(value)
            }
            Err(error) => Err(guard.fail(error)),
        }
    }

    fn stored_key<'a>(&self, key: &'a str) -> NamespacedKey<'a> {
        NamespacedKey {
            namespace: self.name,
            key,
        }
    }
}

/// `PING` probe. The name is `cache`. Failure text is `cache ping failed: <error.type>` only.
///
/// Acquisition uses the caller's startup/readiness budget. Once connected,
/// PING has a one-second ceiling and ends if its generation retires.
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

fn admit_address(
    info: &redis::ConnectionInfo,
    allow_plaintext: bool,
    has_root_ca: bool,
    has_client_certificate: bool,
) -> Result<ServerIdentity, CacheError> {
    match info.addr() {
        redis::ConnectionAddr::Tcp(host, port) => {
            if !allow_plaintext {
                return Err(CacheError::PlaintextRefused);
            }
            if has_root_ca {
                return Err(CacheError::CaRequiresTls);
            }
            if has_client_certificate {
                return Err(CacheError::ClientCertificateRequiresTls);
            }
            Ok(ServerIdentity {
                host: host.clone(),
                port: *port,
                tls: false,
            })
        }
        redis::ConnectionAddr::TcpTls { insecure: true, .. } => Err(CacheError::InsecureTlsRefused),
        redis::ConnectionAddr::TcpTls {
            host,
            port,
            insecure: false,
            ..
        } => Ok(ServerIdentity {
            host: host.clone(),
            port: *port,
            tls: true,
        }),
        // Unix sockets and any future address class are outside standalone TCP.
        _ => Err(CacheError::UnsupportedAddress),
    }
}

/// Reads a PEM root CA. [`admit_address`] has already refused it on plaintext.
fn read_root_ca(path: &Path) -> Result<Vec<u8>, CacheError> {
    use rustls::pki_types::pem::PemObject;

    let bytes = std::fs::read(path).map_err(|error| CacheError::CaFile { kind: error.kind() })?;
    let mut found = false;
    for certificate in rustls::pki_types::CertificateDer::pem_slice_iter(&bytes) {
        certificate.map_err(|_| CacheError::InvalidCa)?;
        found = true;
    }
    if !found {
        return Err(CacheError::InvalidCa);
    }
    Ok(bytes)
}

/// Reads the PEM certificate chain and key. [`admit_address`] has already
/// refused them on plaintext.
///
/// redis-rs hands the pair to rustls only when it dials, where a key that
/// does not belong to the certificate is a configuration error on every
/// connection attempt. Building the signing key here makes it a startup error.
fn read_client_certificate(
    certificate: &ClientCertificate,
) -> Result<redis::ClientTlsConfig, CacheError> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let client_cert = std::fs::read(&certificate.cert_path)
        .map_err(|error| CacheError::ClientCertificateFile { kind: error.kind() })?;
    let client_key = std::fs::read(&certificate.key_path)
        .map_err(|error| CacheError::ClientKeyFile { kind: error.kind() })?;
    let chain = CertificateDer::pem_slice_iter(&client_cert)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| CacheError::InvalidClientCertificate)?;
    if chain.is_empty() {
        return Err(CacheError::InvalidClientCertificate);
    }
    let key =
        PrivateKeyDer::from_pem_slice(&client_key).map_err(|_| CacheError::InvalidClientKey)?;
    rustls::sign::CertifiedKey::from_der(
        chain,
        key,
        &rustls::crypto::aws_lc_rs::default_provider(),
    )
    .map_err(|_| CacheError::InvalidClientKey)?;
    Ok(redis::ClientTlsConfig {
        client_cert,
        client_key,
    })
}

fn tcp_settings() -> redis::io::tcp::TcpSettings {
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(KEEPALIVE_TIME)
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_RETRIES);
    let settings = redis::io::tcp::TcpSettings::default()
        .set_nodelay(true)
        .set_keepalive(keepalive);
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    let settings = settings.set_user_timeout(USER_TIMEOUT);
    settings
}

/// redis-rs builds its TLS config from the process default provider. The
/// workspace also compiles `ring`, so rustls cannot pick one on its own.
fn install_tls_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn valid_namespace(name: &str) -> bool {
    let mut bytes = name.bytes();
    if !matches!(bytes.next(), Some(b'a'..=b'z')) {
        return false;
    }
    let rest = bytes.len();
    rest <= 63
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Write the prefixed key as one Redis argument without a temporary String.
/// Cmd's `write_arg_fmt` writes directly into its preallocated payload buffer.
struct NamespacedKey<'a> {
    namespace: &'static str,
    key: &'a str,
}

impl redis::ToRedisArgs for NamespacedKey<'_> {
    fn write_redis_args<W: redis::RedisWrite + ?Sized>(&self, out: &mut W) {
        out.write_arg_fmt(format_args!("{}:{}", self.namespace, self.key));
    }
}
impl redis::ToSingleRedisArg for NamespacedKey<'_> {}
