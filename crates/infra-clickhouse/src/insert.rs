use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use operation_context::OperationContext;

use crate::error::{
    FailureReason, InsertBatchSize, InsertError, InsertFailure, ObservedFailure, UnknownOutcome,
    classify_post_dispatch_insert_error, is_local_serialization_error, is_schema_drift_error,
};
use crate::metadata::{self, SchemaGen};
use crate::observe::{InsertModeLabel, Operation};
use crate::{
    AdmitFailure, ClickHouse, QuerySettings, Shared, apply_client_budget_settings,
    format_execution_timeout_secs, within_bounded,
};

const MAX_TABLE_BYTES: usize = 128;
const MAX_DEDUP_TOKEN_BYTES: usize = 256;

#[derive(Clone, Debug, Default)]
pub struct InsertOutcomes(Arc<AtomicUsize>);

impl InsertOutcomes {
    #[must_use]
    pub fn has_unconfirmed_write(&self) -> bool {
        self.0.load(Ordering::Acquire) != 0
    }

    #[must_use]
    pub(crate) fn begin_dispatch(&self) -> DispatchedInsert<'_> {
        self.0.fetch_add(1, Ordering::Release);
        DispatchedInsert(self)
    }
}

pub(crate) struct DispatchedInsert<'a>(&'a InsertOutcomes);

impl DispatchedInsert<'_> {
    pub(crate) fn settle(self) {
        self.0.0.fetch_sub(1, Ordering::Release);
    }
}

pub trait WriteRow<'a>: clickhouse::RowWrite + std::borrow::Borrow<Self::Value<'a>> + Sync {}

