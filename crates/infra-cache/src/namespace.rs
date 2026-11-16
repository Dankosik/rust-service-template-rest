use std::sync::Arc;
use std::time::Duration;

use operation_context::OperationContext;
use redis::{SetExpiry, SetOptions};

use crate::connection::{CommandError, CommandScope};
use crate::observe::{OperationGuard, Outcome};
use crate::{Cache, CacheNamespace, Namespace, SetError, Unavailable};

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

#[derive(Clone, Copy, enum_map::Enum)]
pub(super) enum Operation {
    Get,
    Set,
    Delete,
}

impl Operation {
    pub(super) fn redis_name(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Set => "SET",
            Self::Delete => "DEL",
        }
    }

    fn is_read(self) -> bool {
        match self {
            Self::Get => true,
            Self::Set | Self::Delete => false,
        }
    }
}

impl CacheNamespace {
    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Unavailable> {
        self.get_bounded(key, &self.standalone_command_scope())
            .await
    }

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

    pub async fn set(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), SetError> {
        self.set_bounded(key, value, ttl, &self.standalone_command_scope())
            .await
    }

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

    pub async fn delete(&self, key: &str) -> Result<(), Unavailable> {
        self.delete_bounded(key, &self.standalone_command_scope())
            .await
    }

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

    fn command_scope(&self, context: &OperationContext) -> CommandScope {
        CommandScope::under(context, self.cache.link.command_timeout)
    }

    fn standalone_command_scope(&self) -> CommandScope {
        CommandScope::standalone(self.cache.link.command_timeout)
    }

    async fn run<T: redis::FromRedisValue + Send + 'static>(
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
        match self.cache.link.command(command, scope).await {
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
}

impl redis::ToRedisArgs for NamespacedKey<'_> {
    fn write_redis_args<W: redis::RedisWrite + ?Sized>(&self, out: &mut W) {
        out.write_arg_fmt(format_args!("{}:{}", self.namespace, self.key));
    }
}
impl redis::ToSingleRedisArg for NamespacedKey<'_> {}
