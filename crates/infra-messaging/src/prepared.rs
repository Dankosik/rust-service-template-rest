use std::borrow::Cow;
use std::io::{self, Write};

use async_nats::Subject;
use bytes::Bytes;
use domain_events::{Event, EventPayload};
use serde::Serialize;
use time::OffsetDateTime;

use crate::error::EnvelopeError;

/// Serialized, routed event intent. Retrying never reserializes or reroutes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedEvent {
    subject: Subject,
    message_id: String,
    publication_id: String,
    event_type: Cow<'static, str>,
    schema_version: u16,
    occurred_at: OffsetDateTime,
    payload: Bytes,
}

#[derive(Debug)]
pub(crate) struct PreparedEventParts {
    pub(crate) subject: String,
    pub(crate) message_id: String,
    pub(crate) publication_id: String,
    pub(crate) event_type: String,
    pub(crate) schema_version: u16,
    pub(crate) occurred_at: OffsetDateTime,
    pub(crate) payload: Bytes,
}

impl PreparedEvent {
    /// Serializes one typed occurrence without connecting to a broker.
    ///
    /// This is shared by direct publication, outbox storage and the
    /// broker-independent Go compatibility proof.
    ///
    /// Serialization runs once and synchronously. Retained JSON is bounded by
    /// `max_payload_bytes`; excess output is counted until serialization ends,
    /// preserving a later serializer error. Custom serializers must be finite,
    /// trusted, nonblocking code. Callers own source size, collection cardinality
    /// and concurrent preparation within their operation deadline. That deadline
    /// and the output bound cannot preempt a callback or bound its private
    /// allocations. Bound the source before invoking this synchronous API.
    ///
    /// # Errors
    /// Rejects an invalid subject, unserializable payload, payload beyond the
    /// configured bound, an invalid identity or ID with surrounding whitespace,
    /// or an invalid creation time.
    pub fn prepare<T: EventPayload + Serialize>(
        subject: impl Into<String>,
        event: &Event<T>,
        max_payload_bytes: usize,
    ) -> Result<Self, EnvelopeError> {
        Self::prepare_routed(Subject::from(subject.into()), event, max_payload_bytes)
    }

    /// [`Self::prepare`] for a subject a route already owns, shared without a copy.
    pub(crate) fn prepare_routed<T: EventPayload + Serialize>(
        subject: Subject,
        event: &Event<T>,
        max_payload_bytes: usize,
    ) -> Result<Self, EnvelopeError> {
        let key = crate::registry::route_key::<T>();
        if !crate::wire::valid_subject(&subject) {
            return Err(EnvelopeError::Subject);
        }
        let mut payload = PayloadWriter::new(max_payload_bytes);
        serde_json::to_writer(&mut payload, &event.payload)
            .map_err(|_| EnvelopeError::Serialization)?;
        if payload.total_bytes > max_payload_bytes {
            return Err(EnvelopeError::PayloadTooLarge);
        }
        let occurred_at = event.occurred_at.to_offset(time::UtcOffset::UTC);
        // The event type was checked at compile time and the publication ID
        // is the message ID, so only the ID and time need checking here.
        crate::wire::validate_identity(&event.id)?;
        if event.id.trim() != event.id || crate::wire::is_zero_time(occurred_at) {
            return Err(EnvelopeError::Identity);
        }
        // A caller can retry the retained payload without reformatting it; the
        // created-at text is derived from `occurred_at` when the envelope is
        // encoded. Check the range here so preparation rejects an unencodable
        // time rather than a later publication attempt.
        crate::wire::check_timestamp(occurred_at)?;
        Ok(Self {
            subject,
            message_id: event.id.clone(),
            publication_id: event.id.clone(),
            event_type: Cow::Borrowed(key.event_type),
            schema_version: key.schema_version,
            occurred_at,
            payload: payload.bytes.into(),
        })
    }

