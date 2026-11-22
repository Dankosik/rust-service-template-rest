use std::fmt::{self, Write as _};
use std::time::Duration;

use arrayvec::ArrayString;
use async_nats::Subject;
use bytes::Bytes;
use domain_events::{Event, EventPayload};
use infra_jobs::outbox::{self as job_outbox, Order};
use infra_jobs::{Handler, Job, JobError, Kinds, PayloadSizer, Policy, RetrySchedule};
use infra_postgres::{Tx, TxMode};
use operation_context::Deadline;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use time::UtcDateTime;
use uuid::NonNilUuid;

use crate::budget::{BROKER_OPERATION_BUDGET, BrokerBudget};
use crate::error::{PrepareError, PublishError, PublishRejection};
use crate::prepared::{
    PreparedEvent, Publication, payload_depth_allowed, validate_reconstructed_envelope,
};
use crate::wire::CreatedAt;
use crate::{OutboxProducer, PublicationRoute};

const FORMAT_VERSION: u8 = 1;

const POLICY: Policy = Policy {
    max_attempts: 25,
    timeout: Duration::from_secs(30),
    max_running: None,
    retry: RetrySchedule::Exponential {
        max_base: Duration::from_secs(54),
    },
};

pub use infra_jobs::outbox::{OutboxEnqueued, ReconcileOutcome};

#[derive(Debug, thiserror::Error)]
pub enum OutboxEnqueueError {
    #[error("outbox payload is {bytes} bytes; the immutable intent allows {max_bytes}")]
    PayloadTooLarge { bytes: usize, max_bytes: usize },
    #[error("outbox payload contains a decoded NUL character")]
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
    Jobs(#[from] infra_jobs::EnqueueError),
}

impl From<job_outbox::OutboxEnqueueError> for OutboxEnqueueError {
    fn from(error: job_outbox::OutboxEnqueueError) -> Self {
        match error {
            job_outbox::OutboxEnqueueError::PayloadContainsNul => Self::PayloadContainsNul,
            job_outbox::OutboxEnqueueError::EventIdConflict => Self::EventIdConflict,
            job_outbox::OutboxEnqueueError::OrderConflict => Self::OrderConflict,
            job_outbox::OutboxEnqueueError::InvalidOrder => Self::InvalidOrder,
            job_outbox::OutboxEnqueueError::Enqueue(error) => Self::Jobs(error),
        }
    }
}

#[derive_where::derive_where(Clone)]
pub struct OutboxRoute<T> {
    publication: PublicationRoute<T>,
    id_prefix: ArrayString<43>,
}

impl<T: EventPayload> fmt::Debug for OutboxRoute<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutboxRoute")
            .field("publication", &self.publication)
            .field("id_prefix", &self.id_prefix)
            .finish()
    }
}

impl<T: EventPayload> PublicationRoute<T> {
    #[must_use]
    pub fn for_outbox(self, publisher_id: NonNilUuid) -> OutboxRoute<T> {
        let mut id_prefix = ArrayString::new();
        let _ = write!(&mut id_prefix, "outbox-v1-{}-", publisher_id.get().simple());
        OutboxRoute {
            publication: self,
            id_prefix,
        }
    }
}

impl<T: EventPayload + Serialize> OutboxRoute<T> {
    pub fn prepare(&self, event: &Event<T>) -> Result<OutboxEvent, PrepareError> {
        self.publication
            .prepare_with_id(event, || {
                crate::wire::prefixed_digest_hex(
                    &self.id_prefix,
                    &Sha256::digest(event.id.as_bytes()).into(),
                )
            })
            .map(OutboxEvent)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxEvent(PreparedEvent);

impl AsRef<PreparedEvent> for OutboxEvent {
    fn as_ref(&self) -> &PreparedEvent {
        &self.0
    }
}

impl From<OutboxEvent> for PreparedEvent {
    fn from(event: OutboxEvent) -> Self {
        event.0
    }
}

impl OutboxEvent {
    #[must_use]
    pub const fn as_prepared(&self) -> &PreparedEvent {
        &self.0
    }

    #[must_use]
    pub fn into_prepared(self) -> PreparedEvent {
        self.0
    }

    #[must_use]
    pub fn subject(&self) -> &str {
        self.0.subject()
    }

    #[must_use]
    pub fn message_id(&self) -> &str {
        self.0.message_id()
    }

    #[must_use]
    pub fn publication_id(&self) -> &str {
        self.0.publication_id()
    }

    #[must_use]
    pub fn event_type(&self) -> &str {
        self.0.event_type()
    }

    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.0.schema_version()
    }

    #[must_use]
    pub fn schema_label(&self) -> String {
        self.0.schema_label()
    }

    #[must_use]
    pub const fn occurred_at(&self) -> UtcDateTime {
        self.0.occurred_at()
    }

    #[must_use]
    pub fn payload(&self) -> &Bytes {
        self.0.payload()
    }

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
        self.enqueue_with_order(
            tx,
            Some(Order {
                aggregate_key,
                version,
            }),
        )
        .await
    }

