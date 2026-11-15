//! Callers supply the ID and occurrence time: this crate mints no IDs and
//! reads no clock, so a retry cannot change an event's identity.
//!
//! The messaging adapter validates an event when it prepares it. The event
//! type and ID must be short printable text and the occurrence time must not
//! be Go's zero time, which Go consumers read as an unset field.

use time::UtcDateTime;

/// The messaging adapter asks for `serde::Serialize` where it publishes a
/// payload and for `serde::de::DeserializeOwned` where it delivers one, so a
/// type implements only the direction its service uses. It asks for
/// `utoipa::ToSchema` where a route is documented in the contract document.
pub trait EventPayload {
    /// Never rename it once published: consumers route by it.
    const EVENT_TYPE: &'static str;
    /// Bump this when the payload changes incompatibly.
    const SCHEMA_VERSION: u16;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event<T> {
    pub id: String,
    pub occurred_at: UtcDateTime,
    pub payload: T,
}
