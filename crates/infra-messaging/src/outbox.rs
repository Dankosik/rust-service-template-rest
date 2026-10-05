//! Transactional `JetStream` publication through the canonical jobs queue.
//!
//! This module stores the already serialized event bytes with its immutable
//! route and identity. Jobs owns every queue statement and the caller owns the
//! transaction outcome; this adapter only maps immutable intent to a jobs kind.

use std::future::poll_fn;
use std::sync::Arc;
use std::task::Poll;
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
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::Producer;
use crate::prepared::PreparedEvent;
use crate::wire::{prefixed_digest_hex, valid_subject, validate_prepared};

const OUTBOX_KIND: &str = "publish_domain_event";
const FORMAT_VERSION: u8 = 1;
const PREFLIGHT_BYTES_PER_POLL: usize = 4096;

/// A failed publication retries with the jobs backoff; after `max_attempts`
/// the job stays visible in the `failed` state.
const POLICY: Policy = Policy {
    max_attempts: 25,
    timeout: Duration::from_secs(30),
    max_running: None,
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
        if self.subject.len() > infra_jobs::MAX_PAYLOAD_BYTES {
            return Ok(0);
        }
        max_payload_bytes(&PublishDomainEvent::metadata(self)).map_err(OutboxEnqueueError::from)
    }

    /// Enqueues this immutable event inside the caller's already-open transaction.
    ///
    /// The method never creates a connection or commits. A duplicate live key
    /// is accepted only when the jobs owner confirms byte-for-byte equal intent;
    /// a vanished live key returns the caller to its existing transaction retry
    /// policy. Borrowed metadata is checked cooperatively before cloning or
    /// base64 construction; dropping this future during that check sends no SQL.
    /// Direct callers own finite input, concurrent-call and deadline bounds.
    ///
    /// # Errors
    ///
    /// Returns a conflict for different live immutable content, a retryable
    /// contention result when no live row remains to compare, or the canonical
    /// jobs enqueue error.
    pub async fn enqueue(&self, tx: &mut Tx<'_>) -> Result<OutboxEnqueued, OutboxEnqueueError> {
        let intent = self.prepare_outbox_intent().await?;
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

    async fn prepare_outbox_intent(&self) -> Result<PublishDomainEvent, OutboxEnqueueError> {
        // This cheap refusal preserves the raw-body error without scanning or
        // cloning a subject that cannot leave any encoded body capacity.
        if self.subject.len() > infra_jobs::MAX_PAYLOAD_BYTES && !self.payload.is_empty() {
            return Err(OutboxEnqueueError::PayloadTooLarge {
                bytes: self.payload.len(),
                max_bytes: 0,
            });
        }
        let overhead = self.outbox_metadata_bytes().await?;
        let available = infra_jobs::MAX_PAYLOAD_BYTES.saturating_sub(overhead);
        let max_bytes = (available / 4) * 3;
        if self.payload.len() > max_bytes {
            return Err(OutboxEnqueueError::PayloadTooLarge {
                bytes: self.payload.len(),
                max_bytes,
            });
        }
        // An empty body passed the raw-body check even when metadata alone
        // exceeded the jobs ceiling. Preserve that distinct canonical error.
        if overhead > infra_jobs::MAX_PAYLOAD_BYTES {
            return Err(infra_jobs::EnqueueError::PayloadTooLarge { bytes: overhead }.into());
        }
        Ok(PublishDomainEvent::from(self))
    }

    async fn outbox_metadata_bytes(&self) -> Result<usize, infra_jobs::EnqueueError> {
        let mut total = serde_json::to_vec(&PublishDomainEvent::empty_metadata(self))
            .map_err(infra_jobs::EnqueueError::Serialize)?
            .len();
        let fields = [
            self.subject.as_str(),
            self.message_id.as_str(),
            self.publication_id.as_str(),
            self.event_type.as_ref(),
        ];
        let mut field = 0;
        let mut offset = 0;
        poll_fn(|cx| {
            let mut remaining = PREFLIGHT_BYTES_PER_POLL;
            while field < fields.len() {
                let source = fields[field];
                if offset == source.len() {
                    field += 1;
                    offset = 0;
                    continue;
                }
                let mut end = source.len().min(offset + remaining);
                while !source.is_char_boundary(end) {
                    end -= 1;
                }
                if end == offset {
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                // Reuse serde's escaping, with bounded source and allocation.
                // The empty metadata already contributed the string quotes.
                let encoded = match serde_json::to_vec(&source[offset..end]) {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        return Poll::Ready(Err(infra_jobs::EnqueueError::Serialize(error)));
                    }
                };
                total += encoded.len() - 2;
                remaining -= end - offset;
                offset = end;
            }
            Poll::Ready(Ok(total))
        })
        .await
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
        // A retry keeps the publication ID. The broker deduplicates it inside
        // the stream's duplicate window; consumers deduplicate by logical ID
        // beyond it.
        self.producer
            .publish(&event, job.deadline(), &job.cancellation())
            .await
            .map(|_| ())
            .map_err(JobError::retryable)
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
            payload_base64: STANDARD.encode(&event.payload),
            ..Self::metadata(event)
        }
    }
}

