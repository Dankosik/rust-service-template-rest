//! Inbound HTTP idempotency: composition, authenticated capture, and replay.
//!
//! Passing a tracked route carrier to [`Composer::route`] opts it in and
//! generates the served contract; final contract authentication wraps that
//! carrier. The seam captures the verified caller, decoded key, original URI,
//! raw Content-Type values, and bounded raw body. Handlers validate and
//! authorize every attempt before `Idempotency::execute`, then return that
//! response (or preserve its extensions) so the boundary can seal replay
//! provenance. This detects route miswiring; it cannot roll back effects a
//! handler performs outside the seam.

mod compose;
mod declaration;
mod execute;
mod identity;
mod openapi;
mod stored;

pub use compose::{Activation, Composer};
pub use declaration::CompositionError;
pub use execute::{HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Idempotency};
pub use infra_postgres::Tx;
pub use stored::{MAX_STORED_BODY_BYTES, MAX_STORED_HEADER_BYTES};
