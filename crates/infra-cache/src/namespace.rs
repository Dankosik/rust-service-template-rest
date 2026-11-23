use std::sync::Arc;
use std::time::Duration;

use operation_context::{OperationContext, Stopped};
use redis::aio::MultiplexedConnection;
use redis::{SetExpiry, SetOptions};

use crate::connection::AfterReply;
use crate::error::{MutationKind, ReadFailure};
use crate::exchange::{reply_data, send_accepted};
use crate::observe::{
    LocalSizeRefusal, NamespaceMetrics, Operation, OperationGuard, SuccessOutcome,
};
use crate::{Cache, MutationError, MutationFailure, OperationError};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Namespace(&'static str);

impl Namespace {
    #[must_use]
    pub const fn try_new(name: &'static str) -> Option<Self> {
        if valid_namespace(name) {
            Some(Self(name))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::borrow::Borrow<str> for Namespace {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Namespace {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
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
            $crate::Namespace::try_new($name)
                .expect("cache namespace must match ^[a-z][a-z0-9_]{0,63}$")
        }
    };
}

#[derive(Clone, Debug)]
pub struct CacheNamespace {
    cache: Cache,
    metrics: Arc<NamespaceMetrics>,
}

#[derive(Clone, Debug)]
pub struct CacheLookup {
    namespace: Option<CacheNamespace>,
    source_reserve: Duration,
}

impl Cache {
    #[must_use]
    pub fn namespace(&self, namespace: Namespace) -> CacheNamespace {
        CacheNamespace {
            cache: self.clone(),
            metrics: Arc::new(NamespaceMetrics::new(namespace)),
        }
    }
}

impl CacheLookup {
    #[must_use]
    pub const fn new(namespace: Option<CacheNamespace>, source_reserve: Duration) -> Self {
        Self {
            namespace,
            source_reserve,
        }
    }

    #[must_use]
    pub const fn namespace(&self) -> Option<&CacheNamespace> {
        self.namespace.as_ref()
    }

    pub async fn lookup(
        &self,
        parent: &OperationContext,
        key: &str,
    ) -> Result<Option<Vec<u8>>, Stopped> {
        parent.check()?;
        let Some(namespace) = &self.namespace else {
            return Ok(None);
        };
        let wait_ceiling = parent
            .deadline()
            .map(|deadline| deadline.remaining().saturating_sub(self.source_reserve));
        if wait_ceiling.is_some_and(|ceiling| ceiling.is_zero()) {
            return parent.check().map(|()| None);
        }

        let result = namespace.get_with_ceiling(parent, key, wait_ceiling).await;
        parent.check()?;
        Ok(result.ok().flatten())
    }
}

impl CacheNamespace {
    pub async fn get(
        &self,
        parent: &OperationContext,
        key: &str,
    ) -> Result<Option<Vec<u8>>, OperationError> {
        self.get_with_ceiling(parent, key, None).await
    }

    async fn get_with_ceiling(
        &self,
        parent: &OperationContext,
        key: &str,
        wait_ceiling: Option<Duration>,
    ) -> Result<Option<Vec<u8>>, OperationError> {
        let key = self.admit_key(key).ok_or(OperationError::KeyTooLarge)?;
        let max_value_bytes = self.cache.max_value_bytes;
        let guard = OperationGuard::start(&self.metrics, Operation::Get, &self.cache.link.server);
        let result = self
            .cache
            .link
            .dispatch(
                |connection| {
                    query_read(
                        key.read_transaction(max_value_bytes),
                        connection,
                        max_value_bytes,
                    )
                },
                parent,
                wait_ceiling,
                AfterReply::HonorStop,
            )
            .await
            .map_err(ReadFailure::from)
            .and_then(|value| match value {
                Some(bytes) if bytes.len() > max_value_bytes => Err(ReadFailure::ValueTooLarge),
                value => Ok(value),
            });
        match result {
            Ok(value) => {
                guard.succeed(if value.is_some() {
                    SuccessOutcome::Hit
                } else {
                    SuccessOutcome::Miss
                });
                Ok(value)
            }
            Err(failure) => {
                guard.fail_read(failure);
                Err(failure.into())
            }
        }
    }

    pub async fn set(
        &self,
        parent: &OperationContext,
        key: &str,
        value: &[u8],
        ttl: Duration,
    ) -> Result<(), MutationError> {
        let milliseconds = i64::try_from(ttl.as_millis())
            .ok()
            .filter(|milliseconds| *milliseconds > 0)
            .ok_or(MutationError::NotApplied(MutationFailure::InvalidTtl))?;
        let key = self
            .admit_key(key)
            .ok_or(MutationError::NotApplied(MutationFailure::KeyTooLarge))?;
        if value.len() > self.cache.max_value_bytes {
            self.metrics.record_size_refusal(LocalSizeRefusal::Value);
            return Err(MutationError::NotApplied(MutationFailure::ValueTooLarge));
        }
        self.run_mutation(parent, MutationKind::Set, |connection| {
            query_mutation(
                key.set_command(value, milliseconds.cast_unsigned()),
                connection,
                |reply| matches!(reply, redis::Value::Okay),
            )
        })
        .await
    }

    pub async fn delete(&self, parent: &OperationContext, key: &str) -> Result<(), MutationError> {
        let key = self
            .admit_key(key)
            .ok_or(MutationError::NotApplied(MutationFailure::KeyTooLarge))?;
        self.run_mutation(parent, MutationKind::Delete, |connection| {
            query_mutation(key.delete_command(), connection, |reply| {
                matches!(reply, redis::Value::Int(0 | 1))
            })
        })
        .await
    }

    async fn run_mutation<F>(
        &self,
        parent: &OperationContext,
        kind: MutationKind,
        build_query: impl FnOnce(MultiplexedConnection) -> F,
    ) -> Result<(), MutationError>
    where
        F: Future<Output = redis::RedisResult<()>> + Send + 'static,
    {
        let operation = match kind {
            MutationKind::Set => Operation::Set,
            MutationKind::Delete => Operation::Delete,
        };
        let guard = OperationGuard::start(&self.metrics, operation, &self.cache.link.server);
        let result = self
            .cache
            .link
            .dispatch(build_query, parent, None, AfterReply::KeepConfirmed)
            .await
            .map_err(|error| error.into_mutation_error(kind));
        match result {
            Ok(()) => {
                guard.succeed(SuccessOutcome::Ok);
                Ok(())
            }
            Err(error) => {
                guard.fail_mutation(error);
                Err(error.into())
            }
        }
    }

    fn admit_key<'a>(&self, key: &'a str) -> Option<NamespacedKey<'a>> {
        let namespace = self.metrics.namespace();
        let full_key = NamespacedKey { namespace, key };
        if full_key.len() > self.cache.max_key_bytes {
            self.metrics.record_size_refusal(LocalSizeRefusal::Key);
            return None;
        }
        Some(full_key)
    }
}

pub(super) async fn query_mutation(
    command: redis::Cmd,
    mut connection: MultiplexedConnection,
    accepts: fn(redis::Value) -> bool,
) -> redis::RedisResult<()> {
    send_accepted(&mut connection, &command, accepts).await
}

const READ_TRANSACTION_COMMANDS: usize = 2;
const READ_TRANSACTION_REPLIES: usize = READ_TRANSACTION_COMMANDS + 2;

async fn query_read(
    pipeline: redis::Pipeline,
    mut connection: MultiplexedConnection,
    max_value_bytes: usize,
) -> redis::RedisResult<Option<Vec<u8>>> {
    let replies = tokio::task::unconstrained(connection.send_packed_commands(
        &pipeline,
        0,
        READ_TRANSACTION_REPLIES,
    ))
    .await?;
    decode_read_transaction(replies, max_value_bytes)
}

const fn getrange_reply_limit(max_value_bytes: usize) -> usize {
    max_value_bytes + 1
}

fn decode_read_transaction(
    replies: Vec<redis::Value>,
    max_value_bytes: usize,
) -> redis::RedisResult<Option<Vec<u8>>> {
    use redis::Value;

    let unexpected =
        || redis::RedisError::from((redis::ErrorKind::Parse, "unexpected cache read reply"));
    let [multi, exists_queued, range_queued, exec] =
        <[_; READ_TRANSACTION_REPLIES]>::try_from(replies).map_err(|_| unexpected())?;
    if reply_data(multi)? != Value::Okay {
        return Err(unexpected());
    }
    for queued in [exists_queued, range_queued] {
        if !matches!(reply_data(queued)?, Value::SimpleString(status) if status == "QUEUED") {
            return Err(unexpected());
        }
    }
    let Value::Array(results) = reply_data(exec)? else {
        return Err(unexpected());
    };
    let [exists, value] =
        <[_; READ_TRANSACTION_COMMANDS]>::try_from(results).map_err(|_| unexpected())?;
    match (reply_data(exists)?, reply_data(value)?) {
        (Value::Int(0), Value::BulkString(bytes)) if bytes.is_empty() => Ok(None),
        (Value::Int(1), Value::BulkString(bytes))
            if bytes.len() <= getrange_reply_limit(max_value_bytes) =>
        {
            Ok(Some(bytes))
        }
        _ => Err(unexpected()),
    }
}

const fn valid_namespace(name: &str) -> bool {
    let [first, rest @ ..] = name.as_bytes() else {
        return false;
    };
    if !first.is_ascii_lowercase() || rest.len() > 63 {
        return false;
    }
    let mut rest = rest;
    while let [byte, tail @ ..] = rest {
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_') {
            return false;
        }
        rest = tail;
    }
    true
}

