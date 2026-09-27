//! Typed domain events, independent of any message broker.
//!
//! Feature code declares payload types and builds [`Event`] values. Subjects,
//! headers, wire limits and publication belong to the messaging adapter.

use serde::Serialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;

/// A payload type published and consumed as a domain event.
pub trait EventPayload: Serialize + DeserializeOwned {
    /// Stable event name, for example `"order.created"`. Never rename it once
    /// published: consumers route by it.
    const EVENT_TYPE: &'static str;
    /// Payload schema version. Must be positive; bump it when the payload
    /// changes incompatibly.
    const SCHEMA_VERSION: u16;
}

/// One occurrence of a domain event.
///
/// Build it once, before a retryable business transaction, so every retry
/// publishes the same `id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event<T> {
    /// Logical ID. It stays the same across publication retries and redrives;
    /// consumers deduplicate by it.
    pub id: String,
    /// When the domain occurrence happened.
    pub occurred_at: OffsetDateTime,
    /// Typed feature payload.
    pub payload: T,
}
