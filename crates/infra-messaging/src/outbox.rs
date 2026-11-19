use std::time::Duration;

use async_nats::Subject;
use bytes::Bytes;
use domain_events::{Event, EventPayload};
use infra_jobs::outbox::{self as job_outbox, Admission, KIND as OUTBOX_KIND};
use infra_jobs::{Handler, Job, JobError, JobKind, Kinds, Policy};
use infra_postgres::{Tx, TxMode};
use operation_context::Deadline;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use uuid::{NonNilUuid, fmt::Simple};

use crate::error::{PublishError, PublishRejection};
use crate::prepared::{
    PreparedEvent, Publication, payload_depth_allowed, validate_reconstructed_envelope,
};
use crate::{EnvelopeError, OutboxProducer, PublicationRoute};

const FORMAT_VERSION: u8 = 1;
const PREFLIGHT_BYTES_PER_POLL: usize = 4096;
const JSON_STRING_QUOTES_BYTES: usize = "\"\"".len();

const POLICY: Policy = Policy {
    max_attempts: 25,
    timeout: Duration::from_secs(30),
    max_running: None,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxEnqueued {
    Created,
    Duplicate,
}

#[derive(Debug, thiserror::Error)]
pub enum OutboxEnqueueError {
    #[error("new outbox events require a publication route bound with for_outbox(publisher_id)")]
    PublisherRequired,
    #[error("outbox payload is {bytes} bytes; the immutable intent allows {max_bytes}")]
    PayloadTooLarge { bytes: usize, max_bytes: usize },
    #[error("outbox event identity conflicts with its immutable intent or business order")]
    EventIdConflict,
    #[error("outbox aggregate version is occupied or its predecessor is missing")]
    OrderConflict,
    #[error(
        "outbox order requires a 1..=255 byte key without control characters and a positive version"
    )]
    InvalidOrder,
    #[error(transparent)]
    Jobs(#[from] infra_jobs::EnqueueError),
}

#[derive(Debug)]
#[derive_where::derive_where(Clone)]
pub struct OutboxRoute<T: EventPayload> {
    publication: PublicationRoute<T>,
    publisher_id: NonNilUuid,
}

impl<T: EventPayload> PublicationRoute<T> {
    #[must_use]
    pub fn for_outbox(self, publisher_id: NonNilUuid) -> OutboxRoute<T> {
        OutboxRoute {
            publication: self,
            publisher_id,
        }
    }
}

impl<T: EventPayload + Serialize> OutboxRoute<T> {
    pub fn prepare(&self, event: &Event<T>) -> Result<PreparedEvent, EnvelopeError> {
        self.publication.prepare_outbox(event, || {
            let prefix = "outbox-v1-";
            let digest: [u8; 32] = Sha256::digest(event.id.as_bytes()).into();
            let mut hex = const_hex::Buffer::<32>::new();
            let digest = hex.format(&digest);
            let mut publisher = [0; Simple::LENGTH];
            let publisher = self
                .publisher_id
                .get()
                .simple()
                .encode_lower(&mut publisher);
            let mut id = String::with_capacity(prefix.len() + publisher.len() + 1 + digest.len());
            id.push_str(prefix);
            id.push_str(publisher);
            id.push('-');
            id.push_str(digest);
            id
        })
    }
}

pub fn kinds(producer: OutboxProducer) -> Result<Kinds, infra_jobs::KindError> {
    job_outbox::publisher_kinds(POLICY, producer)
}

#[derive(Default)]
struct OutboxText<'a> {
    subject: &'a str,
    message_id: &'a str,
    publication_id: &'a str,
    event_type: &'a str,
}

impl<'a> OutboxText<'a> {
    fn fields(self) -> [&'a str; 4] {
        let Self {
            subject,
            message_id,
            publication_id,
            event_type,
        } = self;
        [subject, message_id, publication_id, event_type]
    }
}

impl PreparedEvent {
    pub async fn enqueue(
        &self,
        tx: &mut Tx<'_, impl TxMode>,
    ) -> Result<OutboxEnqueued, OutboxEnqueueError> {
        self.enqueue_with_order(tx, None).await
    }

    pub async fn enqueue_ordered(
        &self,
        tx: &mut Tx<'_, impl TxMode>,
        aggregate_key: &str,
        version: i64,
    ) -> Result<OutboxEnqueued, OutboxEnqueueError> {
        self.enqueue_with_order(tx, Some((aggregate_key, version)))
            .await
    }

