use infra_postgres::{Tx, TxMode, observed};

use super::{EnqueueError, encode_payload, is_valid_unique_key, notify_workers_after_commit};
use crate::{
    Handler, JobId, JobKind, KindError, KindName, Kinds, KindsBuilder, Policy, trace_context,
};

pub const KIND: KindName = KindName::new("publish_domain_event");

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxEnqueued {
    Created,
    Duplicate,
}

#[derive(Debug, thiserror::Error)]
pub enum OutboxEnqueueError {
    #[error("payload contains a decoded NUL character")]
    PayloadContainsNul,
    #[error("outbox event identity conflicts with its immutable intent or business order")]
    EventIdConflict,
    #[error("outbox aggregate version is occupied or its predecessor is missing")]
    OrderConflict,
    #[error(
        "outbox order requires a 1..=255 byte key without control characters and a positive version"
    )]
    InvalidOrder,
    #[error(transparent)]
    Enqueue(#[from] EnqueueError),
}

#[derive(Debug, thiserror::Error)]
pub enum OutboxReconcileError {
    #[error("payload contains a decoded NUL character")]
    PayloadContainsNul,
    #[error("outbox event identity conflicts with its immutable intent or business order")]
    EventIdConflict,
    #[error(
        "outbox order requires a 1..=255 byte key without control characters and a positive version"
    )]
    InvalidOrder,
    #[error("outbox event has no existing receipt")]
    MissingReceipt,
    #[error(transparent)]
    Enqueue(#[from] EnqueueError),
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
) -> Result<OutboxEnqueued, OutboxEnqueueError> {
    let payload = prepare_intent(intent, order)?;
    let (aggregate_key, version) = order.unzip();
    let trace = trace_context::capture();
    let id = JobId::new_v7();
    let inserted = observed!(
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
            KIND.as_str(),
            trace.trace_parent(),
            trace.trace_state(),
        )
        .execute(&mut *tx),
    )
    .await
    .map_err(EnqueueError::from)?;
    if inserted.rows_affected() == 1 {
        notify_workers_after_commit(tx, KIND.as_str());
        return Ok(OutboxEnqueued::Created);
    }
    match compare_receipt(tx, &payload, order).await? {
        ReceiptComparison::Same => Ok(OutboxEnqueued::Duplicate),
        ReceiptComparison::Different => Err(OutboxEnqueueError::EventIdConflict),
        ReceiptComparison::Missing => Err(OutboxEnqueueError::OrderConflict),
    }
}

pub async fn reconcile<T: JobKind>(
    tx: &mut Tx<'_, impl TxMode>,
    intent: &T,
    order: Option<(&str, i64)>,
) -> Result<(), OutboxReconcileError> {
    let payload = prepare_intent(intent, order)?;
    match compare_receipt(tx, &payload, order).await? {
        ReceiptComparison::Same => Ok(()),
        ReceiptComparison::Different => Err(OutboxReconcileError::EventIdConflict),
        ReceiptComparison::Missing => Err(OutboxReconcileError::MissingReceipt),
    }
}

#[derive(Debug)]
enum PrepareIntentError {
    InvalidOrder,
    PayloadContainsNul,
    Enqueue(EnqueueError),
}

impl From<EnqueueError> for PrepareIntentError {
    fn from(error: EnqueueError) -> Self {
        Self::Enqueue(error)
    }
}

impl From<PrepareIntentError> for OutboxEnqueueError {
    fn from(error: PrepareIntentError) -> Self {
        match error {
            PrepareIntentError::InvalidOrder => Self::InvalidOrder,
            PrepareIntentError::PayloadContainsNul => Self::PayloadContainsNul,
            PrepareIntentError::Enqueue(error) => Self::Enqueue(error),
        }
    }
}

impl From<PrepareIntentError> for OutboxReconcileError {
    fn from(error: PrepareIntentError) -> Self {
        match error {
            PrepareIntentError::InvalidOrder => Self::InvalidOrder,
            PrepareIntentError::PayloadContainsNul => Self::PayloadContainsNul,
            PrepareIntentError::Enqueue(error) => Self::Enqueue(error),
        }
    }
}

fn prepare_intent<T: JobKind>(
    intent: &T,
    order: Option<(&str, i64)>,
) -> Result<String, PrepareIntentError> {
    if T::NAME != KIND {
        return Err(EnqueueError::OutboxKind.into());
    }
    if order.is_some_and(|(key, version)| !is_valid_unique_key(key) || version < 1) {
        return Err(PrepareIntentError::InvalidOrder);
    }
    let payload = encode_payload(intent)?;
    if contains_decoded_nul(&payload) {
        return Err(PrepareIntentError::PayloadContainsNul);
    }
    serde_json::from_str::<T>(&payload).map_err(EnqueueError::PayloadDoesNotDecode)?;
    Ok(payload)
}

fn contains_decoded_nul(payload: &str) -> bool {
    static NUL_ESCAPE: std::sync::LazyLock<memchr::memmem::Finder<'static>> =
        std::sync::LazyLock::new(|| memchr::memmem::Finder::new(br"\u0000"));
    let bytes = payload.as_bytes();
    NUL_ESCAPE.find_iter(bytes).any(|offset| {
        let preceding_backslashes = bytes[..offset]
            .iter()
            .rev()
            .take_while(|&&byte| byte == b'\\')
            .count();
        preceding_backslashes.is_multiple_of(2)
    })
}

enum ReceiptComparison {
    Same,
    Different,
    Missing,
}

async fn compare_receipt(
    tx: &mut Tx<'_, impl TxMode>,
    payload: &str,
    order: Option<(&str, i64)>,
) -> Result<ReceiptComparison, EnqueueError> {
    let (aggregate_key, version) = order.unzip();
    let same = observed!(
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
        Some(true) => ReceiptComparison::Same,
        Some(false) => ReceiptComparison::Different,
        None => ReceiptComparison::Missing,
    })
}

#[cfg(test)]
mod tests {
    use super::{JobKind, KIND, KindName, PrepareIntentError, prepare_intent};

    #[test]
    fn outbox_admission_distinguishes_literal_escapes_from_nul() {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Intent(String);

        impl JobKind for Intent {
            const NAME: KindName = KIND;
        }

        for prefix in ["", "\\", "\\\\"] {
            let literal = Intent(format!(r"{prefix}\u0000"));
            assert!(prepare_intent(&literal, None).is_ok());
            let nul = Intent(format!("\\u0000{prefix}\0"));
            std::assert_matches!(
                prepare_intent(&nul, None).err(),
                Some(PrepareIntentError::PayloadContainsNul)
            );
        }
    }
}