impl PublishDomainEvent {
    fn metadata(event: &PreparedEvent) -> Self {
        Self {
            subject: event.subject.clone(),
            message_id: event.message_id.clone(),
            publication_id: event.publication_id.clone(),
            event_type: event.event_type.clone().into_owned(),
            ..Self::empty_metadata(event)
        }
    }

    fn empty_metadata(event: &PreparedEvent) -> Self {
        Self {
            version: FORMAT_VERSION,
            subject: String::new(),
            message_id: String::new(),
            publication_id: String::new(),
            event_type: String::new(),
            schema_version: event.schema_version,
            occurred_at_unix_seconds: event.occurred_at.unix_timestamp(),
            occurred_at_nanosecond: event.occurred_at.nanosecond(),
            payload_base64: String::new(),
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
        serde_json::from_slice::<serde::de::IgnoredAny>(&payload).map_err(|_| StoredIntentError)?;
        let mut event = PreparedEvent {
            subject: self.subject.clone(),
            message_id: self.message_id.clone(),
            publication_id: self.publication_id.clone(),
            event_type: self.event_type.clone().into(),
            schema_version: self.schema_version,
            occurred_at,
            created_at: String::new(),
            payload,
        };
        event.created_at = validate_prepared(&event).map_err(|_| StoredIntentError)?;
        Ok(event)
    }
}

#[derive(Clone, Copy, Debug)]
struct StoredIntentError;

fn event_key(message_id: &str) -> String {
    prefixed_digest_hex("event-", &Sha256::digest(message_id.as_bytes()))
}

/// Standard base64 never needs a JSON escape, so the stored payload adds
/// exactly its own length to the serialized intent.
fn max_payload_bytes(intent: &PublishDomainEvent) -> Result<usize, infra_jobs::EnqueueError> {
    let overhead = serde_json::to_vec(intent)
        .map_err(infra_jobs::EnqueueError::Serialize)?
        .len()
        - intent.payload_base64.len();
    let available = infra_jobs::MAX_PAYLOAD_BYTES.saturating_sub(overhead);
    Ok((available / 4) * 3)
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use bytes::Bytes;
    use time::OffsetDateTime;

    use super::{
        FORMAT_VERSION, OutboxEnqueueError, PREFLIGHT_BYTES_PER_POLL, PublishDomainEvent,
        event_key, max_payload_bytes,
    };
    use crate::prepared::PreparedEvent;

    fn prepared(payload: Bytes) -> PreparedEvent {
        PreparedEvent {
            subject: "events.created".to_owned(),
            message_id: "logical-id".to_owned(),
            publication_id: "logical-id".to_owned(),
            event_type: "example.created".into(),
            schema_version: 1,
            occurred_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            created_at: "2023-11-14T22:13:20Z".to_owned(),
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
        let mut stored = PublishDomainEvent::from(&prepared(Bytes::from_static(br"{}")));
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
        let mut event = prepared(Bytes::from_static(br"{}"));
        event.publication_id = "redrive-8e51b5a19b7924ce".to_owned();

        let restored = PublishDomainEvent::from(&event)
            .prepared()
            .expect("stored redrive intent is valid");

        assert_eq!(restored.message_id(), "logical-id");
        assert_eq!(restored.publication_id(), "redrive-8e51b5a19b7924ce");
    }

    #[test]
    fn payload_limit_fits_exact_base64_boundary_for_varied_metadata() {
        let compact = PublishDomainEvent::from(&prepared(Bytes::from_static(br"{}")));
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

    #[test]
    fn cooperative_preflight_preserves_escaped_metadata_and_the_exact_body_limit() {
        let mut event = prepared(Bytes::new());
        // UTF-8 crosses a fragment boundary, and serde must count both short
        // escapes and six-byte control escapes without altering stored bytes.
        event.subject = format!("{}é😀\\\"\n\u{0001}", "s".repeat(4095));
        event.message_id = "logical-\"id\\".to_owned();
        event.publication_id = "publication-😀".to_owned();
        event.event_type = "example.\tcreated".into();
        let limit = event.outbox_payload_limit().unwrap();
        event.payload = Bytes::from(vec![b'x'; limit]);
        let expected = serde_json::to_vec(&PublishDomainEvent::from(&event)).unwrap();

        let (result, polls) = complete(event.prepare_outbox_intent());
        assert!(polls > 1);
        assert_eq!(serde_json::to_vec(&result.unwrap()).unwrap(), expected);
        assert!(expected.len() <= infra_jobs::MAX_PAYLOAD_BYTES);

        event.payload = Bytes::from(vec![b'x'; limit + 1]);
        let (result, _) = complete(event.prepare_outbox_intent());
        assert!(matches!(
            result,
            Err(OutboxEnqueueError::PayloadTooLarge { bytes, max_bytes })
                if bytes == limit + 1 && max_bytes == limit
        ));
    }

    #[test]
    fn oversized_metadata_preserves_empty_and_nonempty_body_errors() {
        for subject in [
            "s".repeat(infra_jobs::MAX_PAYLOAD_BYTES + 1),
            "\"".repeat(infra_jobs::MAX_PAYLOAD_BYTES / 2),
        ] {
            let mut event = prepared(Bytes::new());
            event.subject = subject;
            let expected = serde_json::to_vec(&PublishDomainEvent::from(&event))
                .unwrap()
                .len();
            assert!(expected > infra_jobs::MAX_PAYLOAD_BYTES);
            assert_eq!(event.outbox_payload_limit().unwrap(), 0);

            let (result, polls) = complete(event.prepare_outbox_intent());
            assert!(polls >= event.subject.len().div_ceil(PREFLIGHT_BYTES_PER_POLL));
            assert!(matches!(
                result,
                Err(OutboxEnqueueError::Jobs(infra_jobs::EnqueueError::PayloadTooLarge { bytes }))
                    if bytes == expected
            ));

            event.payload = Bytes::from_static(b"{}");
            let (result, polls) = complete(event.prepare_outbox_intent());
            assert!(matches!(
                result,
                Err(OutboxEnqueueError::PayloadTooLarge {
                    bytes: 2,
                    max_bytes: 0
                })
            ));
            if event.subject.len() > infra_jobs::MAX_PAYLOAD_BYTES {
                assert_eq!(polls, 1, "a nonempty body needs no oversized-subject scan");
            }
        }
    }

    #[test]
    fn preflight_poll_budget_is_shared_across_metadata_fields() {
        let mut event = prepared(Bytes::new());
        event.subject = "s".repeat(3000);
        event.message_id = "i".repeat(3000);
        event.publication_id = "p".repeat(3000);
        event.event_type = "e".repeat(3000).into();

        let (result, polls) = complete(event.prepare_outbox_intent());
        assert!(result.is_ok());
        assert!(polls >= 12000_usize.div_ceil(PREFLIGHT_BYTES_PER_POLL));
    }

    #[derive(Default)]
    struct WakeCount(AtomicUsize);

    impl Wake for WakeCount {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn complete<T>(future: impl Future<Output = T>) -> (T, usize) {
        let notifications = Arc::new(WakeCount::default());
        let waker = Waker::from(Arc::clone(&notifications));
        let mut context = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        for polls in 1..=1024 {
            let previous_wakes = notifications.0.load(Ordering::Relaxed);
            match future.as_mut().poll(&mut context) {
                Poll::Ready(result) => return (result, polls),
                Poll::Pending => assert!(
                    notifications.0.load(Ordering::Relaxed) > previous_wakes,
                    "a bounded preflight must schedule its next poll"
                ),
            }
        }
        panic!("bounded fixture did not complete");
    }

    fn serialized_len(intent: &PublishDomainEvent, payload_bytes: usize) -> usize {
        let mut candidate = intent.clone();
        candidate.payload_base64 = STANDARD.encode(vec![0; payload_bytes]);
        serde_json::to_vec(&candidate)
            .expect("primitive intent serializes")
            .len()
    }
}
