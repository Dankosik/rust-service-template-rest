use std::sync::atomic::Ordering;
use std::time::Duration;

use operation_context::{BoundedStop, OperationContext};

use crate::error::{
    InsertError, InsertFailure, UnknownOutcome, classify_metadata_error,
    classify_post_dispatch_insert_error, is_local_serialization_error,
};
use crate::{ClickHouse, within_bounded};

const MAX_TABLE_BYTES: usize = 128;
const MAX_DEDUP_TOKEN_BYTES: usize = 256;

mod sealed {
    pub trait WriteRowSealed {}
    impl<R> WriteRowSealed for R where R: clickhouse::RowOwned + clickhouse::RowWrite + Send + Sync {}
}

pub trait WriteRow:
    sealed::WriteRowSealed + clickhouse::RowOwned + clickhouse::RowWrite + Send + Sync
{
}

impl<R> WriteRow for R where R: clickhouse::RowOwned + clickhouse::RowWrite + Send + Sync {}

#[must_use]
#[derive(Clone, Copy, Debug, Default)]
pub struct BatchInsertOptions<'a> {
    timeout: Option<Duration>,
    deduplication_token: Option<&'a str>,
}

impl<'a> BatchInsertOptions<'a> {
    pub const fn new() -> Self {
        Self {
            timeout: None,
            deduplication_token: None,
        }
    }

    pub const fn timeout(mut self, limit: Duration) -> Self {
        self.timeout = Some(limit);
        self
    }

    pub const fn deduplication_token(mut self, token: &'a str) -> Self {
        self.deduplication_token = Some(token);
        self
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AsyncDeduplication<'a> {
    #[default]
    Disabled,
    ByPayloadHash,
    ByToken(&'a str),
}

#[must_use]
#[derive(Clone, Copy, Debug, Default)]
pub struct AsyncInsertOptions<'a> {
    timeout: Option<Duration>,
    deduplication: AsyncDeduplication<'a>,
}

impl<'a> AsyncInsertOptions<'a> {
    pub const fn new() -> Self {
        Self {
            timeout: None,
            deduplication: AsyncDeduplication::Disabled,
        }
    }

    pub const fn timeout(mut self, limit: Duration) -> Self {
        self.timeout = Some(limit);
        self
    }

    pub const fn deduplication(mut self, mode: AsyncDeduplication<'a>) -> Self {
        self.deduplication = mode;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InsertReport {
    pub rows: u64,
}

enum InsertMode<'a> {
    Batch { dedup_token: Option<&'a str> },
    Async { dedup: AsyncDeduplication<'a> },
}

impl ClickHouse {
    pub async fn insert_batch<R: WriteRow>(
        &self,
        ctx: &OperationContext,
        summary: &'static str,
        table: &str,
        rows: &[R],
        options: BatchInsertOptions<'_>,
    ) -> Result<InsertReport, InsertError> {
        self.execute_insert(
            ctx,
            summary,
            table,
            rows,
            options.timeout,
            InsertMode::Batch {
                dedup_token: options.deduplication_token,
            },
        )
        .await
    }

    pub async fn insert_async<R: WriteRow>(
        &self,
        ctx: &OperationContext,
        summary: &'static str,
        table: &str,
        rows: &[R],
        options: AsyncInsertOptions<'_>,
    ) -> Result<InsertReport, InsertError> {
        self.execute_insert(
            ctx,
            summary,
            table,
            rows,
            options.timeout,
            InsertMode::Async {
                dedup: options.deduplication,
            },
        )
        .await
    }

    async fn execute_insert<R: WriteRow>(
        &self,
        ctx: &OperationContext,
        _summary: &'static str,
        table: &str,
        rows: &[R],
        timeout: Option<Duration>,
        mode: InsertMode<'_>,
    ) -> Result<InsertReport, InsertError> {
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(InsertError::NotApplied(InsertFailure::Unavailable));
        }
        ctx.check()
            .map_err(|stopped| InsertError::NotApplied(InsertFailure::Stopped(stopped)))?;
        validate_insert_inputs(table, timeout, &mode)?;
        if rows.is_empty() {
            return Ok(InsertReport { rows: 0 });
        }

        let _permit = self
            .inner
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| InsertError::NotApplied(InsertFailure::Busy))?;
        let _task_token = self.inner.tasks.token();

        let effective_timeout = self
            .inner
            .insert_timeout
            .min(timeout.unwrap_or(self.inner.insert_timeout));
        let bounded = ctx.bounded(effective_timeout);
        bounded.check().map_err(pre_dispatch_stop)?;

        let mut insert = within_bounded(&bounded, self.inner.base_client.insert::<R>(table))
            .await
            .map_err(pre_dispatch_stop)?
            .map_err(|err| InsertError::NotApplied(classify_metadata_error(&err)))?;

        let remaining = bounded.remaining();
        insert = insert
            .with_timeouts(Some(remaining), Some(remaining))
            .with_setting("query_id", uuid::Uuid::new_v4().to_string());

        match mode {
            InsertMode::Batch { dedup_token } => {
                insert = insert.with_setting("async_insert", "0");
                if let Some(token) = dedup_token {
                    insert = insert.with_setting("insert_deduplication_token", token);
                }
            }
            InsertMode::Async { dedup } => {
                insert = insert
                    .with_setting("async_insert", "1")
                    .with_setting("wait_for_async_insert", "1");
                match dedup {
                    AsyncDeduplication::Disabled => {}
                    AsyncDeduplication::ByPayloadHash => {
                        insert = insert.with_setting("async_insert_deduplicate", "1");
                    }
                    AsyncDeduplication::ByToken(token) => {
                        insert = insert
                            .with_setting("async_insert_deduplicate", "1")
                            .with_setting("insert_deduplication_token", token);
                    }
                }
            }
        }

        for (idx, row) in rows.iter().enumerate() {
            match within_bounded(&bounded, insert.write(row)).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    if idx == 0 && is_local_serialization_error(&err) {
                        return Err(InsertError::NotApplied(InsertFailure::Encode));
                    }
                    return Err(classify_post_dispatch_insert_error(&err));
                }
                Err(stop) => return Err(post_dispatch_stop(stop)),
            }
        }

