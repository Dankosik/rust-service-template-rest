use std::borrow::Cow;
use std::io::{self, Write};

use bytes::Bytes;
use domain_events::{Event, EventPayload};
use serde::Serialize;
use time::OffsetDateTime;

/// Serialized, routed event intent. Retrying never reserializes or reroutes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedEvent {
    pub(crate) subject: String,
    pub(crate) message_id: String,
    pub(crate) publication_id: String,
    pub(crate) event_type: Cow<'static, str>,
    pub(crate) schema_version: u16,
    pub(crate) occurred_at: OffsetDateTime,
    /// `occurred_at` as the RFC 3339 `Created-At` header value.
    pub(crate) created_at: String,
    pub(crate) payload: Bytes,
}

impl PreparedEvent {
    /// Serializes one typed occurrence without connecting to a broker.
    ///
    /// This is shared by direct publication, outbox storage and the
    /// broker-independent Go compatibility proof.
    ///
    /// # Errors
    /// Rejects an invalid subject, unserializable payload or exceeded wire bound.
    pub fn prepare<T: EventPayload + Serialize>(
        subject: impl Into<String>,
        event: &Event<T>,
        max_payload_bytes: usize,
    ) -> Result<Self, crate::MessagingError> {
        const {
            assert!(
                T::SCHEMA_VERSION > 0,
                "event schema version must be positive"
            );
            assert!(
                crate::wire::valid_text(T::EVENT_TYPE),
                "event type must be 1 to 256 bytes without control characters"
            );
        }
        let subject = subject.into();
        if !crate::wire::valid_subject(&subject) {
            return Err(crate::MessagingError::Envelope("subject is invalid"));
        }
        let mut payload = PayloadWriter::new(max_payload_bytes);
        serde_json::to_writer(&mut payload, &event.payload)
            .map_err(|_| crate::MessagingError::Envelope("event payload cannot be serialized"))?;
        if payload.total_bytes > max_payload_bytes {
            return Err(crate::MessagingError::Envelope(
                "payload exceeds configured maximum",
            ));
        }
        let occurred_at = event.occurred_at.to_offset(time::UtcOffset::UTC);
        // The event type was checked at compile time and the publication ID
        // is the message ID, so only the ID and time need checking here.
        crate::wire::validate_text(&event.id)?;
        if crate::wire::is_zero_time(occurred_at) {
            return Err(crate::MessagingError::Envelope("event identity is invalid"));
        }
        Ok(Self {
            subject,
            message_id: event.id.clone(),
            publication_id: event.id.clone(),
            event_type: Cow::Borrowed(T::EVENT_TYPE),
            schema_version: T::SCHEMA_VERSION,
            occurred_at,
            created_at: crate::wire::format_timestamp(occurred_at)?,
            payload: payload.bytes.into(),
        })
    }
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }
    #[must_use]
    pub fn message_id(&self) -> &str {
        &self.message_id
    }
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
    #[must_use]
    pub fn schema(&self) -> String {
        format!("v{}", self.schema_version)
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

/// Confirmed broker publication acknowledgment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishAck {
    pub stream: String,
    pub sequence: u64,
    pub duplicate: bool,
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "fixed valid events and independent JSON expectations"
)]
mod tests {
    use std::cell::Cell;

    use domain_events::{Event, EventPayload};
    use serde::ser::{Error as _, SerializeSeq as _};
    use serde::{Serialize, Serializer};
    use time::OffsetDateTime;

    use super::{PayloadWriter, PreparedEvent};
    use crate::MessagingError;

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
        assert!(matches!(
            PreparedEvent::prepare("events.created", &event, expected.len() - 1),
            Err(MessagingError::Envelope(
                "payload exceeds configured maximum"
            ))
        ));
    }

    #[test]
    fn preparation_serializes_once_and_preserves_error_precedence() {
        for (subject, limit, fail_at_end, invalid_id, zero_time, reason, calls) in [
            ("events.*", 31, true, true, true, "subject is invalid", 0),
            (
                "events.created",
                31,
                true,
                true,
                true,
                "event payload cannot be serialized",
                1,
            ),
            (
                "events.created",
                31,
                false,
                true,
                true,
                "payload exceeds configured maximum",
                1,
            ),
            (
                "events.created",
                0,
                false,
                false,
                false,
                "payload exceeds configured maximum",
                1,
            ),
            (
                "events.created",
                32_769,
                false,
                true,
                false,
                "header identity is invalid",
                1,
            ),
            (
                "events.created",
                32_769,
                false,
                false,
                true,
                "event identity is invalid",
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
            assert!(matches!(error, MessagingError::Envelope(actual) if actual == reason));
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
}
