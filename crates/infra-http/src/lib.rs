mod body;
mod contract;
pub mod extract;
pub mod problem;
// template:begin authn:infra-http-authn-module
pub mod authn;
// template:end authn:infra-http-authn-module
// template:begin http-idempotency:infra-http-idempotency-module
pub mod idempotency;
// template:end http-idempotency:infra-http-idempotency-module

mod context;
mod harden;
#[cfg(test)]
mod listener_tests;
mod observe;
mod probes;
mod request_id;
#[cfg(test)]
mod test_support;

pub use context::{RequestContext, ResponseContext};
pub use contract::{Application, Authentication, FinalizeError, HttpContract};
pub use harden::HardenOptions;
#[cfg(test)]
use harden::harden;
pub use observe::HISTOGRAMS;
// template:begin authn:infra-http-authn-exports
pub use authn::VerifiedPrincipal;
pub use infra_bearerauthn::DelegationCredential;
// template:end authn:infra-http-authn-exports
// template:begin request-budget:infra-http-request-deadline-export
pub use context::RequestDeadline;
// template:end request-budget:infra-http-request-deadline-export
pub use probes::liveness_router;
pub use problem::{Code, InvalidParam, Problem};
pub use request_id::REQUEST_ID_HEADER;
