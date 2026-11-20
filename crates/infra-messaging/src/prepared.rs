use std::borrow::Cow;
use std::fmt;

use async_nats::Subject;
use bytes::Bytes;
use domain_events::{Event, EventPayload};
use serde::Serialize;
use time::UtcDateTime;

use crate::error::EnvelopeError;
use crate::registry::route_key;

const MAX_PAYLOAD_JSON_DEPTH: usize = 127;

#[derive(Clone, PartialEq, Eq)]
enum EventIdentity {
    Direct {
        message_id: String,
    },
    Bound {
        message_id: String,
        distinct_publication_id: Option<String>,
    },
}

impl EventIdentity {
    fn publication_id(&self) -> &str {
        match self {
            Self::Direct { message_id } => message_id,
            Self::Bound {
                message_id,
                distinct_publication_id,
            } => distinct_publication_id.as_deref().unwrap_or(message_id),
        }
    }
}

impl fmt::Debug for EventIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct { message_id } => formatter
                .debug_struct("Direct")
                .field("message_id", message_id)
                .finish(),
            Self::Bound { message_id, .. } => formatter
                .debug_struct("Bound")
                .field("message_id", message_id)
                .field("publication_id", &self.publication_id())
                .finish(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedEvent {
    subject: Subject,
    identity: EventIdentity,
    event_type: Cow<'static, str>,
    schema_version: u16,
    occurred_at: UtcDateTime,
    payload: Bytes,
}

#[derive(Debug)]
pub(crate) struct PreparedEventParts {
    pub(crate) subject: String,
    pub(crate) message_id: String,
    pub(crate) publication_id: String,
    pub(crate) event_type: String,
    pub(crate) schema_version: u16,
    pub(crate) occurred_at: UtcDateTime,
    pub(crate) payload: Bytes,
}

#[expect(
    clippy::struct_field_names,
    reason = "distinguish the broker publication ID from the logical message ID"
)]
pub(crate) struct Publication<'a> {
    pub(crate) subject: &'a Subject,
    pub(crate) message_id: &'a str,
    pub(crate) publication_id: &'a str,
    pub(crate) event_type: &'a str,
    pub(crate) schema_version: u16,
    pub(crate) occurred_at: UtcDateTime,
    pub(crate) payload: &'a Bytes,
}

impl PreparedEvent {
    pub(crate) fn prepare_with_valid_subject<T: EventPayload + Serialize>(
        subject: Subject,
        event: &Event<T>,
        max_payload_bytes: usize,
    ) -> Result<Self, EnvelopeError> {
        Self::prepare(subject, event, max_payload_bytes, || {
            EventIdentity::Direct {
                message_id: event.id.clone(),
            }
        })
    }

    // template:begin outbox:messaging-outbox-preparation
    pub(crate) fn prepare_outbox_with_valid_subject<T: EventPayload + Serialize>(
        subject: Subject,
        event: &Event<T>,
        max_payload_bytes: usize,
        make_bound_id: impl FnOnce() -> String,
    ) -> Result<Self, EnvelopeError> {
        Self::prepare(subject, event, max_payload_bytes, || EventIdentity::Bound {
            message_id: make_bound_id(),
            distinct_publication_id: None,
        })
    }

    pub(crate) const fn has_bound_publication(&self) -> bool {
        matches!(self.identity, EventIdentity::Bound { .. })
    }
    // template:end outbox:messaging-outbox-preparation