    /// Rebuilds an event from stored parts, checking a concrete subject, identities of at most
    /// [`IDENTITY_MAX_BYTES`](crate::wire::IDENTITY_MAX_BYTES) bytes, so the
    /// five headers stay far below
    /// [`HEADER_LIMIT_BYTES`](crate::wire::HEADER_LIMIT_BYTES), a positive
    /// schema version, and a creation time that is set and can be formatted.
    pub(crate) fn from_parts(parts: PreparedEventParts) -> Result<Self, EnvelopeError> {
        let PreparedEventParts {
            subject,
            message_id,
            publication_id,
            event_type,
            schema_version,
            occurred_at,
            payload,
        } = parts;
        if !crate::wire::valid_subject(&subject) {
            return Err(EnvelopeError::Subject);
        }
        let subject = Subject::from(subject);
        for value in [&message_id, &publication_id, &event_type] {
            crate::wire::validate_identity(value)?;
        }
        if schema_version == 0 || crate::wire::is_zero_time(occurred_at) {
            return Err(EnvelopeError::Identity);
        }
        crate::wire::check_timestamp(occurred_at)?;
        Ok(Self {
            subject,
            message_id,
            publication_id,
            event_type: event_type.into(),
            schema_version,
            occurred_at,
            payload,
        })
    }
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }
    /// The subject as the client's reference-counted type.
    pub(crate) fn subject_handle(&self) -> &Subject {
        &self.subject
    }
    /// The logical event identity consumers deduplicate by.
    #[must_use]
    pub fn message_id(&self) -> &str {
        &self.message_id
    }
    /// The identity the broker deduplicates this publication by. It equals
    /// [`Self::message_id`] except for a redrive, which publishes the same
    /// event again under a new publication identity.
    #[must_use]
    pub fn publication_id(&self) -> &str {
        &self.publication_id
    }
    #[must_use]
    pub fn event_type(&self) -> &str {
        &self.event_type
    }
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }
    /// The `Event-Schema` header value, such as `v1`.
    #[must_use]
    pub fn schema_label(&self) -> String {
        crate::wire::schema_label(self.schema_version)
    }
    #[must_use]
    pub const fn occurred_at(&self) -> OffsetDateTime {
        self.occurred_at
    }
    #[must_use]
    pub fn payload(&self) -> &Bytes {
        &self.payload
    }
}

// Keep accepting writes past the ceiling so a later serialization error still
// takes precedence over the size refusal. Only the retained prefix is bounded.
struct PayloadWriter {
    bytes: Vec<u8>,
    total_bytes: usize,
    limit: usize,
}

impl PayloadWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            total_bytes: 0,
            limit,
        }
    }
}