impl<'a, R> WriteRow<'a> for R where
    R: clickhouse::RowWrite + std::borrow::Borrow<R::Value<'a>> + Sync
{
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AsyncDeduplication<'a> {
    Disabled,
    ByPayloadHash,
    ByToken(&'a str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InsertMode<'a> {
    Batch { dedup_token: Option<&'a str> },
    Async { dedup: AsyncDeduplication<'a> },
}

#[must_use]
#[derive(Clone, Copy, Debug)]
pub struct InsertOptions<'a> {
    mode: InsertMode<'a>,
    timeout: Option<Duration>,
    outcomes: Option<&'a InsertOutcomes>,
}

impl<'a> InsertOptions<'a> {
    const fn new(mode: InsertMode<'a>) -> Self {
        Self {
            mode,
            timeout: None,
            outcomes: None,
        }
    }

    pub const fn batch() -> Self {
        Self::new(InsertMode::Batch { dedup_token: None })
    }

    pub const fn batch_with_token(token: &'a str) -> Self {
        Self::new(InsertMode::Batch {
            dedup_token: Some(token),
        })
    }

    pub const fn async_with_token(token: &'a str) -> Self {
        Self::new(InsertMode::Async {
            dedup: AsyncDeduplication::ByToken(token),
        })
    }

    pub const fn async_with_payload_hash() -> Self {
        Self::new(InsertMode::Async {
            dedup: AsyncDeduplication::ByPayloadHash,
        })
    }

    pub const fn async_without_deduplication() -> Self {
        Self::new(InsertMode::Async {
            dedup: AsyncDeduplication::Disabled,
        })
    }

    pub const fn timeout(mut self, limit: Duration) -> Self {
        self.timeout = Some(limit);
        self
    }

    pub const fn observe_outcomes(mut self, outcomes: &'a InsertOutcomes) -> Self {
        self.outcomes = Some(outcomes);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InsertReport {
    pub rows: u64,
}

impl ClickHouse {
    pub async fn insert<'r, R: WriteRow<'r>>(
        &self,
        ctx: &OperationContext,
        summary: &'static str,
        table: &str,
        rows: &[R],
        options: InsertOptions<'_>,
    ) -> Result<InsertReport, InsertError> {
        let shared = &self.inner.shared;
        if shared.is_closing() {
            return Err(InsertError::NotApplied(InsertFailure::Unavailable));
        }
        ctx.check()
            .map_err(|stopped| InsertError::NotApplied(InsertFailure::Stopped(stopped)))?;
        validate_insert_inputs(table, options)?;
        if rows.is_empty() {
            return Ok(InsertReport { rows: 0 });
        }

        let (operation, mode_label) = match options.mode {
            InsertMode::Batch { .. } => (Operation::InsertBatch, InsertModeLabel::Batch),
            InsertMode::Async { .. } => (Operation::InsertAsync, InsertModeLabel::Async),
        };
        let mut admitted = shared
            .admit_operation(operation, summary)
            .map_err(|failure| match failure {
                AdmitFailure::Busy => InsertError::NotApplied(InsertFailure::Busy),
                AdmitFailure::Closing => InsertError::NotApplied(InsertFailure::Unavailable),
            })?;
        let result = async {
            let effective_timeout = shared
                .policy
                .insert_timeout
                .min(options.timeout.unwrap_or(shared.policy.insert_timeout));
            let bounded = ctx.bounded(effective_timeout);
            bounded
                .check()
                .map_err(ObservedFailure::pre_dispatch_stop)?;

            let query_id = uuid::Uuid::new_v4().to_string();
            admitted.guard_mut().record_query_id(query_id.clone());

            let creds_snap = shared.credentials.load();
            let slot =
                shared
                    .table_slots
                    .get_or_create(table, &creds_snap, &shared.endpoint.database);
            let (insert, observed_gen) = metadata::prepare_insert::<R>(
                &slot,
                table,
                &query_id,
                shared,
                admitted.guard(),
                &bounded,
            )
            .await?;

            bounded
                .check()
                .map_err(ObservedFailure::pre_dispatch_stop)?;
            let insert = configure_insert(insert, shared, options.mode, bounded.remaining());
            admitted.guard_mut().mark_dispatched();
            let dispatched = options.outcomes.map(InsertOutcomes::begin_dispatch);
            let outcome =
                write_and_end_insert(insert, rows, &bounded, &slot, observed_gen, shared).await;
            if let Some(inflight) = dispatched
                && !matches!(
                    outcome,
                    Err(ObservedFailure {
                        error: InsertError::OutcomeUnknown(_),
                        ..
                    })
                )
            {
                inflight.settle();
            }
            outcome
        }
        .await;
        admitted.finish_insert(mode_label, result)
    }
}

fn configure_insert<R: clickhouse::Row>(
    mut insert: clickhouse::insert::Insert<R>,
    shared: &Shared,
    mode: InsertMode<'_>,
    remaining: Duration,
) -> clickhouse::insert::Insert<R> {
    let client_timeout = match shared.policy.query_settings {
        QuerySettings::Client { max_memory_usage } => {
            let timeout_secs = format_execution_timeout_secs(remaining);
            insert = apply_client_budget_settings(
                insert,
                max_memory_usage,
                &timeout_secs,
                |insert, key, value| insert.with_setting(key, value),
            );
            Some(timeout_secs)
        }
        QuerySettings::Server => None,
    };
    // ClickHouse 26.2+ deduplicate_insert setting
    match mode {
        InsertMode::Batch { dedup_token } => {
            insert = insert.with_setting("async_insert", "0");
            if let Some(token) = dedup_token {
                insert = insert
                    .with_setting("deduplicate_insert", "enable")
                    .with_setting("insert_deduplication_token", token);
            }
        }
        InsertMode::Async { dedup } => {
            insert = insert
                .with_setting("async_insert", "1")
                .with_setting("wait_for_async_insert", "1");
            if let Some(timeout_secs) = client_timeout {
                insert = insert.with_setting("wait_for_async_insert_timeout", timeout_secs);
            }
            match dedup {
                AsyncDeduplication::Disabled => {
                    insert = insert.with_setting("deduplicate_insert", "disable");
                }
                AsyncDeduplication::ByPayloadHash => {
                    insert = insert.with_setting("deduplicate_insert", "enable");
                }
                AsyncDeduplication::ByToken(token) => {
                    insert = insert
                        .with_setting("deduplicate_insert", "enable")
                        .with_setting("insert_deduplication_token", token);
                }
            }
        }
    }
    insert
}

async fn write_and_end_insert<'r, R: WriteRow<'r>>(
    mut insert: clickhouse::insert::Insert<R>,
    rows: &[R],
    bounded: &operation_context::BoundedContext,
    slot: &metadata::TableSlot,
    observed_gen: SchemaGen,
    shared: &Shared,
) -> Result<InsertReport, ObservedFailure<InsertError>> {
    let batch_size = InsertBatchSize::from_len(rows.len());
    for (idx, row) in rows.iter().enumerate() {
        let value: &R::Value<'r> = std::borrow::Borrow::borrow(row);
        match within_bounded(bounded, insert.write(value)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                if is_schema_drift_error(&err) {
                    slot.invalidate_if_gen(
                        observed_gen,
                        &shared.credentials.load(),
                        &shared.endpoint.database,
                    );
                }
                if is_local_serialization_error(&err) {
                    let error = if idx == 0 {
                        InsertError::NotApplied(InsertFailure::Encode)
                    } else {
                        InsertError::OutcomeUnknown(UnknownOutcome::Unavailable)
                    };
                    return Err(ObservedFailure::with_reason(error, FailureReason::Encode));
                }
                if let Some(stop) = bounded.stopped() {
                    return Err(ObservedFailure::post_dispatch_stop(stop));
                }
                return Err(classify_post_dispatch_insert_error(&err, batch_size));
            }
            Err(stop) => return Err(ObservedFailure::post_dispatch_stop(stop)),
        }
    }

    match within_bounded(bounded, insert.end()).await {
        Ok(Ok(())) => Ok(InsertReport {
            rows: u64::try_from(rows.len()).unwrap_or(u64::MAX),
        }),
        Ok(Err(err)) => {
            if is_schema_drift_error(&err) {
                slot.invalidate_if_gen(
                    observed_gen,
                    &shared.credentials.load(),
                    &shared.endpoint.database,
                );
            }
            if let Some(stop) = bounded.stopped() {
                return Err(ObservedFailure::post_dispatch_stop(stop));
            }
            Err(classify_post_dispatch_insert_error(&err, batch_size))
        }
        Err(stop) => Err(ObservedFailure::post_dispatch_stop(stop)),
    }
}

fn validate_insert_inputs(table: &str, options: InsertOptions<'_>) -> Result<(), InsertError> {
    if !is_valid_table_name(table) || options.timeout.is_some_and(|limit| limit.is_zero()) {
        return Err(InsertError::NotApplied(InsertFailure::InvalidInput));
    }
    let token = match options.mode {
        InsertMode::Batch { dedup_token } => dedup_token,
        InsertMode::Async {
            dedup: AsyncDeduplication::ByToken(token),
        } => Some(token),
        InsertMode::Async { .. } => None,
    };
    if let Some(tok) = token
        && (tok.is_empty()
            || tok.len() > MAX_DEDUP_TOKEN_BYTES
            || shared_rules::rfc9110::has_ascii_control(tok.as_bytes()))
    {
        return Err(InsertError::NotApplied(InsertFailure::InvalidInput));
    }
    Ok(())
}

fn is_valid_table_name(table: &str) -> bool {
    !table.is_empty()
        && table.len() <= MAX_TABLE_BYTES
        && !shared_rules::rfc9110::has_ascii_control(table.as_bytes())
        && !table
            .bytes()
            .any(|b| b.is_ascii_whitespace() || matches!(b, b'.' | b'`' | b'\'' | b'"' | b';'))
}