    async fn enqueue_with_order(
        &self,
        tx: &mut Tx<'_, impl TxMode>,
        order: Option<Order<'_>>,
    ) -> Result<OutboxEnqueued, OutboxEnqueueError> {
        let intent = self.0.prepare_outbox_intent().await?;
        Ok(job_outbox::enqueue(tx, intent.as_job_intent(), order).await?)
    }
}

pub fn kinds(producer: OutboxProducer) -> Result<Kinds, infra_jobs::KindError> {
    job_outbox::publisher_kinds(POLICY, producer)
}

#[derive(Clone, Copy)]
struct OutboxText<'a> {
    subject: &'a str,
    message_id: &'a str,
    publication_id: &'a str,
    event_type: &'a str,
}

impl OutboxText<'static> {
    const EMPTY: Self = Self {
        subject: "",
        message_id: "",
        publication_id: "",
        event_type: "",
    };
}

impl<'a> OutboxText<'a> {
    const fn fields(self) -> [&'a str; 4] {
        let Self {
            subject,
            message_id,
            publication_id,
            event_type,
        } = self;
        [subject, message_id, publication_id, event_type]
    }

    fn raw_bytes(self) -> usize {
        self.fields()
            .into_iter()
            .fold(0_usize, |total, field| total.saturating_add(field.len()))
    }
}

#[cfg_attr(test, derive(Debug))]
struct PreparedIntent<'a> {
    envelope: EnqueueIntent<'a>,
    encoded_bytes: usize,
}

impl<'a> PreparedIntent<'a> {
    const fn as_job_intent(&self) -> job_outbox::Intent<'_, EnqueueIntent<'a>> {
        job_outbox::Intent::with_capacity(&self.envelope, self.encoded_bytes)
    }
}

impl PreparedEvent {
    pub async fn reconcile_legacy_outbox(
        &self,
        tx: &mut Tx<'_, impl TxMode>,
    ) -> Result<ReconcileOutcome, OutboxEnqueueError> {
        self.reconcile_legacy_outbox_with_order(tx, None).await
    }

    pub async fn reconcile_legacy_outbox_ordered(
        &self,
        tx: &mut Tx<'_, impl TxMode>,
        aggregate_key: &str,
        version: i64,
    ) -> Result<ReconcileOutcome, OutboxEnqueueError> {
        self.reconcile_legacy_outbox_with_order(
            tx,
            Some(Order {
                aggregate_key,
                version,
            }),
        )
        .await
    }

    async fn reconcile_legacy_outbox_with_order(
        &self,
        tx: &mut Tx<'_, impl TxMode>,
        order: Option<Order<'_>>,
    ) -> Result<ReconcileOutcome, OutboxEnqueueError> {
        let intent = self.prepare_outbox_intent().await?;
        Ok(job_outbox::reconcile(tx, intent.as_job_intent(), order).await?)
    }