    async fn enqueue_with_order(
        &self,
        tx: &mut Tx<'_, impl TxMode>,
        order: Option<(&str, i64)>,
    ) -> Result<OutboxEnqueued, OutboxEnqueueError> {
        let intent = self.prepare_outbox_intent().await?;
        let admission = if self.has_bound_publication() {
            job_outbox::enqueue(tx, &intent, order).await?
        } else {
            job_outbox::reconcile(tx, &intent, order).await?
        };
        match admission {
            Admission::Created => Ok(OutboxEnqueued::Created),
            Admission::Duplicate => Ok(OutboxEnqueued::Duplicate),
            Admission::EventIdConflict => Err(OutboxEnqueueError::EventIdConflict),
            Admission::OrderConflict => Err(OutboxEnqueueError::OrderConflict),
            Admission::InvalidOrder => Err(OutboxEnqueueError::InvalidOrder),
            Admission::MissingReceipt => Err(OutboxEnqueueError::PublisherRequired),
        }
    }

    async fn prepare_outbox_intent(&self) -> Result<PublishDomainEvent, OutboxEnqueueError> {
        if self.subject().len() > infra_jobs::MAX_PAYLOAD_BYTES && !self.payload().is_empty() {
            return Err(OutboxEnqueueError::PayloadTooLarge {
                bytes: self.payload().len(),
                max_bytes: 0,
            });
        }
        let overhead = self.outbox_metadata_bytes().await?;
        let max_bytes = raw_payload_capacity(overhead);
        if self.payload().len() > max_bytes {
            return Err(OutboxEnqueueError::PayloadTooLarge {
                bytes: self.payload().len(),
                max_bytes,
            });
        }
        if overhead > infra_jobs::MAX_PAYLOAD_BYTES {
            return Err(infra_jobs::EnqueueError::PayloadTooLarge { bytes: overhead }.into());
        }
        Ok(PublishDomainEvent::from(self))
    }

    async fn outbox_metadata_bytes(&self) -> Result<usize, infra_jobs::EnqueueError> {
        let mut total =
            infra_jobs::json_payload_len(&PublishDomainEvent::for_metadata_sizing(self))
                .map_err(infra_jobs::EnqueueError::Serialize)?;
        let mut budget = PREFLIGHT_BYTES_PER_POLL;
        for mut rest in self.outbox_text().fields() {
            while !rest.is_empty() {
                let end = rest.floor_char_boundary(budget);
                if end == 0 {
                    tokio::task::yield_now().await;
                    budget = PREFLIGHT_BYTES_PER_POLL;
                    continue;
                }
                let (chunk, tail) = rest.split_at(end);
                total += infra_jobs::json_payload_len(chunk)
                    .map_err(infra_jobs::EnqueueError::Serialize)?
                    - JSON_STRING_QUOTES_BYTES;
                budget -= chunk.len();
                rest = tail;
            }
        }
        Ok(total)
    }

    fn outbox_text(&self) -> OutboxText<'_> {
        OutboxText {
            subject: self.subject(),
            message_id: self.message_id(),
            publication_id: self.publication_id(),
            event_type: self.event_type(),
        }
    }
}

impl Handler<PublishDomainEvent> for OutboxProducer {
    async fn run(&self, job: Job<PublishDomainEvent>) -> Result<(), JobError> {
        let Self(producer) = self;
        let stored = job.payload();
        let (occurred_at, payload) = stored.validate().map_err(JobError::permanent)?;
        let subject = Subject::from(stored.subject.as_str());
        let event = Publication {
            subject: &subject,
            message_id: &stored.message_id,
            publication_id: &stored.publication_id,
            event_type: &stored.event_type,
            schema_version: stored.schema_version,
            occurred_at,
            payload,
        };
        let deadline = crate::budget::within_operation_budget(job.deadline());
        let cancel = job.cancellation();
        let admission = crate::topology::recheck_outbox_source(&producer.shared, deadline, &cancel);
        producer
            .publish_after_admission(&event, Some(Deadline::at(deadline)), &cancel, admission)
            .await
            .map(|_| ())
            .map_err(publication_job_error)
    }
}