struct NamespacedKey<'a> {
    namespace: Namespace,
    key: &'a str,
}

impl NamespacedKey<'_> {
    fn len(&self) -> usize {
        self.namespace.as_str().len() + 1 + self.key.len()
    }

    fn read_transaction(self, max_value_bytes: usize) -> redis::Pipeline {
        let key_len = self.len();
        let mut buffer = std::fmt::NumBuffer::new();
        let range_end = max_value_bytes.format_into(&mut buffer);
        let mut exists = redis::Cmd::with_capacity(2, "EXISTS".len() + key_len);
        exists.arg("EXISTS").arg(&self);
        let mut range =
            redis::Cmd::with_capacity(4, "GETRANGE".len() + key_len + 1 + range_end.len());
        range.arg("GETRANGE").arg(self).arg("0").arg(range_end);

        let mut pipeline = redis::Pipeline::with_capacity(READ_TRANSACTION_COMMANDS);
        pipeline.atomic().add_command(exists).add_command(range);
        pipeline
    }

    fn set_command(self, value: &[u8], ttl_ms: u64) -> redis::Cmd {
        const SET_PX_EXTRA_ARGS: usize = 3;
        const PX_OPTION_BYTES: usize = "PX".len();
        const MAX_U64_DECIMAL_BYTES: usize = 20;

        let options = SetOptions::default().with_expiration(SetExpiry::PX(ttl_ms));
        let mut command = redis::Cmd::with_capacity(
            2 + SET_PX_EXTRA_ARGS,
            "SET".len() + self.len() + value.len() + PX_OPTION_BYTES + MAX_U64_DECIMAL_BYTES,
        );
        command.arg("SET").arg(self).arg(value).arg(options);
        command
    }

    fn delete_command(self) -> redis::Cmd {
        let mut command = redis::Cmd::with_capacity(2, "DEL".len() + self.len());
        command.arg("DEL").arg(self);
        command
    }
}

