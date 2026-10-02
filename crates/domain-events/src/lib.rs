//! Typed domain events, independent of any message broker.
//!
//! Feature code declares payload types and builds [`Event`] values. Subjects,
//! headers, wire limits and publication belong to the messaging adapter.
//!
//! Callers supply the ID and occurrence time: this crate mints no IDs and
//! reads no clock, so a retry cannot change an event's identity.
//!
//! The messaging adapter validates an event when it prepares it. The event
//! type and ID must be short printable text and the occurrence time must not
//! be Go's zero time; see `docs/durable-messaging.md`.

use time::UtcDateTime;

/// A payload type published or consumed as a domain event.
///
/// The messaging adapter asks for `serde::Serialize` where it publishes a
/// payload and for `serde::de::DeserializeOwned` where it delivers one, so a
/// type implements only the direction its service uses. It asks for
/// `utoipa::ToSchema` where a route is documented in the contract document.
pub trait EventPayload {
    /// Stable event name, for example `"order.created"`. Never rename it once
    /// published: consumers route by it.
    const EVENT_TYPE: &'static str;
    /// Payload schema version. Must be positive, and the messaging adapter
    /// rejects zero at compile time; bump it when the payload changes
    /// incompatibly.
    const SCHEMA_VERSION: u16;
}

/// One occurrence of a domain event.
///
/// Build it once, before a retryable business transaction, so every retry
/// publishes the same `id`. The fields stay public; the value is fixed when
/// the messaging adapter prepares it for publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event<T> {
    /// Logical ID. It stays the same across publication retries and redrives;
    /// consumers deduplicate by it.
    pub id: String,
    /// When the domain occurrence happened.
    pub occurred_at: UtcDateTime,
    /// Typed feature payload.
    pub payload: T,
}
