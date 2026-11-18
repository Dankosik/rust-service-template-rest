use std::sync::Arc;
use std::time::Duration;

use operation_context::{Deadline, OperationContext, Stopped};
use redis::aio::MultiplexedConnection;
use redis::{SetExpiry, SetOptions};
use tokio::time::Instant;

use crate::connection::reply_data;
use crate::error::{CommandError, ErrorType};
use crate::observe::{LocalSizeRefusal, OperationGuard, Outcome};
use crate::{
    Cache, CacheLookup, CacheNamespace, MutationError, Namespace, OperationError, SetError,
};

impl Cache {
    #[must_use]
    pub fn namespace(&self, Namespace(name): Namespace) -> CacheNamespace {
        CacheNamespace {
            cache: self.clone(),
            name,
            histograms: Arc::default(),
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
        Ok(result.unwrap_or_default())
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
}

impl CacheNamespace {
    pub async fn get(
        &self,
        parent: &OperationContext,
        key: &str,
    ) -> Result<Option<Vec<u8>>, OperationError> {
        let key = self.admit_key(key)?;
        let limit = self.cache.max_value_bytes;
        self.run(
            parent,
            Operation::Get,
            |connection| {
                let mut pipeline = redis::Pipeline::with_capacity(2);
                pipeline
                    .atomic()
                    .cmd("EXISTS")
                    .arg(&key)
                    .cmd("GETRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(limit);
                query_read(pipeline, connection, limit)
            },
            |value| match value {
                Some(bytes) if bytes.len() > limit => Err(ErrorType::ValueTooLarge),
                Some(_) => Ok(Outcome::Hit),
                None => Ok(Outcome::Miss),
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
        let operation = Operation::Set;
        let key = self.admit_key(key).map_err(MutationError::NotApplied)?;
        if value.len() > self.cache.max_value_bytes {
            LocalSizeRefusal::Value.record(self.name);
            return Err(MutationError::NotApplied(OperationError::ValueTooLarge).into());
        }
        let build_query = |connection| {
            let options =
                SetOptions::default().with_expiration(SetExpiry::PX(milliseconds.cast_unsigned()));
            let mut command = key.command(
                operation,
                ExtraCommandCapacity {
                    extra_args: 3,
                    extra_bytes: value.len() + 2 + 20,
                },
            );
            command.arg(value).arg(options);
            query_mutation(command, connection, operation)
        };
        self.run(parent, operation, build_query, |()| Ok(Outcome::Ok))
            .await
            .map_err(Into::into)
    }

    pub async fn delete(&self, parent: &OperationContext, key: &str) -> Result<(), MutationError> {
        let operation = Operation::Delete;
        let key = self.admit_key(key).map_err(MutationError::NotApplied)?;
        let build_query = |connection| {
            let command = key.command(operation, ExtraCommandCapacity::default());
            query_mutation(command, connection, operation)
        };
        self.run(parent, operation, build_query, |()| Ok(Outcome::Ok))
            .await
            .map_err(Into::into)
    }

    async fn run<T: Send + 'static, F>(
        &self,
        parent: &OperationContext,
        operation: Operation,
        build_query: impl FnOnce(MultiplexedConnection) -> F,
        outcome: impl FnOnce(&T) -> Result<Outcome, ErrorType>,
    ) -> Result<T, CommandError>
    where
        F: Future<Output = redis::RedisResult<T>> + Send + 'static,
    {
        let guard = OperationGuard::start(
            self.name,
            &self.histograms,
            operation,
            &self.cache.link.server,
        );
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
                guard.failed(error);
                Err(error)
            }
        }
    }

    fn admit_key<'a>(&self, key: &'a str) -> Result<NamespacedKey<'a>, OperationError> {
        let full_key = NamespacedKey {
            namespace: self.name,
            key,
        };
        if full_key.len() > self.cache.max_key_bytes {
            LocalSizeRefusal::Key.record(self.name);
            return Err(OperationError::KeyTooLarge);
        }
        Ok(full_key)
    }
}

pub(super) async fn query_mutation(
    command: redis::Cmd,
    mut connection: MultiplexedConnection,
    operation: Operation,
) -> redis::RedisResult<()> {
    let reply = tokio::task::unconstrained(connection.send_packed_command(&command)).await?;
    match (operation, reply_data(reply)?) {
        (Operation::Set, redis::Value::Okay) | (Operation::Delete, redis::Value::Int(0 | 1)) => {
            Ok(())
        }
        _ => Err((redis::ErrorKind::Parse, "unexpected cache command reply").into()),
    }
}

async fn query_read(
    pipeline: redis::Pipeline,
    mut connection: MultiplexedConnection,
    limit: usize,
) -> redis::RedisResult<Option<Vec<u8>>> {
    use redis::Value;

    let replies =
        tokio::task::unconstrained(connection.send_packed_commands(&pipeline, 0, 4)).await?;
    let unexpected =
        || redis::RedisError::from((redis::ErrorKind::Parse, "unexpected cache read reply"));
    let [multi, exists_queued, range_queued, exec] =
        <[_; 4]>::try_from(replies).map_err(|_| unexpected())?;
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
    let [exists, value] = <[_; 2]>::try_from(results).map_err(|_| unexpected())?;
    match (reply_data(exists)?, reply_data(value)?) {
        (Value::Int(0), Value::BulkString(bytes)) if bytes.is_empty() => Ok(None),
        (Value::Int(1), Value::BulkString(bytes)) if bytes.len() <= limit + 1 => Ok(Some(bytes)),
        _ => Err(unexpected()),
    }
}

#[derive(Clone, Copy, Default)]
struct ExtraCommandCapacity {
    extra_args: usize,
    extra_bytes: usize,
}

pub(crate) const fn valid_namespace(name: &str) -> bool {
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
    namespace: &'static str,
    key: &'a str,
}

impl NamespacedKey<'_> {
    fn len(&self) -> usize {
        self.namespace.len() + 1 + self.key.len()
    }

    fn command(self, operation: Operation, extra_capacity: ExtraCommandCapacity) -> redis::Cmd {
        let mut command = redis::Cmd::with_capacity(
            2 + extra_capacity.extra_args,
            operation.redis_name().len() + self.len() + extra_capacity.extra_bytes,
        );
        command.arg(operation.redis_name()).arg(self);
        command
    }
}

impl redis::ToRedisArgs for NamespacedKey<'_> {
    fn write_redis_args<W: redis::RedisWrite + ?Sized>(&self, out: &mut W) {
        out.write_arg_fmt(format_args!("{}:{}", self.namespace, self.key));
    }
}
