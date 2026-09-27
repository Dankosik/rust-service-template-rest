//! Optional Redis-compatible cache client.
//!
//! The contract is bytes in and bytes out. The calling feature owns key
//! shape, serialization, TTL policy, and invalidation. A miss and an outage
//! are both [`Unavailable`] or `Ok(None)`: this crate does not gate readiness.
//! Standalone TCP only — no Sentinel, Cluster, or Unix socket.
//!
//! Each namespace stores keys as `{namespace}:{key}` so two features sharing
//! one server do not collide. The namespace name is also the bounded `cache`
//! metric label.

mod observe;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

use redis::IntoConnectionInfo;
use redis::aio::ConnectionManager;
use secrecy::{ExposeSecret, SecretString};
use tracing::Instrument;

use self::observe::{OperationGuard, classify};

/// One reconnect attempt stays inside the startup check.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// Floor of the client's exponential reconnect backoff.
const MIN_DELAY: Duration = Duration::from_millis(100);
/// Cap so a long outage does not park one reconnect chain for minutes.
const MAX_DELAY: Duration = Duration::from_secs(2);
/// Factor of the client's exponential reconnect schedule.
const EXPONENT_BASE: f32 = 2.0;
/// Bound one reconnect chain; a later command may start another.
const NUMBER_OF_RETRIES: usize = 6;
/// Detect a dead peer before the platform's connection idle timeout.
const KEEPALIVE_TIME: Duration = Duration::from_secs(30);
/// Space between keepalive probes once the idle time has elapsed.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// Give up on a silent peer after a few probes, where the platform allows it.
const KEEPALIVE_RETRIES: u32 = 3;
/// Linux `TCP_USER_TIMEOUT`: a half-open connection is failed and reconnected.
#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
const USER_TIMEOUT: Duration = Duration::from_secs(10);

const PEM_CERTIFICATE_MARKER: &str = "-----BEGIN CERTIFICATE-----";

/// Admission input. The DSN is secret; [`Debug`] redacts it.
#[derive(Debug)]
pub struct CacheOptions {
    /// `redis://`, `rediss://`, `valkey://`, or `valkeys://` URL, password included.
    pub dsn: SecretString,
    /// PEM file of a private root CA. Absent uses the process trust store.
    pub root_ca_path: Option<PathBuf>,
    /// Permit a non-TLS address. Callers must already have applied the local-only policy.
    pub allow_plaintext: bool,
    /// Permit a DSN with no password. Callers must already have applied the local-only policy.
    pub allow_unauthenticated: bool,
    /// Bound for one command, including the wait for a reconnect.
    pub command_timeout: Duration,
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
    /// No password in the DSN and unauthenticated access was not allowed.
    #[error("cache unauthenticated connection is refused")]
    UnauthenticatedRefused,
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
    /// The client or the lazy connection manager could not be built.
    #[error("{0}")]
    Client(&'static str),
}

/// A cache call that the caller should treat as a miss and continue without.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("cache unavailable")]
pub struct Unavailable;

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

/// A lazy standalone Redis-compatible connection.
///
/// [`Debug`] prints only host, port, and whether TLS is in use.
#[derive(Clone)]
pub struct Cache {
    connection: ConnectionManager,
    server: ServerIdentity,
    command_timeout: Duration,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the connection can carry the DSN; Debug prints only host, port, and TLS"
)]
impl std::fmt::Debug for Cache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Cache")
            .field("host", &self.server.host)
            .field("port", &self.server.port)
            .field("tls", &self.server.tls)
            .finish()
    }
}