impl redis::ToRedisArgs for NamespacedKey<'_> {
    fn write_redis_args<W: redis::RedisWrite + ?Sized>(&self, out: &mut W) {
        use std::io::Write as _;

        let mut writer = out.writer_for_next_arg();
        let _ = writer.write_all(self.namespace.as_str().as_bytes());
        let _ = writer.write_all(b":");
        let _ = writer.write_all(self.key.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaced_key_encodes_exact_resp_bytes() {
        let ns = namespace!("cache_ns");
        for key in ["", "plain", "klúč:é🚀"] {
            let namespaced = NamespacedKey { namespace: ns, key };
            let formatted = format!("{}:{key}", ns.as_str());

            let expected_read = redis::pipe()
                .atomic()
                .cmd("EXISTS")
                .arg(&formatted)
                .cmd("GETRANGE")
                .arg(&formatted)
                .arg("0")
                .arg("64")
                .get_packed_pipeline();
            assert_eq!(
                namespaced.read_transaction(64).get_packed_pipeline(),
                expected_read,
                "{key:?}"
            );

            let mut expected_set = redis::cmd("SET");
            expected_set
                .arg(&formatted)
                .arg(b"payload")
                .arg("PX")
                .arg(1500_u64);
            assert_eq!(
                NamespacedKey { namespace: ns, key }
                    .set_command(b"payload", 1500)
                    .get_packed_command(),
                expected_set.get_packed_command(),
                "{key:?}"
            );

            let mut expected_del = redis::cmd("DEL");
            expected_del.arg(&formatted);
            assert_eq!(
                NamespacedKey { namespace: ns, key }
                    .delete_command()
                    .get_packed_command(),
                expected_del.get_packed_command(),
                "{key:?}"
            );
        }
    }
}
