//! Transactional `JetStream` publication through the canonical jobs queue.
//!
//! This module stores the already serialized event bytes with its immutable
//! route and identity. Jobs owns every queue statement and the caller owns the
//! transaction outcome; this adapter only maps immutable intent to a jobs kind.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use infra_jobs::{
    EnqueueOptions, Enqueued, Job, JobError, JobKind, Kinds, LivePayloadComparison, Policy,
    Registry, compare_live_payload, enqueue,
};
use infra_postgres::Tx;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;

use crate::prepared::PreparedEvent;
use crate::wire::{encode_prepared, valid_subject};
use crate::{Producer, PublishError};

const OUTBOX_KIND: &str = "publish_domain_event";
const FORMAT_VERSION: u8 = 1;
const OUTAGE_DELAY: Duration = Duration::from_secs(30);

/// Policy for an acknowledged publication attempt.
const POLICY: Policy = Policy {
    max_attempts: 25,
    timeout: Duration::from_secs(30),
};

/// The durable enqueue outcome for an immutable event intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxEnqueued {
    /// A new live jobs row holds this intent.
    Created,
    /// The same immutable intent already has a live jobs row.
    Duplicate,
}

/// Why a prepared event could not establish its durable publication intent.
#[derive(Debug, thiserror::Error)]
pub enum OutboxEnqueueError {
    /// Exact bytes exceed the available encoded jobs payload capacity.
    #[error("outbox payload is {bytes} bytes; the immutable intent allows {max_bytes}")]
    PayloadTooLarge {
        /// Bytes in the prepared JSON payload.
        bytes: usize,
        /// Largest raw JSON payload that this event's immutable metadata admits.
        max_bytes: usize,
    },
    /// A live row with this event identity contains different immutable intent.
    #[error("outbox event identity conflicts with a live immutable intent")]
    EventIdConflict,
    /// The duplicate row disappeared before it could be compared.
    ///
    /// The caller's transaction/retry owner must retry the same prepared event.
    #[error("outbox live identity changed; retry the caller transaction")]
    LiveIdentityChanged,
    /// Canonical jobs validation or persistence refused the enqueue.
    #[error(transparent)]
    Jobs(#[from] infra_jobs::EnqueueError),
}

/// Register the sole outbox job kind for a dedicated one-slot publisher engine.
///
/// The caller owns engine construction, pool admission, and lifecycle. This
/// helper only creates the checked registry consumed by that engine.
///
/// # Errors
///
/// Returns a jobs registry error when the fixed kind or policy is invalid.
pub fn registry(producer: Producer) -> Result<Registry, infra_jobs::KindError> {
    let publisher = Arc::new(Publisher { producer });
    let mut kinds = Kinds::new();
    kinds.register(POLICY, move |job| {
        let publisher = Arc::clone(&publisher);
        async move { publisher.publish(job).await }
    });
    kinds.validate()
}

impl PreparedEvent {
    /// Largest raw JSON payload this event's immutable jobs representation admits.
    ///
    /// This is smaller than the jobs JSON limit because it includes the stable
    /// routing and identity fields plus standard padded base64 storage.
    ///
    /// # Errors
    ///
    /// Returns the canonical jobs serialization error if immutable metadata
    /// cannot be represented as a jobs payload.
    pub fn outbox_payload_limit(&self) -> Result<usize, OutboxEnqueueError> {
        max_payload_bytes(&PublishDomainEvent::from(self)).map_err(OutboxEnqueueError::from)
    }

