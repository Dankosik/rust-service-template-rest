//! Inbound HTTP idempotency: the composition and handler seam.
//!
//! An operation opts in by declaring `x-idempotent: true` in its OpenAPI
//! metadata and by passing its `routes!` tuple to [`Composer::route`]. The
//! composer checks the declaration, layers key handling inside the existing
//! bearer authentication, and [`Composer::agree`] refuses a document whose
//! idempotent operations and composed routes differ. The handler takes the
//! [`Idempotency`] extractor, authorizes the attempt, and runs its work
//! through [`Idempotency::execute`] with a [`Fingerprint`] of its semantic
//! input; the work's PostgreSQL writes go through the opaque [`Tx`], so
//! they commit exactly when the stored success does.
//!
//! The scope of a key is the verified caller, the `operationId`, and the key
//! itself, digested before it leaves this module. The record store
//! (`infra_idempotency_store`) arbitrates and keeps the digests and the
//! stored success; this module owns everything HTTP-shaped: key grammar,
//! digests, stored-success encoding, Problem mapping, the outcome counter,
//! and the contract types adopters name in annotations.

mod compose;
mod declaration;
mod execute;
mod fingerprint;
mod identity;
mod openapi;
mod stored;

pub use compose::{Activation, Composer};
pub use declaration::AgreementError;
pub use execute::{HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Idempotency};
pub use fingerprint::{Fingerprint, FingerprintError};
pub use infra_idempotency_store::Tx;
pub use openapi::{
    IdempotencyBadRequest, IdempotencyKey, IdempotencyKeyMismatch, IdempotencyRequestInProgress,
    IdempotencyUnavailable, IdempotentOperationProblemResponses,
};
pub use stored::{MAX_STORED_BODY_BYTES, MAX_STORED_HEADER_BYTES};
