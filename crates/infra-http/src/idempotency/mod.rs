//! Inbound HTTP idempotency: composition, authenticated capture, and replay.
//!
//! Passing a tracked route carrier to [`Composer::route`] opts it in and
//! generates the served contract; final contract authentication wraps that
//! carrier. The seam captures the verified caller, decoded key, original URI,
//! raw Content-Type values, bounded raw body, and trailers. Pre-upgrade
//! records contain no trailer identity. Until those records expire, a
//! retry with trailers gets a mismatch, while a retry without trailers can
//! still match a record written for a request with trailers.
//! Handlers validate and authorize every attempt before `Idempotency::execute`, then return that
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

/// Whether a body read failed at a length limit. axum boxes the transport
/// limit's error into [`axum::Error`] when a request has no `Content-Length`,
/// so the limit can sit anywhere in the source chain. The inbound-webhooks
/// and http-idempotency profiles each keep a copy: a shared one in
/// `problem.rs` is dead code in a projection that drops both.
fn caused_by_length_limit(error: &(dyn std::error::Error + 'static)) -> bool {
    shared_rules::error_chain::causes(error)
        .any(<dyn std::error::Error>::is::<http_body_util::LengthLimitError>)
}