    fn prepare<T: EventPayload + Serialize>(
        subject: Subject,
        event: &Event<T>,
        max_payload_bytes: usize,
        make_identity: impl FnOnce() -> EventIdentity,
    ) -> Result<Self, EnvelopeError> {
        let payload =
            json_payload::to_vec_bounded(&event.payload, max_payload_bytes).map_err(|error| {
                match error {
                    json_payload::EncodeError::Serialize(_) => EnvelopeError::Serialization,
                    json_payload::EncodeError::TooLarge { .. } => EnvelopeError::PayloadTooLarge,
                }
            })?;
        if !payload_depth_allowed(&payload) {
            return Err(EnvelopeError::PayloadStructure);
        }
        let occurred_at = event.occurred_at;
        crate::wire::validate_outbound_identity(&event.id)?;
        let key = route_key::<T>();
        crate::wire::validate_outbound_created_at(occurred_at)?;
        Ok(Self {
            subject,
            identity: make_identity(),
            event_type: Cow::Borrowed(key.event_type),
            schema_version: key.schema_version,
            occurred_at,
            payload: payload.into(),
        })
    }

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
        validate_reconstructed_envelope(
            &subject,
            &message_id,
            &publication_id,
            &event_type,
            schema_version,
            occurred_at,
        )?;
        Ok(Self {
            subject: Subject::from(subject),
            identity: EventIdentity::Bound {
                distinct_publication_id: (message_id != publication_id).then_some(publication_id),
                message_id,
            },
            event_type: event_type.into(),
            schema_version,
            occurred_at,
            payload,
        })
    }

    pub(crate) fn publication(&self) -> Publication<'_> {
        Publication {
            subject: &self.subject,
            message_id: self.message_id(),
            publication_id: self.publication_id(),
            event_type: self.event_type(),
            schema_version: self.schema_version,
            occurred_at: self.occurred_at,
            payload: &self.payload,
        }
    }

    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }
    #[must_use]
    pub fn message_id(&self) -> &str {
        match &self.identity {
            EventIdentity::Direct { message_id } | EventIdentity::Bound { message_id, .. } => {
                message_id
            }
        }
    }
    #[must_use]
    pub fn publication_id(&self) -> &str {
        self.identity.publication_id()
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
    pub fn schema_label(&self) -> String {
        crate::wire::schema_label(self.schema_version)
    }
    #[must_use]
    pub const fn occurred_at(&self) -> UtcDateTime {
        self.occurred_at
    }
    #[must_use]
    pub fn payload(&self) -> &Bytes {
        &self.payload
    }
}

pub(crate) fn validate_reconstructed_envelope(
    subject: &str,
    message_id: &str,
    publication_id: &str,
    event_type: &str,
    schema_version: u16,
    occurred_at: UtcDateTime,
) -> Result<(), EnvelopeError> {
    if !crate::wire::valid_subject(subject) {
        return Err(EnvelopeError::Subject);
    }
    for value in [message_id, publication_id, event_type] {
        crate::wire::validate_outbound_identity(value)?;
    }
    if schema_version == 0 {
        return Err(EnvelopeError::Schema);
    }
    crate::wire::validate_outbound_created_at(occurred_at)
}

