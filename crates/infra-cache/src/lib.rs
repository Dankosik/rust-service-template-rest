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
//! match profiles.set(key, &bytes, Duration::from_secs(60)).await {
//!     Ok(()) | Err(SetError::Unavailable(_)) => {}
//!     Err(error @ SetError::InvalidTtl) => return Err(error),
//! }
//! Ok(bytes)
//! # }
//! ```

mod admission;
mod connection;
mod credentials;
mod observe;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use operation_context::OperationContext;
use redis::{SetExpiry, SetOptions};
use secrecy::SecretString;

use self::admission::Admitted;
use self::connection::{CommandError, CommandScope, Link};
pub use self::observe::{HISTOGRAMS, OPERATION_DURATION_BUCKETS, OPERATION_DURATION_METRIC};
use self::observe::{NamespaceHistograms, OperationGuard, Outcome};

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

/// Why admission refused to build a client. Display never includes the DSN,
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
    /// Validate the options, build the client, and start connecting in the background.
    ///
    /// Must be called from a Tokio runtime with the time and I/O drivers enabled.
    /// Configured password and TLS files are read synchronously, so an async
    /// caller runs this on the blocking pool with the runtime entered. Commands
    /// and the probe report an outage.
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
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(CacheError::NoRuntime);
        }
        // Tokio exposes a missing time driver as a panic at timer construction.
        std::panic::catch_unwind(|| drop(tokio::time::sleep(Duration::ZERO)))
            .map_err(|_| CacheError::NoTimeDriver)?;
        admission::check_io_driver()?;
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
    pub fn server(&self) -> &ServerEndpoint {
        &self.link.server
    }
}

/// `PING` probe. The name is `cache`. Failure text is `cache ping failed: <error.type>` only.
///
/// The caller's startup/readiness budget can cancel the probe. A PING that
/// can finish normally gets enough time for commands already ahead of it.
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

/// The closed set of namespace operations.
#[derive(Clone, Copy, enum_map::Enum)]
enum Operation {
    Get,
    Set,
    Delete,
}

