use infra_postgres::{Tx, TxMode, observed};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{
    EnqueueError, MAX_UNIQUE_KEY_BYTES, encode_payload, is_valid_unique_key,
    notify_workers_after_commit,
};
use crate::{Handler, JobId, KindError, Kinds, Policy, trace_context};

pub(crate) const KIND: &str = "publish_domain_event";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Order<'a> {
    pub aggregate_key: &'a str,
    pub version: i64,
}

impl<'a> Order<'a> {
    const fn parts(self) -> (&'a str, i64) {
        (self.aggregate_key, self.version)
    }
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxEnqueued {
    Created,
    Duplicate,
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileOutcome {
    Matched,
    Missing,
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
        "outbox order requires a 1..={MAX_UNIQUE_KEY_BYTES} byte key without control characters and a positive version"
    )]
    InvalidOrder,
    #[error(transparent)]
    Enqueue(#[from] EnqueueError),
}

/// ```compile_fail
/// const _: infra_jobs::KindName = infra_jobs::KindName::new("publish_domain_event");
/// ```
pub fn publisher_kinds<T>(policy: Policy, handler: impl Handler<T>) -> Result<Kinds, KindError>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    Kinds::outbox(policy, handler)
}

pub async fn enqueue<T>(
    tx: &mut Tx<'_, impl TxMode>,
    intent: &T,
    order: Option<Order<'_>>,
) -> Result<OutboxEnqueued, OutboxEnqueueError>
where
    T: Serialize + DeserializeOwned,
{
    let payload = prepare_intent(intent, order)?;
    let (aggregate_key, version) = order.map(Order::parts).unzip();
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
            KIND,
            trace.trace_parent(),
            trace.trace_state(),
        )
        .execute(&mut *tx),
    )
    .await
    .map_err(EnqueueError::from)?;
    if inserted.rows_affected() == 1 {
        notify_workers_after_commit(tx, KIND);
        return Ok(OutboxEnqueued::Created);
    }
    match compare_receipt(tx, &payload, order).await? {
        ReceiptComparison::Same => Ok(OutboxEnqueued::Duplicate),
        ReceiptComparison::Different => Err(OutboxEnqueueError::EventIdConflict),
        ReceiptComparison::Missing => Err(OutboxEnqueueError::OrderConflict),
    }
}

pub async fn reconcile<T>(
    tx: &mut Tx<'_, impl TxMode>,
    intent: &T,
    order: Option<Order<'_>>,
) -> Result<ReconcileOutcome, OutboxEnqueueError>
where
    T: Serialize + DeserializeOwned,
{
    let payload = prepare_intent(intent, order)?;
    match compare_receipt(tx, &payload, order).await? {
        ReceiptComparison::Same => Ok(ReconcileOutcome::Matched),
        ReceiptComparison::Different => Err(OutboxEnqueueError::EventIdConflict),
        ReceiptComparison::Missing => Ok(ReconcileOutcome::Missing),
    }
}

fn prepare_intent<T>(intent: &T, order: Option<Order<'_>>) -> Result<String, OutboxEnqueueError>
where
    T: Serialize + DeserializeOwned,
{
    if order.is_some_and(|order| !is_valid_unique_key(order.aggregate_key) || order.version < 1) {
        return Err(OutboxEnqueueError::InvalidOrder);
    }
    let payload = encode_payload(intent)?;
    if contains_decoded_nul(&payload) {
        return Err(OutboxEnqueueError::PayloadContainsNul);
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
    order: Option<Order<'_>>,
) -> Result<ReceiptComparison, EnqueueError> {
    let (aggregate_key, version) = order.map(Order::parts).unzip();
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
    use super::{KIND, OutboxEnqueueError, Policy, prepare_intent, publisher_kinds};
    use crate::{Job, KindError};

    #[test]
    fn outbox_admission_distinguishes_literal_escapes_from_nul() {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Intent(String);

        for prefix in ["", "\\", "\\\\"] {
            let literal = Intent(format!(r"{prefix}\u0000"));
            assert!(prepare_intent(&literal, None).is_ok());
            let nul = Intent(format!("\\u0000{prefix}\0"));
            std::assert_matches!(
                prepare_intent(&nul, None).err(),
                Some(OutboxEnqueueError::PayloadContainsNul)
            );
        }
    }

    #[test]
    fn publisher_kinds_builds_single_persistent_outbox_entry_and_validates_policy() {
        #[derive(serde::Deserialize)]
        struct Intent;

        let invalid = publisher_kinds(
            Policy {
                max_attempts: 0,
                ..Policy::default()
            },
            |_: Job<Intent>| async { Ok(()) },
        )
        .unwrap_err();
        assert_eq!(invalid, KindError::InvalidMaxAttempts { kind: KIND });

        let kinds = publisher_kinds(Policy::default(), |_: Job<Intent>| async { Ok(()) }).unwrap();
        assert_eq!(kinds.names().collect::<Vec<_>>(), [KIND]);
        let registered = kinds.get(KIND).unwrap();
        assert_eq!(registered.mode, crate::kind::KindMode::Outbox);
        assert!(registered.mode.is_outbox());
        assert!(!registered.mode.exhausts_transient());
    }
}