impl Cache {
    /// Admit the DSN and build a lazy connection. This does not open a socket.
    ///
    /// Must be called from a Tokio runtime. The lazy manager spawns a disconnect
    /// watcher and does not dial until the first command.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError`] when the DSN, address policy, or CA file is refused,
    /// when no Tokio runtime is current, or when the client cannot be built.
    /// The message does not include the DSN.
    pub fn connect(options: CacheOptions) -> Result<Self, CacheError> {
        let CacheOptions {
            dsn,
            root_ca_path,
            allow_plaintext,
            allow_unauthenticated,
            command_timeout,
        } = options;
        let mut info = dsn
            .expose_secret()
            .into_connection_info()
            .map_err(|_| CacheError::InvalidDsn)?;
        let (host, port, tls) = admit_address(&info, allow_plaintext, root_ca_path.is_some())?;
        if info.redis_settings().password().is_none_or(str::is_empty) && !allow_unauthenticated {
            return Err(CacheError::UnauthenticatedRefused);
        }
        let root_cert = read_root_ca(root_ca_path.as_deref(), tls)?;
        info = info.set_tcp_settings(tcp_settings());
        if tls {
            install_tls_provider();
        }
        let client = match root_cert {
            Some(pem) => redis::Client::build_with_tls(
                info,
                redis::TlsCertificates {
                    client_tls: None,
                    root_cert: Some(pem),
                },
            )
            .map_err(|_| CacheError::Client("cache client could not be built"))?,
            None => redis::Client::open(info)
                .map_err(|_| CacheError::Client("cache client could not be built"))?,
        };
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(CacheError::Client("cache connection could not be built"));
        }
        let connection =
            ConnectionManager::new_lazy_with_config(client, manager_config(command_timeout))
                .map_err(|_| CacheError::Client("cache connection could not be built"))?;
        Ok(Self {
            connection,
            server: ServerIdentity { host, port, tls },
            command_timeout,
        })
    }

    /// A named view of this connection.
    ///
    /// Keys are stored as `{name}:{key}`. `name` is the `cache` metric label.
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
        }
    }

    /// Readiness probe. Not registered by default: a cache outage is degradation.
    #[must_use]
    pub fn probe(&self) -> CacheProbe {
        CacheProbe {
            connection: self.connection.clone(),
            command_timeout: self.command_timeout,
            server: self.server.clone(),
        }
    }

    /// The underlying multiplexed connection, for a later rate-limit or lock feature.
    ///
    /// Commands sent here are not covered by [`CacheOptions::command_timeout`]
    /// and are not recorded on the cache histogram.
    #[must_use]
    pub fn connection(&self) -> ConnectionManager {
        self.connection.clone()
    }

    /// Admitted server identity for logs.
    #[must_use]
    pub fn server(&self) -> ServerIdentity {
        self.server.clone()
    }
}

/// One feature's keys on a shared connection.
#[derive(Clone)]
pub struct CacheNamespace {
    cache: Cache,
    name: &'static str,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the connection can carry the DSN; Debug prints only the namespace and server identity"
)]
impl std::fmt::Debug for CacheNamespace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CacheNamespace")
            .field("name", &self.name)
            .field("host", &self.cache.server.host)
            .field("port", &self.cache.server.port)
            .field("tls", &self.cache.server.tls)
            .finish()
    }
}

impl CacheNamespace {
    /// `GET`. `Ok(None)` is a miss. [`Unavailable`] means the caller should degrade.
    ///
    /// # Errors
    ///
    /// Returns [`Unavailable`] when the command times out or the server cannot be used.
    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Unavailable> {
        let mut guard = self.guard("get", "GET");
        let stored = self.stored_key(key);
        let mut connection = self.cache.connection.clone();
        let span = guard.span();
        let result = tokio::time::timeout(self.cache.command_timeout, async move {
            redis::cmd("GET")
                .arg(stored)
                .query_async::<Option<Vec<u8>>>(&mut connection)
                .await
        })
        .instrument(span)
        .await;
        match classify(result) {
            Ok(value) => {
                guard.succeed(if value.is_some() { "hit" } else { "miss" });
                Ok(value)
            }
            Err((outcome, error_type)) => Err(guard.fail(outcome, error_type)),
        }
    }

    /// `SET key value PX milliseconds`.
    ///
    /// # Errors
    ///
    /// Returns [`Unavailable`] when `ttl` is below 1 ms, without sending a command,
    /// and when the command times out or the server cannot be used. A timed-out
    /// `SET` is not retried: the write may have landed, and the TTL bounds staleness.
    pub async fn set(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), Unavailable> {
        let mut guard = self.guard("set", "SET");
        if ttl < Duration::from_millis(1) {
            return Err(guard.fail("error", "invalid_ttl"));
        }
        let stored = self.stored_key(key);
        let value = value.to_vec();
        let milliseconds = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
        let mut connection = self.cache.connection.clone();
        let span = guard.span();
        let result = tokio::time::timeout(self.cache.command_timeout, async move {
            redis::cmd("SET")
                .arg(stored)
                .arg(value)
                .arg("PX")
                .arg(milliseconds)
                .query_async::<redis::Value>(&mut connection)
                .await
        })
        .instrument(span)
        .await;
        match classify(result) {
            Ok(_) => {
                guard.succeed("ok");
                Ok(())
            }
            Err((outcome, error_type)) => Err(guard.fail(outcome, error_type)),
        }
    }