        match within_bounded(&bounded, insert.end()).await {
            Ok(Ok(())) => Ok(InsertReport {
                rows: u64::try_from(rows.len()).unwrap_or(u64::MAX),
            }),
            Ok(Err(err)) => Err(classify_post_dispatch_insert_error(&err)),
            Err(stop) => Err(post_dispatch_stop(stop)),
        }
    }
}

fn validate_insert_inputs(
    table: &str,
    timeout: Option<Duration>,
    mode: &InsertMode<'_>,
) -> Result<(), InsertError> {
    if !is_valid_table_name(table) || timeout.is_some_and(|limit| limit.is_zero()) {
        return Err(InsertError::NotApplied(InsertFailure::InvalidInput));
    }
    let token = match mode {
        InsertMode::Batch { dedup_token } => *dedup_token,
        InsertMode::Async {
            dedup: AsyncDeduplication::ByToken(token),
        } => Some(*token),
        InsertMode::Async { .. } => None,
    };
    if let Some(tok) = token
        && (tok.is_empty()
            || tok.len() > MAX_DEDUP_TOKEN_BYTES
            || tok.bytes().any(|b| b < 0x20 || b == 0x7f))
    {
        return Err(InsertError::NotApplied(InsertFailure::InvalidInput));
    }
    Ok(())
}

fn is_valid_table_name(table: &str) -> bool {
    !table.is_empty()
        && table.len() <= MAX_TABLE_BYTES
        && !table.bytes().any(|b| {
            b.is_ascii_whitespace()
                || b < 0x20
                || b == 0x7f
                || matches!(b, b'.' | b'`' | b'\'' | b'"' | b';')
        })
}

const fn pre_dispatch_stop(stop: BoundedStop) -> InsertError {
    match stop {
        BoundedStop::Parent(stopped) => InsertError::NotApplied(InsertFailure::Stopped(stopped)),
        BoundedStop::Timeout => InsertError::NotApplied(InsertFailure::Unavailable),
    }
}

const fn post_dispatch_stop(stop: BoundedStop) -> InsertError {
    match stop {
        BoundedStop::Parent(stopped) => {
            InsertError::OutcomeUnknown(UnknownOutcome::Stopped(stopped))
        }
        BoundedStop::Timeout => InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
    }
}
