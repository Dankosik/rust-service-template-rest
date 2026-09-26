//! Inbound HTTP idempotency: composition, authenticated capture, and replay.
//!
//! Passing a route tuple to `Composer::route` opts it in and generates the
//! served contract. The seam captures the verified caller, decoded key,
//! original URI, raw Content-Type values, and bounded raw body. Handlers
//! validate and authorize every attempt before `Idempotency::execute`.

mod compose;
mod declaration;
mod execute;
mod identity;
mod openapi;
mod stored;

pub use compose::{Activation, Composer};
pub use declaration::AgreementError;
pub use execute::{HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Idempotency};
pub use infra_postgres::Tx;
pub use stored::{MAX_STORED_BODY_BYTES, MAX_STORED_HEADER_BYTES};
