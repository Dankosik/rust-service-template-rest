use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use operation_context::{BoundedStop, OperationContext};

use crate::error::{
    FailureReason, InsertError, InsertFailure, ObservedFailure, UnknownOutcome,
    bounded_stop_reason, classify_post_dispatch_insert_error, is_local_serialization_error,
};
use crate::metadata::{self, is_schema_drift_error};
use crate::observe::{InsertModeLabel, Operation, OperationGuard};
use crate::{ClickHouse, within_bounded};

const MAX_TABLE_BYTES: usize = 128;
const MAX_DEDUP_TOKEN_BYTES: usize = 256;

#[derive(Clone, Debug, Default)]
pub struct InsertOutcomes(Arc<AtomicUsize>);

impl InsertOutcomes {
    #[must_use]
    pub fn has_unconfirmed_write(&self) -> bool {
        self.0.load(Ordering::Acquire) != 0
    }

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
    outcomes: Option<&'a InsertOutcomes>,
}

impl<'a> BatchInsertOptions<'a> {
    pub const fn new() -> Self {
        Self {
            timeout: None,
            deduplication_token: None,
            outcomes: None,
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

    pub const fn observe_outcomes(mut self, outcomes: &'a InsertOutcomes) -> Self {
        self.outcomes = Some(outcomes);
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
    outcomes: Option<&'a InsertOutcomes>,
}

impl<'a> AsyncInsertOptions<'a> {
    pub const fn new() -> Self {
        Self {
            timeout: None,
            deduplication: AsyncDeduplication::Disabled,
            outcomes: None,
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

    pub const fn observe_outcomes(mut self, outcomes: &'a InsertOutcomes) -> Self {
        self.outcomes = Some(outcomes);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InsertReport {
    pub rows: u64,
}

#[derive(Clone, Copy)]
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
            options.outcomes,
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
            options.outcomes,
            InsertMode::Async {
                dedup: options.deduplication,
            },
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "internal helper carries validated insert dispatch options without adding a wrapper struct"
    )]
    async fn execute_insert<R: WriteRow>(
        &self,
        ctx: &OperationContext,
        summary: &'static str,
        table: &str,
        rows: &[R],
        timeout: Option<Duration>,
        outcomes: Option<&InsertOutcomes>,
        mode: InsertMode<'_>,
    ) -> Result<InsertReport, InsertError> {
        let shared = &self.inner.shared;
        if shared.closing.load(Ordering::Acquire) {
            return Err(InsertError::NotApplied(InsertFailure::Unavailable));
        }
        ctx.check()
            .map_err(|stopped| InsertError::NotApplied(InsertFailure::Stopped(stopped)))?;
        validate_insert_inputs(table, timeout, &mode)?;
        if rows.is_empty() {
            return Ok(InsertReport { rows: 0 });
        }

        let (operation, mode_label) = match mode {
            InsertMode::Batch { .. } => (Operation::InsertBatch, InsertModeLabel::Batch),
            InsertMode::Async { .. } => (Operation::InsertAsync, InsertModeLabel::Async),
        };
        let Ok(_permit) = shared.permits.clone().try_acquire_owned() else {
            shared.metrics.record_admission_refused(operation);
            return Err(InsertError::NotApplied(InsertFailure::Busy));
        };
        let _task_token = shared.tasks.token();
        if shared.closing.load(Ordering::Acquire) || shared.tasks.is_closed() {
            return Err(InsertError::NotApplied(InsertFailure::Unavailable));
        }

        let mut guard =
            OperationGuard::start(&shared.metrics, operation, summary, &shared.endpoint);
        let result = async {
            let effective_timeout = shared
                .insert_timeout
                .min(timeout.unwrap_or(shared.insert_timeout));
            let bounded = ctx.bounded(effective_timeout);
            bounded.check().map_err(pre_dispatch_stop)?;

            let creds_snap = shared.credentials.load();
            let slot =
                shared
                    .table_slots
                    .get_or_create(table, &creds_snap, &shared.endpoint.database);
            let (insert, observed_gen) = metadata::prepare_insert::<R>(
                &slot,
                table,
                &shared.credentials,
                &guard,
                shared.propagate_trace_context,
                &bounded,
            )
            .await?;

            bounded.check().map_err(pre_dispatch_stop)?;
            let (insert, query_id) = configure_insert(insert, mode);
            guard.record_query_id(query_id);
            guard.mark_dispatched();
            let dispatched = outcomes.map(InsertOutcomes::begin_dispatch);
            let outcome = write_and_end_insert(
                insert,
                rows,
                &bounded,
                &slot,
                observed_gen,
                &shared.credentials,
            )
            .await;
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
        guard.finish_insert(mode_label, result)
    }
}

fn configure_insert<R: WriteRow>(
    insert: clickhouse::insert::Insert<R>,
    mode: InsertMode<'_>,
) -> (clickhouse::insert::Insert<R>, String) {
    let query_id = uuid::Uuid::new_v4().to_string();
    let mut insert = insert.with_setting("query_id", &query_id);
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
    (insert, query_id)
}

async fn write_and_end_insert<R: WriteRow>(
    mut insert: clickhouse::insert::Insert<R>,
    rows: &[R],
    bounded: &operation_context::BoundedContext,
    slot: &metadata::TableSlot,
    observed_gen: u64,
    credentials: &arc_swap::ArcSwap<crate::credentials::ActiveCredentials>,
) -> Result<InsertReport, ObservedFailure<InsertError>> {
    for (idx, row) in rows.iter().enumerate() {
        match within_bounded(bounded, insert.write(row)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                if is_schema_drift_error(&err) {
                    slot.invalidate_if_gen(observed_gen, &credentials.load());
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
                    return Err(post_dispatch_stop(stop));
                }
                return Err(classify_post_dispatch_insert_error(&err));
            }
            Err(stop) => return Err(post_dispatch_stop(stop)),
        }
    }

    match within_bounded(bounded, insert.end()).await {
        Ok(Ok(())) => Ok(InsertReport {
            rows: u64::try_from(rows.len()).unwrap_or(u64::MAX),
        }),
        Ok(Err(err)) => {
            if is_schema_drift_error(&err) {
                slot.invalidate_if_gen(observed_gen, &credentials.load());
            }
            if let Some(stop) = bounded.stopped() {
                return Err(post_dispatch_stop(stop));
            }
            Err(classify_post_dispatch_insert_error(&err))
        }
        Err(stop) => Err(post_dispatch_stop(stop)),
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

pub(crate) const fn pre_dispatch_stop(stop: BoundedStop) -> ObservedFailure<InsertError> {
    let error = match stop {
        BoundedStop::Parent(stopped) => InsertError::NotApplied(InsertFailure::Stopped(stopped)),
        BoundedStop::Timeout => InsertError::NotApplied(InsertFailure::Unavailable),
    };
    ObservedFailure::with_reason(error, bounded_stop_reason(stop))
}

const fn post_dispatch_stop(stop: BoundedStop) -> ObservedFailure<InsertError> {
    let error = match stop {
        BoundedStop::Parent(stopped) => {
            InsertError::OutcomeUnknown(UnknownOutcome::Stopped(stopped))
        }
        BoundedStop::Timeout => InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
    };
    ObservedFailure::with_reason(error, bounded_stop_reason(stop))
}