    async fn prepare_outbox_intent(&self) -> Result<PreparedIntent<'_>, OutboxEnqueueError> {
        if self.subject().len() > infra_jobs::MAX_PAYLOAD_BYTES && !self.payload().is_empty() {
            return Err(OutboxEnqueueError::PayloadTooLarge {
                bytes: self.payload().len(),
                max_bytes: 0,
            });
        }
        let sizer = self.outbox_metadata_sizer().await?;
        let metadata_bytes = sizer.total_bytes();
        let max_bytes = raw_payload_capacity(metadata_bytes);
        if self.payload().len() > max_bytes {
            return Err(OutboxEnqueueError::PayloadTooLarge {
                bytes: self.payload().len(),
                max_bytes,
            });
        }
        sizer.check_limit()?;
        let encoded_bytes = metadata_bytes + wire_payload::encoded_len(self.payload().len());
        Ok(PreparedIntent {
            envelope: EnqueueIntent::from(self),
            encoded_bytes,
        })
    }

    async fn outbox_metadata_sizer(&self) -> Result<PayloadSizer, infra_jobs::EnqueueError> {
        let text = self.outbox_text();
        if text.raw_bytes() <= PayloadSizer::BYTES_PER_POLL {
            return PayloadSizer::from_skeleton(&EnqueueIntent::without_payload(self, text));
        }
        let mut sizer =
            PayloadSizer::from_skeleton(&EnqueueIntent::without_payload(self, OutboxText::EMPTY))?;
        for field in text.fields() {
            sizer.add_str(field).await?;
        }
        Ok(sizer)
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
        let (created_at, payload) = stored.validate().map_err(JobError::permanent)?;
        let publication = Publication {
            subject: &stored.subject,
            message_id: &stored.message_id,
            publication_id: &stored.publication_id,
            event_type: &stored.event_type,
            schema_version: stored.schema_version,
            created_at: created_at.as_str(),
            payload,
        };
        let deadline =
            Deadline::at(job.deadline()).earlier(Deadline::after(BROKER_OPERATION_BUDGET));
        let budget = BrokerBudget::new(deadline, job.cancellation());
        let admission = crate::topology::recheck_outbox_source(&producer.shared, budget);
        producer
            .publish_after_admission(&publication, budget, admission)
            .await
            .map(|_| ())
            .map_err(publication_job_error)
    }
}

fn publication_job_error(error: PublishError) -> JobError {
    match error {
        PublishError::Ambiguous => JobError::retryable("publication_ack_unknown"),
        PublishError::Rejected(PublishRejection::InvalidEnvelope(_)) => {
            JobError::permanent("outbox_envelope_invalid")
        }
        PublishError::Rejected(
            PublishRejection::BrokerRefused | PublishRejection::BrokerStreamSealed,
        ) => JobError::retryable("broker_refused"),
        PublishError::Rejected(PublishRejection::BrokerSizeLimit) => {
            JobError::retryable("broker_size_limit")
        }
        PublishError::Rejected(PublishRejection::WindowFull) => {
            JobError::retryable("publish_window_full")
        }
        PublishError::Rejected(PublishRejection::Unavailable) => {
            JobError::retryable("publisher_unavailable")
        }
        PublishError::Rejected(PublishRejection::ContextStopped) => {
            JobError::retryable("publish_context_stopped")
        }
    }
}

#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug))]
struct InvalidBase64;

mod wire_payload {
    use std::fmt;

    use base64::alphabet::STANDARD;
    use base64::display::Base64Display;
    use base64::engine::{DecodePaddingMode, Engine as _, GeneralPurpose, GeneralPurposeConfig};
    use bytes::Bytes;
    use serde::de::{self, Visitor};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::InvalidBase64;

    const ENGINE: GeneralPurpose = GeneralPurpose::new(
        &STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );

    struct ExpectStr(&'static str);

    impl Visitor<'_> for ExpectStr {
        type Value = ();

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.0)
        }

        fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    #[cfg_attr(test, derive(Debug))]
    pub(super) struct BorrowedText<'a>(pub(super) &'a str);

    impl Serialize for BorrowedText<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_str(self.0)
        }
    }

    impl<'de> Deserialize<'de> for BorrowedText<'_> {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            deserializer
                .deserialize_str(ExpectStr("a string"))
                .map(|()| Self(""))
        }
    }

    #[derive(Clone, Copy)]
    #[cfg_attr(test, derive(Debug))]
    pub(super) struct BorrowedPayload<'a>(pub(super) &'a [u8]);

    impl Serialize for BorrowedPayload<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_str(&Base64Display::new(self.0, &ENGINE))
        }
    }

    impl<'de> Deserialize<'de> for BorrowedPayload<'_> {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            deserializer
                .deserialize_str(ExpectStr("a base64-encoded string"))
                .map(|()| Self(&[]))
        }
    }

    #[derive(Clone)]
    #[cfg_attr(test, derive(Debug))]
    pub(super) struct StoredPayload(pub(super) Result<Bytes, InvalidBase64>);

    impl<'de> Deserialize<'de> for StoredPayload {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct PayloadVisitor;

            impl Visitor<'_> for PayloadVisitor {
                type Value = Result<Bytes, InvalidBase64>;

                fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str("a base64-encoded string")
                }

                fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                    Ok(ENGINE
                        .decode(value)
                        .map(Bytes::from)
                        .map_err(|_| InvalidBase64))
                }
            }

            deserializer.deserialize_str(PayloadVisitor).map(Self)
        }
    }

    pub(super) fn encoded_len(raw_bytes: usize) -> usize {
        base64::encoded_len(raw_bytes, true).unwrap_or(0)
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(test, derive(Debug))]
struct OutboxEnvelope<S, T, P> {
    #[serde(rename = "version")]
    format_version: u8,
    subject: S,
    message_id: T,
    publication_id: T,
    event_type: T,
    schema_version: u16,
    occurred_at_unix_seconds: i64,
    occurred_at_nanosecond: u32,
    #[serde(rename = "payload_base64")]
    payload: P,
}

