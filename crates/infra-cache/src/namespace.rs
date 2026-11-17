use std::sync::Arc;
use std::time::Duration;

use operation_context::OperationContext;
use redis::{SetExpiry, SetOptions};

use crate::connection::CommandError;
use crate::observe::{OperationGuard, Outcome};
use crate::{Cache, CacheNamespace, MutationError, Namespace, OperationError, SetError};

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
}

impl CacheNamespace {
    pub async fn get(
        &self,
        parent: &OperationContext,
        key: &str,
    ) -> Result<Option<Vec<u8>>, OperationError> {
        let operation = Operation::Get;
        let build_command = || self.keyed_command(operation, key, ExtraCommandCapacity::default());
        self.run(
            parent,
            operation,
            build_command,
            |value: &Option<Vec<u8>>| {
                if value.is_some() {
                    Outcome::Hit
                } else {
                    Outcome::Miss
                }
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
        let build_command = || {
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
            command
        };
        self.run(parent, operation, build_command, |(): &()| Outcome::Ok)
            .await
            .map_err(Into::into)
    }

    pub async fn delete(&self, parent: &OperationContext, key: &str) -> Result<(), MutationError> {
        let operation = Operation::Delete;
        let build_command = || self.keyed_command(operation, key, ExtraCommandCapacity::default());
        self.run(parent, operation, build_command, |(): &()| Outcome::Ok)
            .await
            .map_err(Into::into)
    }

    async fn run<T: redis::FromRedisValue + Send + 'static>(
        &self,
        parent: &OperationContext,
        operation: Operation,
        build_command: impl FnOnce() -> redis::Cmd,
        outcome: fn(&T) -> Outcome,
    ) -> Result<T, CommandError> {
        let guard = OperationGuard::start(
            self.name,
            &self.histograms,
            operation,
            &self.cache.link.server,
        );
        match self
            .cache
            .link
            .command(build_command, parent, operation)
            .await
        {
            Ok(value) => {
                guard.succeed(outcome(&value));
                Ok(value)
            }
            Err(error) => {
                guard.failed(error.failure());
                Err(error)
            }
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
