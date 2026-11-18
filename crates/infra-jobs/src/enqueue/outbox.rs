use infra_postgres::{Tx, TxMode, observed};

use super::{EnqueueError, encode_payload, is_valid_unique_key, notify_workers_after_commit};
use crate::{Handler, JobId, JobKind, KindError, Kinds, KindsBuilder, Policy, trace_context};

pub const KIND: &str = "publish_domain_event";

#[must_use]
#[derive(Debug)]
pub enum Admission {
    Created,
    Duplicate,
    EventIdConflict,
    OrderConflict,
    InvalidOrder,
    MissingReceipt,
}

pub fn publisher_kinds<T: JobKind>(
    policy: Policy,
    handler: impl Handler<T>,
) -> Result<Kinds, KindError> {
    let mut kinds = KindsBuilder::new();
    kinds.register(policy, handler);
    kinds.validate_outbox()
}

pub async fn enqueue<T: JobKind>(
    tx: &mut Tx<'_, impl TxMode>,
    intent: &T,
    order: Option<(&str, i64)>,
) -> Result<Admission, EnqueueError> {
    admit(tx, intent, order, true).await
}

pub async fn reconcile<T: JobKind>(
    tx: &mut Tx<'_, impl TxMode>,
    intent: &T,
    order: Option<(&str, i64)>,
) -> Result<Admission, EnqueueError> {
    admit(tx, intent, order, false).await
}

async fn admit<T: JobKind>(
    tx: &mut Tx<'_, impl TxMode>,
    intent: &T,
    order: Option<(&str, i64)>,
    allow_insert: bool,
) -> Result<Admission, EnqueueError> {
    if T::NAME != KIND {
        return Err(EnqueueError::OutboxKind);
    }
    if order.is_some_and(|(key, version)| !is_valid_unique_key(key) || version < 1) {
        return Ok(Admission::InvalidOrder);
    }
    let payload = encode_payload(intent)?;
    if contains_decoded_nul(&payload) {
        return Err(EnqueueError::PayloadContainsNul);
    }
    serde_json::from_str::<T>(&payload).map_err(EnqueueError::PayloadDoesNotDecode)?;
    if !allow_insert {
        return compare_receipt(tx, &payload, order).await;
    }
    let (aggregate_key, version) = order.unzip();
    let trace = trace_context::capture();
    let id = JobId::new_v7();
    let inserted = observed(
        "enqueue outbox event",
        sqlx::query!(
            r#"
            WITH predecessor AS MATERIALIZED (
                SELECT (
                    SELECT job_id FROM outbox_event_receipts
                    WHERE aggregate_key = $2::text COLLATE "C" AND aggregate_version = $3::bigint - 1
                ) AS id
            ), receipt AS (
                INSERT INTO outbox_event_receipts
                    (event_key, job_id, intent_sha256, aggregate_key, aggregate_version)
                SELECT outbox_event_key($1::text::jsonb), $4, outbox_intent_sha256($1::text::jsonb), $2, $3
                FROM predecessor
                WHERE $3 IS NULL OR $3 = 1 OR predecessor.id IS NOT NULL
                ON CONFLICT DO NOTHING
                RETURNING event_key, job_id
            )
            INSERT INTO background_jobs
                (id, kind, payload_text, unique_key, not_before, trace_context, trace_state, predecessor_job_id)
            SELECT receipt.job_id, $5, $1, receipt.event_key, statement_timestamp(), $6, $7, predecessor.id
            FROM receipt CROSS JOIN predecessor
            "#,
            &payload,
            aggregate_key,
            version,
            id.0,
            KIND,
            trace.trace_parent(),
            trace.trace_state(),
        )
        .execute(&mut *tx),
    )
    .await?;
    if inserted.rows_affected() == 1 {
        notify_workers_after_commit(tx, KIND);
        return Ok(Admission::Created);
    }
    match compare_receipt(tx, &payload, order).await? {
        Admission::MissingReceipt => Ok(Admission::OrderConflict),
        other => Ok(other),
    }
}

fn contains_decoded_nul(payload: &str) -> bool {
    if !payload.contains(r"\u0000") {
        return false;
    }
    let payload = payload.as_bytes();
    let mut index = 0;
    while index < payload.len() {
        if payload[index] == b'\\' {
            if payload.get(index + 1..index + 6) == Some(b"u0000") {
                return true;
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    false
}

async fn compare_receipt(
    tx: &mut Tx<'_, impl TxMode>,
    payload: &str,
    order: Option<(&str, i64)>,
) -> Result<Admission, EnqueueError> {
    let (aggregate_key, version) = order.unzip();
    let same = observed(
        "compare outbox receipt",
        sqlx::query_scalar!(
            r#"
            SELECT intent_sha256 = outbox_intent_sha256($1::text::jsonb)
               AND aggregate_key IS NOT DISTINCT FROM $2::text COLLATE "C"
               AND aggregate_version IS NOT DISTINCT FROM $3::bigint AS "same!"
            FROM outbox_event_receipts WHERE event_key = outbox_event_key($1::text::jsonb)
            "#,
            payload,
            aggregate_key,
            version,
        )
        .fetch_optional(&mut *tx),
    )
    .await?;
    Ok(match same {
        Some(true) => Admission::Duplicate,
        Some(false) => Admission::EventIdConflict,
        None => Admission::MissingReceipt,
    })
}