type PublishDomainEvent = OutboxEnvelope<Subject, String, wire_payload::StoredPayload>;
type EnqueueIntent<'a> = OutboxEnvelope<
    wire_payload::BorrowedText<'a>,
    wire_payload::BorrowedText<'a>,
    wire_payload::BorrowedPayload<'a>,
>;

impl<'a> From<&'a PreparedEvent> for EnqueueIntent<'a> {
    fn from(event: &'a PreparedEvent) -> Self {
        Self {
            payload: wire_payload::BorrowedPayload(event.payload()),
            ..Self::without_payload(event, event.outbox_text())
        }
    }
}

impl<'a> EnqueueIntent<'a> {
    fn without_payload(event: &PreparedEvent, text: OutboxText<'a>) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            subject: wire_payload::BorrowedText(text.subject),
            message_id: wire_payload::BorrowedText(text.message_id),
            publication_id: wire_payload::BorrowedText(text.publication_id),
            event_type: wire_payload::BorrowedText(text.event_type),
            schema_version: event.schema_version(),
            occurred_at_unix_seconds: event.occurred_at().unix_timestamp(),
            occurred_at_nanosecond: event.occurred_at().nanosecond(),
            payload: wire_payload::BorrowedPayload(&[]),
        }
    }
}

impl PublishDomainEvent {
    fn validate(&self) -> Result<(CreatedAt, &Bytes), &'static str> {
        if self.format_version != FORMAT_VERSION {
            return Err("outbox_format_unsupported");
        }
        let occurred_at = UtcDateTime::from_unix_timestamp(self.occurred_at_unix_seconds)
            .and_then(|time| time.replace_nanosecond(self.occurred_at_nanosecond))
            .map_err(|_| "outbox_timestamp_invalid")?;
        let payload = self
            .payload
            .0
            .as_ref()
            .map_err(|InvalidBase64| "outbox_base64_invalid")?;
        let json = simdutf8::compat::from_utf8(payload).map_err(|_| "outbox_json_invalid")?;
        if !payload_depth_allowed(payload) {
            return Err("outbox_payload_too_deep");
        }
        serde_json::from_str::<serde::de::IgnoredAny>(json).map_err(|_| "outbox_json_invalid")?;
        let created_at = validate_reconstructed_envelope(
            self.subject.as_str(),
            &self.message_id,
            &self.publication_id,
            &self.event_type,
            self.schema_version,
            occurred_at,
        )
        .map_err(|_| "outbox_envelope_invalid")?;
        Ok((created_at, payload))
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
    use time::UtcDateTime;

