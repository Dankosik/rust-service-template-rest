use std::sync::Arc;
use std::time::Duration;

use operation_context::{Deadline, OperationContext, Stopped};
use redis::aio::MultiplexedConnection;
use redis::{SetExpiry, SetOptions};
use tokio::time::Instant;

use crate::connection::reply_data;
use crate::error::{CommandError, ErrorType};
use crate::observe::{LocalSizeRefusal, NamespaceHistograms, OperationGuard, SuccessOutcome};
use crate::{Cache, MutationError, OperationError, SetError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Namespace(&'static str);

impl Namespace {
    #[must_use]
    pub const fn new(name: &'static str) -> Option<Self> {
        if valid_namespace(name) {
            Some(Self(name))
        } else {
            None
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        self.0
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
    histograms: Arc<NamespaceHistograms>,
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
            histograms: Arc::new(NamespaceHistograms::new(namespace)),
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

    pub async fn lookup(
        &self,
        parent: &OperationContext,
        key: &str,
    ) -> Result<Option<Vec<u8>>, Stopped> {
        parent.check()?;
        let Some(namespace) = &self.namespace else {
            return Ok(None);
        };
        let child = if let Some(deadline) = parent.deadline() {
            let now = Instant::now();
            let budget = deadline
                .remaining_at(now)
                .saturating_sub(self.source_reserve);
            if budget.is_zero() {
                return parent.check().map(|()| None);
            }
            Some(parent.child_until(Deadline::new(now, budget)))
        } else {
            None
        };

        let result = namespace.get(child.as_ref().unwrap_or(parent), key).await;
        parent.check()?;
        Ok(result.ok().flatten())
    }
}

#[derive(Clone, Copy, enum_map::Enum)]
pub(super) enum Operation {
    Get,
    Set,
    Delete,
}

impl Operation {
    pub(super) fn redis_name(self) -> &'static str {
        match self {
            Self::Get => "MULTI",
            Self::Set => "SET",
            Self::Delete => "DEL",
        }
    }

    pub(super) fn redis_batch_size(self) -> Option<i64> {
        match self {
            Self::Get => Some(2),
            Self::Set | Self::Delete => None,
        }
    }
}

impl CacheNamespace {
    pub async fn get(
        &self,
        parent: &OperationContext,
        key: &str,
    ) -> Result<Option<Vec<u8>>, OperationError> {
        let key = self.admit_key(key)?;
        let max_value_bytes = self.cache.max_value_bytes;
        self.run(
            parent,
            Operation::Get,
            |connection| {
                query_read(
                    key.read_transaction(max_value_bytes),
                    connection,
                    max_value_bytes,
                )
            },
            |value| match value {
                Some(bytes) if bytes.len() > max_value_bytes => Err(ErrorType::ValueTooLarge),
                Some(_) => Ok(SuccessOutcome::Hit),
                None => Ok(SuccessOutcome::Miss),
            },
        )
        .await
        .map_err(Into::into)
    }

    pub async fn set(
        &self,
        parent: &OperationContext,
        key: &str,
        value: &[u8],
        ttl: Duration,
    ) -> Result<(), SetError> {
        let milliseconds = i64::try_from(ttl.as_millis())
            .ok()
            .filter(|milliseconds| *milliseconds > 0)
            .ok_or(SetError::InvalidTtl)?;
        let key = self.admit_key(key).map_err(MutationError::NotApplied)?;
        if value.len() > self.cache.max_value_bytes {
            LocalSizeRefusal::Value.record(self.histograms.namespace());
            return Err(MutationError::NotApplied(OperationError::ValueTooLarge).into());
        }
        let build_query = |connection| {
            query_mutation(
                key.set_command(value, milliseconds.cast_unsigned()),
                connection,
                |value| matches!(value, redis::Value::Okay),
            )
        };
        self.run(parent, Operation::Set, build_query, |()| {
            Ok(SuccessOutcome::Ok)
        })
        .await
        .map_err(Into::into)
    }

    pub async fn delete(&self, parent: &OperationContext, key: &str) -> Result<(), MutationError> {
        let key = self.admit_key(key).map_err(MutationError::NotApplied)?;
        let build_query = |connection| {
            query_mutation(key.delete_command(), connection, |value| {
                matches!(value, redis::Value::Int(0 | 1))
            })
        };
        self.run(parent, Operation::Delete, build_query, |()| {
            Ok(SuccessOutcome::Ok)
        })
        .await
        .map_err(Into::into)
    }

    async fn run<T: Send + 'static, F>(
        &self,
        parent: &OperationContext,
        operation: Operation,
        build_query: impl FnOnce(MultiplexedConnection) -> F,
        outcome: impl FnOnce(&T) -> Result<SuccessOutcome, ErrorType>,
    ) -> Result<T, CommandError>
    where
        F: Future<Output = redis::RedisResult<T>> + Send + 'static,
    {
        let guard = OperationGuard::start(&self.histograms, operation, &self.cache.link.server);
        match self
            .cache
            .link
            .command(build_query, parent, operation)
            .await
            .and_then(|value| Ok((outcome(&value)?, value)))
        {
            Ok((outcome, value)) => {
                guard.succeed(outcome);
                Ok(value)
            }
            Err(error) => {
                guard.fail(error);
                Err(error)
            }
        }
    }

    fn admit_key<'a>(&self, key: &'a str) -> Result<NamespacedKey<'a>, OperationError> {
        let namespace = self.histograms.namespace();
        let full_key = NamespacedKey { namespace, key };
        if full_key.len() > self.cache.max_key_bytes {
            LocalSizeRefusal::Key.record(namespace);
            return Err(OperationError::KeyTooLarge);
        }
        Ok(full_key)
    }
}

pub(super) async fn query_mutation(
    command: redis::Cmd,
    mut connection: MultiplexedConnection,
    accepts: fn(redis::Value) -> bool,
) -> redis::RedisResult<()> {
    let reply = tokio::task::unconstrained(connection.send_packed_command(&command)).await?;
    if accepts(reply_data(reply)?) {
        Ok(())
    } else {
        Err((redis::ErrorKind::Parse, "unexpected cache command reply").into())
    }
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
        (Value::Int(1), Value::BulkString(bytes)) if bytes.len() <= max_value_bytes + 1 => {
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

        let command_name = Operation::Set.redis_name();
        let options = SetOptions::default().with_expiration(SetExpiry::PX(ttl_ms));
        let mut command = redis::Cmd::with_capacity(
            2 + SET_PX_EXTRA_ARGS,
            command_name.len() + self.len() + value.len() + PX_OPTION_BYTES + MAX_U64_DECIMAL_BYTES,
        );
        command.arg(command_name).arg(self).arg(value).arg(options);
        command
    }

    fn delete_command(self) -> redis::Cmd {
        let command_name = Operation::Delete.redis_name();
        let mut command = redis::Cmd::with_capacity(2, command_name.len() + self.len());
        command.arg(command_name).arg(self);
        command
    }
}

impl redis::ToRedisArgs for NamespacedKey<'_> {
    fn write_redis_args<W: redis::RedisWrite + ?Sized>(&self, out: &mut W) {
        out.write_arg_fmt(format_args!("{}:{}", self.namespace.as_str(), self.key));
    }
}