    /// Enqueues this immutable event inside the caller's already-open transaction.
    ///
    /// The method never creates a connection or commits. A duplicate live key
    /// is accepted only when the jobs owner confirms byte-for-byte equal intent;
    /// a vanished live key returns the caller to its existing transaction retry
    /// policy.
    ///
    /// # Errors
    ///
    /// Returns a conflict for different live immutable content, a retryable
    /// contention result when no live row remains to compare, or the canonical
    /// jobs enqueue error.
    pub async fn enqueue(&self, tx: &mut Tx<'_>) -> Result<OutboxEnqueued, OutboxEnqueueError> {
        let intent = PublishDomainEvent::from(self);
        let max_bytes = max_payload_bytes(&intent)?;
        if self.payload.len() > max_bytes {
            return Err(OutboxEnqueueError::PayloadTooLarge {
                bytes: self.payload.len(),
                max_bytes,
            });
        }
        let key = event_key(&intent.message_id);
        match enqueue(
            tx,
            &intent,
            EnqueueOptions {
                delay: Duration::ZERO,
                unique_key: Some(&key),
            },
        )
        .await?
        {
            Enqueued::Created(_) => Ok(OutboxEnqueued::Created),
            Enqueued::Duplicate => match compare_live_payload(tx, &key, &intent).await? {
                LivePayloadComparison::Same => Ok(OutboxEnqueued::Duplicate),
                LivePayloadComparison::Different => Err(OutboxEnqueueError::EventIdConflict),
                LivePayloadComparison::NoLongerLive => Err(OutboxEnqueueError::LiveIdentityChanged),
            },
        }
    }
}

struct Publisher {
    producer: Producer,
}

impl Publisher {
    async fn publish(&self, job: Job<PublishDomainEvent>) -> Result<(), JobError> {
        let event = job
            .payload()
            .prepared()
            .map_err(|_| JobError::permanent("outbox immutable intent is invalid"))?;
        match self
            .producer
            .publish(&event, job.deadline(), &job.cancellation())
            .await
        {
            Ok(_) => Ok(()),
            Err(PublishError::Rejected | PublishError::Ambiguous) => {
                let error = JobError::snooze(OUTAGE_DELAY).map_err(JobError::from)?;
                Err(error)
            }
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct PublishDomainEvent {
    version: u8,
    subject: String,
    message_id: String,
    publication_id: String,
    event_type: String,
    schema_version: u16,
    occurred_at_unix_seconds: i64,
    occurred_at_nanosecond: u32,
    payload_base64: String,
}

impl JobKind for PublishDomainEvent {
    const NAME: &'static str = OUTBOX_KIND;
}

impl From<&PreparedEvent> for PublishDomainEvent {
    fn from(event: &PreparedEvent) -> Self {
        Self {
            version: FORMAT_VERSION,
            subject: event.subject.clone(),
            message_id: event.message_id.clone(),
            publication_id: event.publication_id.clone(),
            event_type: event.event_type.clone(),
            schema_version: event.schema_version,
            occurred_at_unix_seconds: event.occurred_at.unix_timestamp(),
            occurred_at_nanosecond: event.occurred_at.nanosecond(),
            payload_base64: STANDARD.encode(&event.payload),
        }
    }
}

impl PublishDomainEvent {
    fn prepared(&self) -> Result<PreparedEvent, StoredIntentError> {
        if self.version != FORMAT_VERSION || !valid_subject(&self.subject) {
            return Err(StoredIntentError);
        }
        let occurred_at = OffsetDateTime::from_unix_timestamp(self.occurred_at_unix_seconds)
            .ok()
            .and_then(|value| value.replace_nanosecond(self.occurred_at_nanosecond).ok())
            .ok_or(StoredIntentError)?;
        let payload = STANDARD
            .decode(&self.payload_base64)
            .map(Bytes::from)
            .map_err(|_| StoredIntentError)?;
        serde_json::from_slice::<serde_json::Value>(&payload).map_err(|_| StoredIntentError)?;
        let event = PreparedEvent {
            subject: self.subject.clone(),
            message_id: self.message_id.clone(),
            publication_id: self.publication_id.clone(),
            event_type: self.event_type.clone(),
            schema_version: self.schema_version,
            occurred_at,
            payload,
        };
        encode_prepared(&event).map_err(|_| StoredIntentError)?;
        Ok(event)
    }
}

#[derive(Clone, Copy, Debug)]
struct StoredIntentError;

fn event_key(message_id: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut key = String::with_capacity(70);
    key.push_str("event-");
    for byte in Sha256::digest(message_id.as_bytes()) {
        key.push(char::from(HEX[usize::from(byte >> 4)]));
        key.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    key
}

fn max_payload_bytes(intent: &PublishDomainEvent) -> Result<usize, infra_jobs::EnqueueError> {
    let mut metadata = intent.clone();
    metadata.payload_base64.clear();
    let overhead = serde_json::to_vec(&metadata)
        .map_err(infra_jobs::EnqueueError::Serialize)?
        .len();
    let available = infra_jobs::MAX_PAYLOAD_BYTES.saturating_sub(overhead);
    Ok((available / 4) * 3)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use bytes::Bytes;
    use time::OffsetDateTime;

    use super::{FORMAT_VERSION, PublishDomainEvent, event_key, max_payload_bytes};
    use crate::prepared::PreparedEvent;

    fn prepared(payload: Bytes) -> PreparedEvent {
        PreparedEvent {
            subject: "events.created".to_owned(),
            message_id: "logical-id".to_owned(),
            publication_id: "logical-id".to_owned(),
            event_type: "example.created".to_owned(),
            schema_version: 1,
            occurred_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            payload,
        }
    }

    #[test]
    fn storage_preserves_exact_padded_base64_payload_bytes() {
        let event = prepared(Bytes::from_static(br#"{"escaped":"\\\\u0000","n":1.0}"#));
        let stored = PublishDomainEvent::from(&event);

        assert_eq!(stored.version, FORMAT_VERSION);
        assert_eq!(
            stored.payload_base64,
            "eyJlc2NhcGVkIjoiXFxcXHUwMDAwIiwibiI6MS4wfQ=="
        );
        let restored = stored.prepared().expect("stored immutable intent is valid");
        assert_eq!(restored.payload(), event.payload());
        assert_eq!(restored.subject(), event.subject());
        assert_eq!(restored.message_id(), event.message_id());
        assert_eq!(restored.publication_id(), event.publication_id());
    }

    #[test]
    fn stored_intent_rejects_invalid_base64_before_broker_dispatch() {
        let mut stored = PublishDomainEvent::from(&prepared(Bytes::from_static(br#"{}"#)));
        stored.payload_base64 = "not-base64".to_owned();

        assert!(stored.prepared().is_err());
    }

    #[test]
    fn event_key_hashes_the_full_logical_identity() {
        assert_eq!(
            event_key("logical-id"),
            "event-4f63c63f6201a9f96cf5efafede950d7bb7d4c83edfff141074bfbf9622e1d03"
        );
    }

    #[test]
    fn storage_preserves_distinct_redrive_publication_identity() {
        let mut event = prepared(Bytes::from_static(br#"{}"#));
        event.publication_id = "redrive-8e51b5a19b7924ce".to_owned();

        let restored = PublishDomainEvent::from(&event)
            .prepared()
            .expect("stored redrive intent is valid");

        assert_eq!(restored.message_id(), "logical-id");
        assert_eq!(restored.publication_id(), "redrive-8e51b5a19b7924ce");
    }

    #[test]
    fn payload_limit_fits_exact_base64_boundary_for_varied_metadata() {
        let compact = PublishDomainEvent::from(&prepared(Bytes::from_static(br#"{}"#)));
        let wide = PublishDomainEvent {
            version: FORMAT_VERSION,
            subject: "s".repeat(256),
            message_id: "i".repeat(256),
            publication_id: "p".repeat(256),
            event_type: "e".repeat(256),
            schema_version: u16::MAX,
            occurred_at_unix_seconds: 253_402_300_799,
            occurred_at_nanosecond: 999_999_999,
            payload_base64: String::new(),
        };

        for intent in [compact, wide] {
            let limit = max_payload_bytes(&intent).expect("primitive intent serializes");

            assert!(serialized_len(&intent, limit) <= infra_jobs::MAX_PAYLOAD_BYTES);
            assert!(serialized_len(&intent, limit + 1) > infra_jobs::MAX_PAYLOAD_BYTES);
        }
    }

    fn serialized_len(intent: &PublishDomainEvent, payload_bytes: usize) -> usize {
        let mut candidate = intent.clone();
        candidate.payload_base64 = STANDARD.encode(vec![0; payload_bytes]);
        serde_json::to_vec(&candidate)
            .expect("primitive intent serializes")
            .len()
    }
}