    /// `DEL`. The deleted-count is ignored; a missing key is still success.
    ///
    /// # Errors
    ///
    /// Returns [`Unavailable`] when the command times out or the server cannot be used.
    pub async fn delete(&self, key: &str) -> Result<(), Unavailable> {
        let mut guard = self.guard("delete", "DEL");
        let stored = self.stored_key(key);
        let mut connection = self.cache.connection.clone();
        let span = guard.span();
        let result = tokio::time::timeout(self.cache.command_timeout, async move {
            redis::cmd("DEL")
                .arg(stored)
                .query_async::<redis::Value>(&mut connection)
                .await
        })
        .instrument(span)
        .await;
        match classify(result) {
            Ok(_) => {
                guard.succeed("ok");
                Ok(())
            }
            Err((outcome, error_type)) => Err(guard.fail(outcome, error_type)),
        }
    }

    fn guard(&self, operation: &'static str, redis_operation: &'static str) -> OperationGuard {
        OperationGuard::start(
            self.name,
            operation,
            redis_operation,
            &self.cache.server.host,
            self.cache.server.port,
        )
    }

    fn stored_key(&self, key: &str) -> String {
        format!("{}:{key}", self.name)
    }
}

/// `PING` probe. The name is `cache`. Failure text is `cache ping failed: <error.type>` only.
#[derive(Clone)]
pub struct CacheProbe {
    connection: ConnectionManager,
    command_timeout: Duration,
    server: ServerIdentity,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the connection can carry the DSN; Debug prints only host, port, and TLS"
)]
impl std::fmt::Debug for CacheProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CacheProbe")
            .field("host", &self.server.host)
            .field("port", &self.server.port)
            .field("tls", &self.server.tls)
            .finish()
    }
}

#[async_trait::async_trait]
impl health::Probe for CacheProbe {
    fn name(&self) -> &'static str {
        "cache"
    }

    async fn check(&self) -> Result<(), health::ProbeError> {
        let mut connection = self.connection.clone();
        let result = tokio::time::timeout(self.command_timeout, async move {
            redis::cmd("PING")
                .query_async::<redis::Value>(&mut connection)
                .await
        })
        .await;
        match result {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(health::ProbeError::new(format!(
                "cache ping failed: {}",
                observe::error_type(&error)
            ))),
            Err(_) => Err(health::ProbeError::new("cache ping failed: timeout")),
        }
    }
}

fn admit_address(
    info: &redis::ConnectionInfo,
    allow_plaintext: bool,
    has_root_ca: bool,
) -> Result<(String, u16, bool), CacheError> {
    match info.addr() {
        redis::ConnectionAddr::Tcp(host, port) => {
            if !allow_plaintext {
                return Err(CacheError::PlaintextRefused);
            }
            if has_root_ca {
                return Err(CacheError::CaRequiresTls);
            }
            Ok((host.clone(), *port, false))
        }
        redis::ConnectionAddr::TcpTls { insecure: true, .. } => Err(CacheError::InsecureTlsRefused),
        redis::ConnectionAddr::TcpTls {
            host,
            port,
            insecure: false,
            ..
        } => Ok((host.clone(), *port, true)),
        // Unix sockets and any future address class are outside standalone TCP.
        _ => Err(CacheError::UnsupportedAddress),
    }
}

fn read_root_ca(path: Option<&Path>, tls: bool) -> Result<Option<Vec<u8>>, CacheError> {
    let Some(path) = path else {
        return Ok(None);
    };
    if !tls {
        return Err(CacheError::CaRequiresTls);
    }
    let bytes = std::fs::read(path).map_err(|error| CacheError::CaFile { kind: error.kind() })?;
    let pem = String::from_utf8_lossy(&bytes);
    if !pem.contains(PEM_CERTIFICATE_MARKER) {
        return Err(CacheError::InvalidCa);
    }
    Ok(Some(bytes))
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

fn manager_config(command_timeout: Duration) -> redis::aio::ConnectionManagerConfig {
    redis::aio::ConnectionManagerConfig::new()
        .set_response_timeout(Some(command_timeout))
        .set_connection_timeout(Some(CONNECT_TIMEOUT))
        .set_min_delay(MIN_DELAY)
        .set_exponent_base(EXPONENT_BASE)
        .set_max_delay(MAX_DELAY)
        .set_number_of_retries(NUMBER_OF_RETRIES)
}

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