fn publication_job_error(error: PublishError) -> JobError {
    match error {
        PublishError::Ambiguous => JobError::retryable("publication_ack_unknown"),
        PublishError::Rejected(reason) => match reason {
            PublishRejection::InvalidEnvelope(_) => JobError::permanent("outbox_envelope_invalid"),
            PublishRejection::BrokerRefused | PublishRejection::BrokerStreamSealed => {
                JobError::retryable("broker_refused")
            }
            PublishRejection::BrokerSizeLimit => JobError::retryable("broker_size_limit"),
            PublishRejection::WindowFull => JobError::retryable("publish_window_full"),
            PublishRejection::Unavailable => JobError::retryable("publisher_unavailable"),
            PublishRejection::ContextStopped => JobError::retryable("publish_context_stopped"),
        },
    }
}

#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug))]
struct InvalidBase64;

mod base64_payload {
    use std::borrow::Cow;

    use base64::{display::Base64Display, engine::general_purpose::STANDARD};
    use bytes::Bytes;
    use serde::de::IntoDeserializer as _;
    use serde::{Deserializer, Serializer};
    use serde_with::{As, BorrowCow, base64::Base64};

    use super::InvalidBase64;

    pub(super) fn serialize<S: Serializer>(
        source: &Result<Bytes, InvalidBase64>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match source {
            Ok(bytes) => serializer.collect_str(&Base64Display::new(bytes, &STANDARD)),
            Err(InvalidBase64) => Err(serde::ser::Error::custom("payload is not base64")),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Result<Bytes, InvalidBase64>, D::Error> {
        let encoded: Cow<'de, str> = As::<BorrowCow>::deserialize(deserializer)?;
        Ok(
            As::<Base64>::deserialize(encoded.as_ref().into_deserializer())
                .map_err(|_: serde::de::value::Error| InvalidBase64),
        )
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(test, derive(Debug))]
struct PublishDomainEvent {
    #[serde(rename = "version")]
    format_version: u8,
    subject: String,
    message_id: String,
    publication_id: String,
    event_type: String,
    schema_version: u16,
    occurred_at_unix_seconds: i64,
    occurred_at_nanosecond: u32,
    #[serde(rename = "payload_base64", with = "base64_payload")]
    payload: Result<Bytes, InvalidBase64>,
}

impl JobKind for PublishDomainEvent {
    const NAME: &'static str = OUTBOX_KIND;
}

impl From<&PreparedEvent> for PublishDomainEvent {
    fn from(event: &PreparedEvent) -> Self {
        Self {
            payload: Ok(event.payload().clone()),
            ..Self::without_payload(event, &event.outbox_text())
        }
    }
}

impl PublishDomainEvent {
    fn for_metadata_sizing(event: &PreparedEvent) -> Self {
        Self::without_payload(event, &OutboxText::default())
    }

    fn without_payload(event: &PreparedEvent, text: &OutboxText<'_>) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            subject: text.subject.to_owned(),
            message_id: text.message_id.to_owned(),
            publication_id: text.publication_id.to_owned(),
            event_type: text.event_type.to_owned(),
            schema_version: event.schema_version(),
            occurred_at_unix_seconds: event.occurred_at().unix_timestamp(),
            occurred_at_nanosecond: event.occurred_at().nanosecond(),
            payload: Ok(Bytes::new()),
        }
    }
    fn validate(&self) -> Result<(OffsetDateTime, &Bytes), &'static str> {
        if self.format_version != FORMAT_VERSION {
            return Err("outbox_format_unsupported");
        }
        let occurred_at = OffsetDateTime::from_unix_timestamp(self.occurred_at_unix_seconds)
            .and_then(|time| time.replace_nanosecond(self.occurred_at_nanosecond))
            .map_err(|_| "outbox_timestamp_invalid")?;
        let payload = self
            .payload
            .as_ref()
            .map_err(|InvalidBase64| "outbox_base64_invalid")?;
        let json = simdutf8::compat::from_utf8(payload).map_err(|_| "outbox_json_invalid")?;
        if !payload_depth_allowed(payload) {
            return Err("outbox_payload_too_deep");
        }
        serde_json::from_str::<serde::de::IgnoredAny>(json).map_err(|_| "outbox_json_invalid")?;
        validate_reconstructed_envelope(
            &self.subject,
            &self.message_id,
            &self.publication_id,
            &self.event_type,
            self.schema_version,
            occurred_at,
        )
        .map_err(|_| "outbox_envelope_invalid")?;
        Ok((occurred_at, payload))
    }
}

fn raw_payload_capacity(overhead: usize) -> usize {
    (infra_jobs::MAX_PAYLOAD_BYTES.saturating_sub(overhead) / 4) * 3
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    use bytes::Bytes;
    use time::OffsetDateTime;

    use super::{
        FORMAT_VERSION, OutboxEnqueueError, PREFLIGHT_BYTES_PER_POLL, PublishDomainEvent,
        raw_payload_capacity,
    };
    use crate::prepared::{PreparedEvent, PreparedEventParts};

    struct Parts {
        subject: String,
        message_id: String,
        publication_id: String,
        event_type: String,
        payload: Bytes,
    }

    impl Parts {
        fn new(payload: Bytes) -> Self {
            Self {
                subject: "events.created".to_owned(),
                message_id: "logical-id".to_owned(),
                publication_id: "logical-id".to_owned(),
                event_type: "example.created".to_owned(),
                payload,
            }
        }

        fn event(&self) -> PreparedEvent {
            PreparedEvent::from_parts(PreparedEventParts {
                subject: self.subject.clone(),
                message_id: self.message_id.clone(),
                publication_id: self.publication_id.clone(),
                event_type: self.event_type.clone(),
                schema_version: 1,
                occurred_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
                payload: self.payload.clone(),
            })
            .expect("fixture parts are valid")
        }
    }

    fn prepared(payload: Bytes) -> PreparedEvent {
        Parts::new(payload).event()
    }

    #[test]
    fn storage_preserves_exact_padded_base64_payload_bytes() {
        let event = prepared(Bytes::from_static(br#"{"escaped":"\\\\u0000","n":1.0}"#));
        let encoded = serde_json::to_vec(&PublishDomainEvent::from(&event)).unwrap();
        let wire: serde_json::Value = serde_json::from_slice(&encoded).unwrap();

        assert_eq!(wire["version"], FORMAT_VERSION);
        assert_eq!(
            wire["payload_base64"],
            "eyJlc2NhcGVkIjoiXFxcXHUwMDAwIiwibiI6MS4wfQ=="
        );
        let stored: PublishDomainEvent = serde_json::from_slice(&encoded).unwrap();
        let (occurred_at, payload) = stored.validate().expect("stored immutable intent is valid");
        assert_eq!(payload, event.payload());
        assert_eq!(occurred_at, event.occurred_at());
        assert_eq!(stored.subject, event.subject());
        assert_eq!(stored.message_id, event.message_id());
        assert_eq!(stored.publication_id, event.publication_id());
    }

    #[test]
    fn stored_intent_rejects_invalid_base64_before_broker_dispatch() {
        let baseline = serde_json::to_value(PublishDomainEvent::from(&prepared(
            Bytes::from_static(b"{}"),
        )))
        .unwrap();

        let mut corrupt = baseline.clone();
        corrupt["payload_base64"] = serde_json::Value::String("not-base64".to_owned());
        let stored: PublishDomainEvent =
            serde_json::from_value(corrupt).expect("corrupt base64 still decodes as an outbox job");
        std::assert_matches!(stored.validate(), Err("outbox_base64_invalid"));

        let mut non_string = baseline;
        non_string["payload_base64"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<PublishDomainEvent>(non_string).is_err());
    }

    #[test]
    fn storage_preserves_distinct_redrive_publication_identity() {
        let mut parts = Parts::new(Bytes::from_static(b"{}"));
        parts.publication_id = "redrive-8e51b5a19b7924ce".to_owned();
        let event = parts.event();

        let encoded = serde_json::to_vec(&PublishDomainEvent::from(&event)).unwrap();
        let stored = serde_json::from_slice::<PublishDomainEvent>(&encoded).unwrap();
        stored.validate().expect("stored redrive intent is valid");

        assert_eq!(stored.message_id, "logical-id");
        assert_eq!(stored.publication_id, "redrive-8e51b5a19b7924ce");
    }

    #[test]
    fn payload_limit_fits_exact_base64_boundary_for_varied_metadata() {
        let compact = PublishDomainEvent::from(&prepared(Bytes::from_static(b"{}")));
        let wide = PublishDomainEvent {
            format_version: FORMAT_VERSION,
            subject: "s".repeat(256),
            message_id: "i".repeat(256),
            publication_id: "p".repeat(256),
            event_type: "e".repeat(256),
            schema_version: u16::MAX,
            occurred_at_unix_seconds: 253_402_300_799,
            occurred_at_nanosecond: 999_999_999,
            payload: Ok(Bytes::new()),
        };

        for intent in [compact, wide] {
            let limit = outbox_raw_capacity(&intent);

            assert!(serialized_len(&intent, limit) <= infra_jobs::MAX_PAYLOAD_BYTES);
            assert!(serialized_len(&intent, limit + 1) > infra_jobs::MAX_PAYLOAD_BYTES);
        }
    }

    #[test]
    fn cooperative_preflight_preserves_escaped_metadata_and_the_exact_body_limit() {
        let mut parts = Parts::new(Bytes::new());
        parts.subject = format!("{}é😀\\\"\u{0001}", "s".repeat(4095));
        parts.message_id = "logical-\"id\\".to_owned();
        parts.publication_id = "publication-😀".to_owned();
        parts.event_type = "example.\"created".to_owned();
        let limit = outbox_raw_capacity(&PublishDomainEvent::from(&parts.event()));
        parts.payload = Bytes::from(vec![b'x'; limit]);
        let event = parts.event();
        let expected = serde_json::to_vec(&PublishDomainEvent::from(&event)).unwrap();

        let (result, polls) = complete(event.prepare_outbox_intent());
        assert!(polls > 1);
        assert_eq!(serde_json::to_vec(&result.unwrap()).unwrap(), expected);
        assert!(expected.len() <= infra_jobs::MAX_PAYLOAD_BYTES);

        parts.payload = Bytes::from(vec![b'x'; limit + 1]);
        let event = parts.event();
        let (result, _) = complete(event.prepare_outbox_intent());
        std::assert_matches!(
            result,
            Err(OutboxEnqueueError::PayloadTooLarge { bytes, max_bytes })
                if bytes == limit + 1 && max_bytes == limit
        );
    }

    #[test]
    fn oversized_metadata_preserves_empty_and_nonempty_body_errors() {
        for subject in [
            "s".repeat(infra_jobs::MAX_PAYLOAD_BYTES + 1),
            "\"".repeat(infra_jobs::MAX_PAYLOAD_BYTES / 2),
        ] {
            let mut parts = Parts::new(Bytes::new());
            parts.subject = subject;
            let event = parts.event();
            let expected = serde_json::to_vec(&PublishDomainEvent::from(&event))
                .unwrap()
                .len();
            assert!(expected > infra_jobs::MAX_PAYLOAD_BYTES);

            let (result, polls) = complete(event.prepare_outbox_intent());
            assert!(polls >= event.subject().len().div_ceil(PREFLIGHT_BYTES_PER_POLL));
            std::assert_matches!(
                result,
                Err(OutboxEnqueueError::Jobs(infra_jobs::EnqueueError::PayloadTooLarge { bytes }))
                    if bytes == expected
            );

            parts.payload = Bytes::from_static(b"{}");
            let event = parts.event();
            let (result, polls) = complete(event.prepare_outbox_intent());
            std::assert_matches!(
                result,
                Err(OutboxEnqueueError::PayloadTooLarge {
                    bytes: 2,
                    max_bytes: 0
                })
            );
            if event.subject().len() > infra_jobs::MAX_PAYLOAD_BYTES {
                assert_eq!(polls, 1, "a nonempty body needs no oversized-subject scan");
            }
        }
    }

    #[test]
    fn preflight_poll_budget_is_shared_across_metadata_fields() {
        let mut parts = Parts::new(Bytes::new());
        parts.subject = "s".repeat(PREFLIGHT_BYTES_PER_POLL - 100);
        parts.message_id = "i".repeat(256);
        parts.publication_id = "p".repeat(256);
        parts.event_type = "e".repeat(256);
        let fields = PREFLIGHT_BYTES_PER_POLL - 100 + 3 * 256;

        let (result, polls) = complete(parts.event().prepare_outbox_intent());
        assert!(result.is_ok());
        assert!(polls >= fields.div_ceil(PREFLIGHT_BYTES_PER_POLL));
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

    fn outbox_raw_capacity(intent: &PublishDomainEvent) -> usize {
        let overhead = serialized_len(intent, 0);
        raw_payload_capacity(overhead)
    }

    fn serialized_len(intent: &PublishDomainEvent, payload_bytes: usize) -> usize {
        let mut candidate = intent.clone();
        candidate.payload = Ok(Bytes::from(vec![0; payload_bytes]));
        serde_json::to_vec(&candidate)
            .expect("primitive intent serializes")
            .len()
    }
}