    use super::{
        EnqueueIntent, FORMAT_VERSION, OutboxEnqueueError, OutboxEnvelope, PayloadSizer,
        PublishDomainEvent, raw_payload_capacity, wire_payload,
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
                occurred_at: UtcDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
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
        let intent = EnqueueIntent::from(&event);
        assert!(std::ptr::eq(intent.subject.0, event.subject()));
        assert!(std::ptr::eq(intent.message_id.0, event.message_id()));
        assert!(std::ptr::eq(
            intent.publication_id.0,
            event.publication_id()
        ));
        assert!(std::ptr::eq(intent.event_type.0, event.event_type()));
        assert!(std::ptr::eq(intent.payload.0, event.payload().as_ref()));
        let encoded = serde_json::to_vec(&intent).unwrap();
        let wire: serde_json::Value = serde_json::from_slice(&encoded).unwrap();

        assert_eq!(wire["version"], FORMAT_VERSION);
        assert_eq!(
            wire["payload_base64"],
            "eyJlc2NhcGVkIjoiXFxcXHUwMDAwIiwibiI6MS4wfQ=="
        );
        assert!(serde_json::from_slice::<EnqueueIntent<'_>>(&encoded).is_ok());
        let stored: PublishDomainEvent = serde_json::from_slice(&encoded).unwrap();
        let (occurred_at, payload) = stored.validate().expect("stored immutable intent is valid");
        assert_eq!(payload, event.payload());
        assert_eq!(occurred_at.instant(), event.occurred_at());
        assert_eq!(stored.subject.as_str(), event.subject());
        assert_eq!(stored.message_id, event.message_id());
        assert_eq!(stored.publication_id, event.publication_id());
    }

    #[test]
    fn stored_intent_rejects_invalid_base64_before_broker_dispatch() {
        let event = prepared(Bytes::from_static(b"{}"));
        let baseline = serde_json::to_value(EnqueueIntent::from(&event)).unwrap();

        let mut unpadded = baseline.clone();
        unpadded["payload_base64"] = serde_json::Value::String("e30".to_owned());
        assert!(serde_json::from_value::<EnqueueIntent<'_>>(unpadded.clone()).is_ok());
        let stored: PublishDomainEvent =
            serde_json::from_value(unpadded).expect("unpadded base64 decodes as an outbox job");
        let (_, payload) = stored.validate().expect("unpadded base64 remains valid");
        assert_eq!(payload.as_ref(), b"{}");

        let mut corrupt = baseline.clone();
        corrupt["payload_base64"] = serde_json::Value::String("not-base64".to_owned());
        assert!(serde_json::from_value::<EnqueueIntent<'_>>(corrupt.clone()).is_ok());
        let stored: PublishDomainEvent =
            serde_json::from_value(corrupt).expect("corrupt base64 still decodes as an outbox job");
        std::assert_matches!(stored.validate(), Err("outbox_base64_invalid"));

        for field in [
            "subject",
            "message_id",
            "publication_id",
            "event_type",
            "payload_base64",
        ] {
            let mut non_string = baseline.clone();
            non_string[field] = serde_json::Value::Null;
            assert!(
                serde_json::from_value::<EnqueueIntent<'_>>(non_string.clone()).is_err(),
                "{field} must reject non-string values on enqueue"
            );
            assert!(
                serde_json::from_value::<PublishDomainEvent>(non_string).is_err(),
                "{field} must reject non-string values on worker decode"
            );
        }
    }

    #[test]
    fn storage_preserves_distinct_redrive_publication_identity() {
        let mut parts = Parts::new(Bytes::from_static(b"{}"));
        parts.publication_id = "redrive-8e51b5a19b7924ce".to_owned();
        let event = parts.event();

        let encoded = serde_json::to_vec(&EnqueueIntent::from(&event)).unwrap();
        let stored = serde_json::from_slice::<PublishDomainEvent>(&encoded).unwrap();
        stored.validate().expect("stored redrive intent is valid");

        assert_eq!(stored.message_id, "logical-id");
        assert_eq!(stored.publication_id, "redrive-8e51b5a19b7924ce");
    }

    #[test]
    fn payload_limit_fits_exact_base64_boundary_for_varied_metadata() {
        let compact_event = prepared(Bytes::from_static(b"{}"));
        let compact = EnqueueIntent::from(&compact_event);
        let wide_subject = "s".repeat(256);
        let wide_message_id = "i".repeat(256);
        let wide_publication_id = "p".repeat(256);
        let wide_event_type = "e".repeat(256);
        let wide = OutboxEnvelope {
            format_version: FORMAT_VERSION,
            subject: wire_payload::BorrowedText(&wide_subject),
            message_id: wire_payload::BorrowedText(&wide_message_id),
            publication_id: wire_payload::BorrowedText(&wide_publication_id),
            event_type: wire_payload::BorrowedText(&wide_event_type),
            schema_version: u16::MAX,
            occurred_at_unix_seconds: 253_402_300_799,
            occurred_at_nanosecond: 999_999_999,
            payload: wire_payload::BorrowedPayload(&[]),
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
        let limit = outbox_raw_capacity(&EnqueueIntent::from(&parts.event()));
        parts.payload = Bytes::from(vec![b'x'; limit]);
        let event = parts.event();
        let expected = serde_json::to_vec(&EnqueueIntent::from(&event)).unwrap();

        let (result, polls) = complete(event.prepare_outbox_intent());
        assert!(polls > 1);
        let prepared = result.unwrap();
        assert_eq!(prepared.encoded_bytes, expected.len());
        assert_eq!(serde_json::to_vec(&prepared.envelope).unwrap(), expected);
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
    fn single_pass_metadata_sizing_matches_exact_bytes_at_the_poll_budget_boundary() {
        for excess in [0_usize, 1] {
            let mut parts = Parts::new(Bytes::from(vec![b'a'; 2_048]));
            parts.message_id = "logical-\"id\\".to_owned();
            parts.publication_id = "publication-😀".to_owned();
            parts.event_type = "example.\"created".to_owned();
            let other_len =
                parts.message_id.len() + parts.publication_id.len() + parts.event_type.len();
            let suffix = "é\\\"";
            let prefix_len = PayloadSizer::BYTES_PER_POLL + excess - other_len - suffix.len();
            parts.subject = format!("{}{suffix}", "s".repeat(prefix_len));
            let event = parts.event();
            let expected = serde_json::to_vec(&EnqueueIntent::from(&event)).unwrap();

            let (result, polls) = complete(event.prepare_outbox_intent());
            let prepared = result.unwrap();
            assert_eq!(prepared.encoded_bytes, expected.len());
            assert_eq!(serde_json::to_vec(&prepared.envelope).unwrap(), expected);
            if excess == 0 {
                assert_eq!(polls, 1);
            } else {
                assert!(polls > 1);
            }
        }
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
            let expected = serde_json::to_vec(&EnqueueIntent::from(&event))
                .unwrap()
                .len();
            assert!(expected > infra_jobs::MAX_PAYLOAD_BYTES);

            let (result, polls) = complete(event.prepare_outbox_intent());
            assert!(polls >= event.subject().len().div_ceil(PayloadSizer::BYTES_PER_POLL));
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
        parts.subject = "s".repeat(PayloadSizer::BYTES_PER_POLL - 100);
        parts.message_id = "i".repeat(256);
        parts.publication_id = "p".repeat(256);
        parts.event_type = "e".repeat(256);
        let fields = PayloadSizer::BYTES_PER_POLL - 100 + 3 * 256;

        let event = parts.event();
        let (result, polls) = complete(event.prepare_outbox_intent());
        assert!(result.is_ok());
        assert!(polls >= fields.div_ceil(PayloadSizer::BYTES_PER_POLL));
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

    fn outbox_raw_capacity(intent: &EnqueueIntent<'_>) -> usize {
        let overhead = serialized_len(intent, 0);
        raw_payload_capacity(overhead)
    }

    fn serialized_len(intent: &EnqueueIntent<'_>, payload_bytes: usize) -> usize {
        let payload = vec![0; payload_bytes];
        let mut candidate = intent.clone();
        candidate.payload = wire_payload::BorrowedPayload(&payload);
        serde_json::to_vec(&candidate)
            .expect("primitive intent serializes")
            .len()
    }

    #[test]
    fn outbox_route_clones_and_formats_debug_without_payload_debug_bound() {
        #[derive(serde::Serialize)]
        struct OpaquePayload;

        impl domain_events::EventPayload for OpaquePayload {
            const EVENT_TYPE: &'static str = "example.opaque";
            const SCHEMA_VERSION: u16 = 1;
        }

        let publisher = uuid::NonNilUuid::new(uuid::Uuid::from_u128(1)).unwrap();
        let mut registry = crate::Registry::default();
        let publication = registry
            .declare_publication(&crate::Route::<OpaquePayload>::new("events.opaque"), 128)
            .unwrap();
        let route = publication.for_outbox(publisher);
        let cloned = route.clone();
        let debug = format!("{route:?}");
        assert_eq!(debug, format!("{cloned:?}"));
        assert!(debug.contains("outbox-v1-00000000000000000000000000000001-"));
        let event = domain_events::Event {
            id: "event-1".to_owned(),
            occurred_at: time::UtcDateTime::UNIX_EPOCH,
            payload: OpaquePayload,
        };
        let prepared = cloned.prepare(&event).unwrap();
        assert!(
            prepared
                .publication_id()
                .starts_with("outbox-v1-00000000000000000000000000000001-")
        );
        assert_eq!(prepared.publication_id().len(), 43 + 64);
    }
}