impl Write for PayloadWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.total_bytes = self
            .total_bytes
            .checked_add(buf.len())
            .ok_or_else(|| io::Error::other("serialized event length overflow"))?;
        let retained = buf.len().min(self.limit - self.bytes.len());
        let needed = self.bytes.len() + retained;
        if needed > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(128)
                .max(needed)
                .min(self.limit);
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        self.bytes.extend_from_slice(&buf[..retained]);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use domain_events::{Event, EventPayload};
    use serde::ser::{Error as _, SerializeSeq as _};
    use serde::{Serialize, Serializer};
    use time::OffsetDateTime;

    use super::{PayloadWriter, PreparedEvent};
    use crate::EnvelopeError;

    #[derive(Serialize)]
    struct ExactPayload {
        text: &'static str,
        amount: f64,
        items: [bool; 2],
    }

    impl EventPayload for ExactPayload {
        const EVENT_TYPE: &'static str = "example.created";
        const SCHEMA_VERSION: u16 = 1;
    }

    struct StreamingPayload {
        calls: Cell<usize>,
        elements: usize,
        fail_at_end: bool,
    }

    impl EventPayload for StreamingPayload {
        const EVENT_TYPE: &'static str = "example.created";
        const SCHEMA_VERSION: u16 = 1;
    }

    impl Serialize for StreamingPayload {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.calls.set(self.calls.get() + 1);
            // This hint must not become an allocation request.
            let mut sequence = serializer.serialize_seq(Some(usize::MAX))?;
            for _ in 0..self.elements {
                sequence.serialize_element(&0_u8)?;
            }
            if self.fail_at_end {
                return Err(S::Error::custom("late failure"));
            }
            sequence.end()
        }
    }

    fn event<T>(payload: T) -> Event<T> {
        Event {
            id: "event-id".to_owned(),
            occurred_at: OffsetDateTime::UNIX_EPOCH.to_utc(),
            payload,
        }
    }

    #[test]
    fn preparation_preserves_json_bytes_at_the_exact_ceiling() {
        let event = event(ExactPayload {
            text: "a\"\\\n\0Ж",
            amount: 1.0,
            items: [true, false],
        });
        let expected = r#"{"text":"a\"\\\n\u0000Ж","amount":1.0,"items":[true,false]}"#;
        let prepared = PreparedEvent::prepare("events.created", &event, expected.len())
            .expect("payload at the exact ceiling is admitted");
        assert_eq!(prepared.payload().as_ref(), expected.as_bytes());
        std::assert_matches!(
            PreparedEvent::prepare("events.created", &event, expected.len() - 1),
            Err(EnvelopeError::PayloadTooLarge)
        );
    }

    #[test]
    fn preparation_refuses_ids_that_nats_would_trim() {
        let mut event = event(ExactPayload {
            text: "sample",
            amount: 1.0,
            items: [true, false],
        });
        for id in [" event-id", "event-id ", "event-id\u{a0}"] {
            event.id = id.to_owned();
            std::assert_matches!(
                PreparedEvent::prepare("events.created", &event, 1024),
                Err(EnvelopeError::Identity)
            );
        }
    }

    #[test]
    fn preparation_serializes_once_and_preserves_error_precedence() {
        for (subject, limit, fail_at_end, invalid_id, zero_time, reason, calls) in [
            ("events.*", 31, true, true, true, EnvelopeError::Subject, 0),
            (
                "events.created",
                31,
                true,
                true,
                true,
                EnvelopeError::Serialization,
                1,
            ),
            (
                "events.created",
                31,
                false,
                true,
                true,
                EnvelopeError::PayloadTooLarge,
                1,
            ),
            (
                "events.created",
                0,
                false,
                false,
                false,
                EnvelopeError::PayloadTooLarge,
                1,
            ),
            (
                "events.created",
                32_769,
                false,
                true,
                false,
                EnvelopeError::Identity,
                1,
            ),
            (
                "events.created",
                32_769,
                false,
                false,
                true,
                EnvelopeError::Identity,
                1,
            ),
        ] {
            let mut event = event(StreamingPayload {
                calls: Cell::new(0),
                elements: 16_384,
                fail_at_end,
            });
            if invalid_id {
                event.id.clear();
            }
            if zero_time {
                event.occurred_at = OffsetDateTime::from_unix_timestamp(-62_135_596_800)
                    .expect("Go zero time is representable")
                    .to_utc();
            }
            let error = PreparedEvent::prepare(subject, &event, limit)
                .expect_err("invalid event must be refused");
            assert_eq!(error, reason);
            assert_eq!(event.payload.calls.get(), calls);
        }
        let event = event(StreamingPayload {
            calls: Cell::new(0),
            elements: 1,
            fail_at_end: false,
        });
        let prepared = PreparedEvent::prepare("events.created", &event, 3)
            .expect("small output ignores the untrusted size hint");
        assert_eq!(prepared.payload().as_ref(), b"[0]");
        assert_eq!(event.payload.calls.get(), 1);
    }

    #[test]
    fn serialization_discards_excess_output_without_reserving_its_size() {
        let payload = StreamingPayload {
            calls: Cell::new(0),
            elements: 16_384,
            fail_at_end: false,
        };
        for limit in [0, 1, 31, 256] {
            let mut writer = PayloadWriter::new(limit);
            assert_eq!(writer.bytes.capacity(), 0);
            serde_json::to_writer(&mut writer, &payload).expect("streamed zeros serialize");
            assert_eq!(writer.total_bytes, 32_769);
            assert_eq!(writer.bytes.len(), limit);
            assert!(writer.bytes.capacity() <= limit);
        }
        let mut writer = PayloadWriter::new(1_024);
        serde_json::to_writer(&mut writer, "x").expect("small string serializes");
        assert_eq!(writer.bytes, b"\"x\"");
        assert!(writer.bytes.capacity() < 1_024);
        serde_json::to_writer(&mut writer, &"x".repeat(65_536))
            .expect("a single large fragment is consumed without retaining it all");
        assert_eq!(writer.total_bytes, 65_541);
        assert_eq!(writer.bytes.len(), 1_024);
        assert!(writer.bytes.capacity() <= 1_024);
    }

    #[test]
    fn preparation_accepts_only_creation_times_rfc3339_can_carry() {
        let payload = || ExactPayload {
            text: "",
            amount: 0.0,
            items: [false; 2],
        };
        let at = |year: i32| {
            time::Date::from_calendar_date(year, time::Month::December, 31)
                .and_then(|date| date.with_hms(23, 59, 59))
                .map(time::PrimitiveDateTime::as_utc)
                .expect("valid date")
        };
        for (occurred_at, accepted) in [(at(9999), true), (at(0), true), (at(-1), false)] {
            let event = Event {
                occurred_at,
                ..event(payload())
            };
            let result = PreparedEvent::prepare("events.created", &event, 1_024);
            if accepted {
                let prepared = result.expect("the time is in range");
                crate::wire::encode_prepared(&prepared).expect("an accepted time encodes");
            } else {
                std::assert_matches!(result, Err(EnvelopeError::CreatedAtOutOfRange));
            }
        }
    }
}