pub(crate) fn payload_depth_allowed(mut bytes: &[u8]) -> bool {
    use bstr::ByteSlice as _;

    let mut depth = 0_usize;
    while let Some((&byte, rest)) = bytes.split_first() {
        match byte {
            b'"' => {
                bytes = rest;
                loop {
                    let Some(offset) = bytes.find_byteset(b"\"\\") else {
                        return true;
                    };
                    let escaped = bytes[offset] == b'\\';
                    let Some(after) = bytes.get(offset + 1 + usize::from(escaped)..) else {
                        return true;
                    };
                    bytes = after;
                    if !escaped {
                        break;
                    }
                }
            }
            b'[' | b'{' => {
                depth += 1;
                if depth > MAX_PAYLOAD_JSON_DEPTH {
                    return false;
                }
                bytes = rest;
            }
            b']' | b'}' => {
                depth = depth.saturating_sub(1);
                bytes = rest;
            }
            _ => bytes = rest,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use domain_events::{Event, EventPayload};
    use serde::ser::{Error as _, SerializeSeq as _};
    use serde::{Serialize, Serializer};
    use time::UtcDateTime;

    use super::PreparedEvent;
    use crate::{EnvelopeError, Registry, Route};

    fn prepare<T: EventPayload + Serialize + 'static>(
        subject: &str,
        event: &Event<T>,
        max_payload_bytes: usize,
    ) -> Result<PreparedEvent, EnvelopeError> {
        Registry::default()
            .declare_publication(&Route::new(subject), max_payload_bytes)
            .map_err(|_| EnvelopeError::Subject)?
            .prepare(event)
    }

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

    struct Counted<T> {
        value: T,
        calls: Cell<usize>,
    }

    impl<T> EventPayload for Counted<T> {
        const EVENT_TYPE: &'static str = "example.created";
        const SCHEMA_VERSION: u16 = 1;
    }

    impl<T: Serialize> Serialize for Counted<T> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.calls.set(self.calls.get() + 1);
            self.value.serialize(serializer)
        }
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
            occurred_at: UtcDateTime::UNIX_EPOCH,
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
        let service = crate::PublicationPolicy {
            max_payload_bytes: expected.len(),
        };
        let route = Route::new("events.created");
        let mut registry = Registry::default();
        let publication = service.declare(&mut registry, &route).unwrap();
        let prepared = publication
            .prepare(&event)
            .expect("payload at the exact ceiling is admitted");
        assert_eq!(prepared.payload().as_ref(), expected.as_bytes());
        assert!(
            registry
                .check_publication_limit(expected.len() - 1)
                .is_err()
        );
        let strict = service
            .capped_at(expected.len() - 1)
            .declare(&mut Registry::default(), &route)
            .unwrap();
        std::assert_matches!(strict.prepare(&event), Err(EnvelopeError::PayloadTooLarge));
        let mut registry = Registry::default();
        service
            .capped_at(usize::MAX)
            .declare(&mut registry, &route)
            .unwrap();
        assert!(registry.check_publication_limit(expected.len()).is_ok());
    }

    #[test]
    fn preparation_rejects_nested_non_finite_floats_before_size_without_losing_null() {
        #[derive(Serialize)]
        struct Numbers {
            required: f64,
            optional: Option<f32>,
        }

        for (required, optional) in [
            (f64::NAN, None),
            (f64::INFINITY, None),
            (f64::NEG_INFINITY, None),
            (0.0, Some(f32::NAN)),
            (0.0, Some(f32::INFINITY)),
            (0.0, Some(f32::NEG_INFINITY)),
        ] {
            let event = event(Counted {
                value: vec![Numbers { required, optional }],
                calls: Cell::new(0),
            });
            let error = prepare("events.created", &event, 0)
                .expect_err("non-finite numbers cannot become publishable intent");
            assert_eq!(error, EnvelopeError::Serialization);
            assert_eq!(error.to_string(), "event payload cannot be serialized");
            assert_eq!(event.payload.calls.get(), 1);
        }

        for (optional, expected) in [
            (None, r#"[{"required":-0.0,"optional":null}]"#),
            (Some(1.5), r#"[{"required":-0.0,"optional":1.5}]"#),
        ] {
            let event = event(Counted {
                value: vec![Numbers {
                    required: -0.0,
                    optional,
                }],
                calls: Cell::new(0),
            });
            let prepared = prepare("events.created", &event, expected.len())
                .expect("intentional null and finite numbers remain valid");
            assert_eq!(prepared.payload().as_ref(), expected.as_bytes());
            assert_eq!(event.payload.calls.get(), 1);
        }
    }

    #[test]
    fn preparation_depth_matches_the_typed_reader_and_ignores_string_delimiters() {
        for depth in [0, 127, 128] {
            for shape in 0..3 {
                let mut value = serde_json::Value::String("[{}]\\\"\\".repeat(128));
                for level in 0..depth {
                    value = if shape == 0 || (shape == 2 && level % 2 == 0) {
                        serde_json::Value::Array(vec![value])
                    } else {
                        serde_json::json!({"[\"\\]": value})
                    };
                }
                let bytes = serde_json::to_vec(&value).expect("fixture serializes");
                let readable = serde_json::from_slice::<serde_json::Value>(&bytes).is_ok();
                assert_eq!(readable, depth <= 127);
                let event = event(Counted {
                    value,
                    calls: Cell::new(0),
                });
                let prepared = prepare("events.created", &event, bytes.len());
                if readable {
                    assert_eq!(
                        prepared
                            .expect("readable depth prepares")
                            .payload()
                            .as_ref(),
                        bytes
                    );
                } else {
                    let error = prepared.expect_err("unreadable depth cannot be published");
                    assert_eq!(error, EnvelopeError::PayloadStructure);
                    assert_eq!(error.to_string(), "event payload structure is invalid");
                }
                assert_eq!(event.payload.calls.get(), 1);
            }
        }
    }

    #[test]
    fn preparation_checks_depth_after_size_and_before_event_metadata() {
        for (subject, limit, expected, calls) in [
            ("events.*", 0, EnvelopeError::Subject, 0),
            ("events.created", 0, EnvelopeError::PayloadTooLarge, 1),
            ("events.created", 1024, EnvelopeError::PayloadStructure, 1),
        ] {
            let mut value = serde_json::Value::Null;
            for _ in 0..128 {
                value = serde_json::Value::Array(vec![value]);
            }
            let mut event = event(Counted {
                value,
                calls: Cell::new(0),
            });
            event.id.clear();
            event.occurred_at = UtcDateTime::from_unix_timestamp(-62_135_596_800)
                .expect("Go zero time is representable");
            assert_eq!(
                prepare(subject, &event, limit).expect_err("invalid event"),
                expected
            );
            assert_eq!(event.payload.calls.get(), calls);
        }
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
                prepare("events.created", &event, 1024),
                Err(EnvelopeError::Identity)
            );
        }
    }

    #[test]
    fn preparation_serializes_once_and_preserves_error_precedence() {
        struct Case {
            subject: &'static str,
            limit: usize,
            fail_at_end: bool,
            invalid_id: bool,
            zero_time: bool,
            expected_error: EnvelopeError,
            expected_calls: usize,
        }

        for case in [
            Case {
                subject: "events.*",
                limit: 31,
                fail_at_end: true,
                invalid_id: true,
                zero_time: true,
                expected_error: EnvelopeError::Subject,
                expected_calls: 0,
            },
            Case {
                subject: "events.created",
                limit: 31,
                fail_at_end: true,
                invalid_id: true,
                zero_time: true,
                expected_error: EnvelopeError::Serialization,
                expected_calls: 1,
            },
            Case {
                subject: "events.created",
                limit: 31,
                fail_at_end: false,
                invalid_id: true,
                zero_time: true,
                expected_error: EnvelopeError::PayloadTooLarge,
                expected_calls: 1,
            },
            Case {
                subject: "events.created",
                limit: 0,
                fail_at_end: false,
                invalid_id: false,
                zero_time: false,
                expected_error: EnvelopeError::PayloadTooLarge,
                expected_calls: 1,
            },
            Case {
                subject: "events.created",
                limit: 32_769,
                fail_at_end: false,
                invalid_id: true,
                zero_time: false,
                expected_error: EnvelopeError::Identity,
                expected_calls: 1,
            },
            Case {
                subject: "events.created",
                limit: 32_769,
                fail_at_end: false,
                invalid_id: false,
                zero_time: true,
                expected_error: EnvelopeError::CreatedAtRequired,
                expected_calls: 1,
            },
        ] {
            let mut event = event(StreamingPayload {
                calls: Cell::new(0),
                elements: 16_384,
                fail_at_end: case.fail_at_end,
            });
            if case.invalid_id {
                event.id.clear();
            }
            if case.zero_time {
                event.occurred_at = UtcDateTime::from_unix_timestamp(-62_135_596_800)
                    .expect("Go zero time is representable");
            }
            let error = prepare(case.subject, &event, case.limit)
                .expect_err("invalid event must be refused");
            assert_eq!(error, case.expected_error);
            assert_eq!(event.payload.calls.get(), case.expected_calls);
        }
        let event = event(StreamingPayload {
            calls: Cell::new(0),
            elements: 1,
            fail_at_end: false,
        });
        let prepared = prepare("events.created", &event, 3)
            .expect("small output ignores the untrusted size hint");
        assert_eq!(prepared.payload().as_ref(), b"[0]");
        assert_eq!(event.payload.calls.get(), 1);
    }

    // template:begin outbox:messaging-outbox-preparation-tests
    #[test]
    fn bound_preparation_defers_identity_and_shares_equal_ids_with_restoration() {
        let subject = async_nats::Subject::from_static("events.created");
        let mut event = event(Counted {
            value: true,
            calls: Cell::new(0),
        });
        let direct = PreparedEvent::prepare_with_valid_subject(subject.clone(), &event, 4).unwrap();
        let bound =
            PreparedEvent::prepare_outbox_with_valid_subject(subject.clone(), &event, 4, || {
                event.id.clone()
            })
            .unwrap();
        assert_ne!(direct, bound);
        assert!(bound.has_bound_publication());
        assert!(std::ptr::eq(bound.message_id(), bound.publication_id()));

        let restore = |publication_id: &str| {
            PreparedEvent::from_parts(super::PreparedEventParts {
                subject: subject.to_string(),
                message_id: bound.message_id().to_owned(),
                publication_id: publication_id.to_owned(),
                event_type: bound.event_type().to_owned(),
                schema_version: bound.schema_version(),
                occurred_at: bound.occurred_at(),
                payload: bound.payload().clone(),
            })
            .unwrap()
        };
        let restored = restore("event-id");
        assert_eq!(bound, restored);
        assert!(std::ptr::eq(
            restored.message_id(),
            restored.publication_id()
        ));
        assert!(
            format!("{bound:?}").contains(
                r#"identity: Bound { message_id: "event-id", publication_id: "event-id" }"#
            )
        );
        let redrive = restore("redrive-id");
        assert_ne!(bound, redrive);
        assert!(format!("{redrive:#?}").contains(concat!(
            "identity: Bound {\n",
            "        message_id: \"event-id\",\n",
            "        publication_id: \"redrive-id\",\n",
            "    }"
        )));

        event.id.clear();
        std::assert_matches!(
            PreparedEvent::prepare_outbox_with_valid_subject(subject, &event, 4, || {
                panic!("invalid identity must be rejected before building the bound ID")
            }),
            Err(EnvelopeError::Identity)
        );
    }
    // template:end outbox:messaging-outbox-preparation-tests

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
            let result = prepare("events.created", &event, 1_024);
            if accepted {
                let prepared = result.expect("the time is in range");
                crate::wire::encode_prepared(&prepared).expect("an accepted time encodes");
            } else {
                std::assert_matches!(result, Err(EnvelopeError::CreatedAtOutOfRange));
            }
        }
    }
}
