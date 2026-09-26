//! Provider-free typed domain event identity.
//!
//! An event is constructed once, before a retryable business transaction.
//! Transport routing, publication and delivery metadata deliberately belong to
//! the messaging adapter rather than this crate.

use serde::Serialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;

/// A typed payload with a stable, composition-routed wire identity.
pub trait EventPayload: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// Stable event type selected by the owning feature.
    const EVENT_TYPE: &'static str;
    /// Positive schema version selected by the owning feature.
    const SCHEMA_VERSION: u16;
}

/// An immutable, provider-free domain occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event<T> {
    id: String,
    occurred_at: OffsetDateTime,
    payload: T,
}

impl<T: EventPayload> Event<T> {
    /// Creates one event occurrence without minting an ID or reading a clock.
    ///
    /// The constructor admits the Go-compatible identity and time domain. The
    /// adapter serializes the payload exactly once after this boundary.
    ///
    /// # Errors
    /// Rejects absent, oversized or control-bearing identity, zero schema, and
    /// a zero or unrepresentable UTC occurrence time.
    pub fn new(
        id: impl Into<String>,
        occurred_at: OffsetDateTime,
        payload: T,
    ) -> Result<Self, EventError> {
        let id = id.into();
        validate_text("event id", &id)?;
        validate_text("event type", T::EVENT_TYPE)?;
        if T::SCHEMA_VERSION == 0 {
            return Err(EventError::SchemaVersionZero);
        }
        let occurred_at = occurred_at
            .checked_to_offset(time::UtcOffset::UTC)
            .ok_or(EventError::OccurredAtOutOfRange)?;
        if occurred_at.year() == 1
            && occurred_at.ordinal() == 1
            && occurred_at.time() == time::Time::MIDNIGHT
        {
            return Err(EventError::OccurredAtZero);
        }

        Ok(Self {
            id,
            occurred_at,
            payload,
        })
    }

    /// Logical ID, unchanged for every publication retry and redrive.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// UTC occurrence time, normalized at construction.
    #[must_use]
    pub fn occurred_at(&self) -> OffsetDateTime {
        self.occurred_at
    }

    /// Typed feature payload.
    #[must_use]
    pub fn payload(&self) -> &T {
        &self.payload
    }
}

/// A rejected domain event identity.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EventError {
    /// A required identity string is absent.
    #[error("{field} is required")]
    MissingText { field: &'static str },
    /// A Go header identity exceeds its accepted wire bound.
    #[error("{field} exceeds the 256-byte identity limit")]
    TextTooLong { field: &'static str },
    /// Identity strings are text and must never carry control characters.
    #[error("{field} must be valid text without control characters")]
    InvalidText { field: &'static str },
    /// Version zero has no canonical `vN` wire spelling.
    #[error("event schema version must be positive")]
    SchemaVersionZero,
    /// Go's zero time is not an event occurrence.
    #[error("event occurrence time is required")]
    OccurredAtZero,
    /// The normalized occurrence exceeds the supported calendar range.
    #[error("event occurrence time is out of range")]
    OccurredAtOutOfRange,
}

fn validate_text(field: &'static str, value: &str) -> Result<(), EventError> {
    if value.is_empty() {
        return Err(EventError::MissingText { field });
    }
    if value.len() > 256 {
        return Err(EventError::TextTooLong { field });
    }
    if value.chars().any(char::is_control) {
        return Err(EventError::InvalidText { field });
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test fixtures construct known valid times and events"
)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct Created {
        value: String,
    }

    impl EventPayload for Created {
        const EVENT_TYPE: &'static str = "example.created";
        const SCHEMA_VERSION: u16 = 1;
    }

    #[test]
    fn constructor_keeps_logical_identity_and_normalizes_offset() {
        let event = Event::new(
            "evt-1",
            OffsetDateTime::from_unix_timestamp(1_700_000_000)
                .unwrap()
                .to_offset(time::UtcOffset::from_hms(3, 0, 0).unwrap()),
            Created {
                value: "payload".into(),
            },
        )
        .unwrap();

        assert_eq!(event.id(), "evt-1");
        assert_eq!(event.occurred_at().offset(), time::UtcOffset::UTC);
        assert_eq!(event.payload().value, "payload");
    }

    #[test]
    fn constructor_rejects_control_characters_before_transport() {
        let error = Event::new(
            "evt\n1",
            OffsetDateTime::now_utc(),
            Created {
                value: "payload".into(),
            },
        )
        .unwrap_err();

        assert_eq!(error, EventError::InvalidText { field: "event id" });
    }
}