impl Operation {
    fn redis_name(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Set => "SET",
            Self::Delete => "DEL",
        }
    }

    /// A stopped caller discards a read reply as a timeout or cancellation.
    /// A confirmed mutation is final and keeps its result.
    fn is_read(self) -> bool {
        match self {
            Self::Get => true,
            Self::Set | Self::Delete => false,
        }
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

impl CacheNamespace {
    /// `GET`. `Ok(None)` is a miss. [`Unavailable`] means the caller should degrade.
    ///
    /// # Errors
    ///
    /// Returns [`Unavailable`] for a timeout, a stop, an unusable server, or a
    /// server error reply.
    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Unavailable> {
        self.get_bounded(key, &self.standalone_command_scope())
            .await
    }

    /// `GET` within the caller's remaining budget and command ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`Unavailable`] for a timeout, a stop, an unusable server, or a
    /// server error reply.
    pub async fn get_with_context(
        &self,
        key: &str,
        context: &OperationContext,
    ) -> Result<Option<Vec<u8>>, Unavailable> {
        self.get_bounded(key, &self.command_scope(context)).await
    }

    async fn get_bounded(
        &self,
        key: &str,
        scope: &CommandScope,
    ) -> Result<Option<Vec<u8>>, Unavailable> {
        let operation = Operation::Get;
        let command = self.keyed_command(operation, key, ExtraCommandCapacity::default());
        self.run(scope, operation, command, |value: &Option<Vec<u8>>| {
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
    /// out-of-range TTL, or [`SetError::Unavailable`] for a stopped command, an
    /// unusable server, or a server refusal such as an admitted TTL whose
    /// absolute expiry overflows. A timed-out write may have landed and is
    /// never replayed.
    pub async fn set(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), SetError> {
        self.set_bounded(key, value, ttl, &self.standalone_command_scope())
            .await
    }

    /// `SET` within the caller's remaining budget and command ceiling.
    ///
    /// The TTL contract is the same as [`Self::set`].
    ///
    /// # Errors
    ///
    /// Returns [`SetError::InvalidTtl`] before considering caller cancellation,
    /// admission or observation, or [`SetError::Unavailable`] for a stopped
    /// command, an unusable server, or a server refusal. A stopped write may
    /// have landed; it is not replayed.
    pub async fn set_with_context(
        &self,
        key: &str,
        value: &[u8],
        ttl: Duration,
        context: &OperationContext,
    ) -> Result<(), SetError> {
        self.set_bounded(key, value, ttl, &self.command_scope(context))
            .await
    }

    async fn set_bounded(
        &self,
        key: &str,
        value: &[u8],
        ttl: Duration,
        scope: &CommandScope,
    ) -> Result<(), SetError> {
        let milliseconds = i64::try_from(ttl.as_millis())
            .ok()
            .filter(|milliseconds| *milliseconds > 0)
            .ok_or(SetError::InvalidTtl)?;
        let operation = Operation::Set;
        let options =
            SetOptions::default().with_expiration(SetExpiry::PX(milliseconds.cast_unsigned()));
        // Value, `PX`, and an expiry of at most 20 decimal digits (u64).
        let mut command = self.keyed_command(
            operation,
            key,
            ExtraCommandCapacity {
                extra_args: 3,
                extra_bytes: value.len() + 2 + 20,
            },
        );
        command.arg(value).arg(options);
        self.run(scope, operation, command, |(): &()| Outcome::Ok)
            .await
            .map_err(SetError::Unavailable)
    }

    /// `DEL`. The deleted-count is ignored; a missing key is still success.
    ///
    /// # Errors
    ///
    /// Returns [`Unavailable`] for a timeout, a stop, an unusable server, or a
    /// server error reply. A timed-out deletion may have landed and is
    /// never replayed.
    pub async fn delete(&self, key: &str) -> Result<(), Unavailable> {
        self.delete_bounded(key, &self.standalone_command_scope())
            .await
    }

    /// `DEL` within the caller's remaining budget and command ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`Unavailable`] for a timeout, a stop, an unusable server, or a
    /// server error reply. A stopped pending deletion may have landed and is
    /// never replayed.
    pub async fn delete_with_context(
        &self,
        key: &str,
        context: &OperationContext,
    ) -> Result<(), Unavailable> {
        self.delete_bounded(key, &self.command_scope(context)).await
    }

    async fn delete_bounded(&self, key: &str, scope: &CommandScope) -> Result<(), Unavailable> {
        let operation = Operation::Delete;
        let command = self.keyed_command(operation, key, ExtraCommandCapacity::default());
        self.run(scope, operation, command, |(): &()| Outcome::Ok)
            .await
    }

    /// The caller's context under this cache's command ceiling. Applied at the
    /// public operation so preparation and dispatch share one cutoff.
    fn command_scope(&self, context: &OperationContext) -> CommandScope {
        CommandScope::under(context, self.cache.command_timeout)
    }

    /// [`Self::command_scope`] of an unbounded parent.
    fn standalone_command_scope(&self) -> CommandScope {
        CommandScope::standalone(self.cache.command_timeout)
    }

    /// One observed command under the caller's already-bounded context.
    /// `outcome` names a successful reply.
    async fn run<T: redis::FromRedisValue>(
        &self,
        scope: &CommandScope,
        operation: Operation,
        command: redis::Cmd,
        outcome: fn(&T) -> Outcome,
    ) -> Result<T, Unavailable> {
        let guard = OperationGuard::start(
            self.name,
            &self.histograms,
            operation,
            &self.cache.link.server,
        );
        // One context cutoff includes preparation, connection wait, and exactly one dispatch.
        match self.cache.link.command(&command, scope).await {
            Ok(value) => {
                if operation.is_read()
                    && let Some(stopped) = scope.context.stopped()
                {
                    return Err(guard.stop(stopped));
                }
                guard.succeed(outcome(&value));
                Ok(value)
            }
            Err(CommandError::Stopped(stopped)) => Err(guard.stop(stopped)),
            Err(CommandError::Failed(error)) => Err(guard.fail(error)),
        }
    }

    /// Reserve both argument slots and payload bytes (excluding RESP framing)
    /// to avoid growing either buffer when adding arguments.
    fn keyed_command(
        &self,
        operation: Operation,
        key: &str,
        extra_capacity: ExtraCommandCapacity,
    ) -> redis::Cmd {
        let key = NamespacedKey {
            namespace: self.name,
            key,
        };
        let mut command = redis::Cmd::with_capacity(
            2 + extra_capacity.extra_args,
            operation.redis_name().len() + key.len() + extra_capacity.extra_bytes,
        );
        command.arg(operation.redis_name()).arg(key);
        command
    }
}

#[derive(Clone, Copy, Default)]
struct ExtraCommandCapacity {
    extra_args: usize,
    extra_bytes: usize,
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
/// [`redis::Cmd`]'s [`write_arg_fmt`](redis::RedisWrite::write_arg_fmt) writes
/// directly into its preallocated payload buffer.
struct NamespacedKey<'a> {
    namespace: &'static str,
    key: &'a str,
}

impl NamespacedKey<'_> {
    /// Payload bytes of the `{namespace}:{key}` argument.
    fn len(&self) -> usize {
        self.namespace.len() + 1 + self.key.len()
    }
}

impl redis::ToRedisArgs for NamespacedKey<'_> {
    fn write_redis_args<W: redis::RedisWrite + ?Sized>(&self, out: &mut W) {
        out.write_arg_fmt(format_args!("{}:{}", self.namespace, self.key));
    }
}
impl redis::ToSingleRedisArg for NamespacedKey<'_> {}
