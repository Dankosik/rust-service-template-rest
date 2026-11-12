//! HTTP transport adapter.
//!
//! Owns the hardened middleware chain, the problem envelope, the health
//! probe handlers with their OpenAPI contract, and a bounded server over
//! hyper. It does not own business rules, configuration loading, or process
//! lifecycle; the composition root in the `service` crate wires those
//! together and assembles the full API document.
//!
//! The chain mixes `tower-http` layers (request id, response headers, panic
//! recovery, body limit) with this crate's own middleware for observation,
//! admission, the request deadline and request-id sanitizing.

mod contract;
pub mod extract;
pub mod problem;
// template:begin inbound-webhooks:http-webhooks-module
pub mod webhooks;
// template:end inbound-webhooks:http-webhooks-module
// template:begin authn:infra-http-authn-module
pub mod authn;
// template:end authn:infra-http-authn-module
// template:begin http-idempotency:infra-http-idempotency-module
pub mod idempotency;
// template:end http-idempotency:infra-http-idempotency-module

mod context;
mod harden;
mod observe;
mod probes;
mod request_id;
mod server;

pub use context::{RequestContext, ResponseContext};
pub use contract::{FinalizeError, finalize_public};
pub use harden::{HardenOptions, harden};
pub use observe::HISTOGRAMS;
// template:begin authn:infra-http-authn-exports
pub use authn::VerifiedPrincipal;
// template:end authn:infra-http-authn-exports
// template:begin request-budget:infra-http-request-deadline-export
pub use context::RequestDeadline;
// template:end request-budget:infra-http-request-deadline-export
pub use probes::{liveness_router, router};
pub use problem::{Code, InvalidParam, Problem};
pub use request_id::REQUEST_ID_HEADER;
pub use server::{
    AcceptFailure, BoundListener, Drained, HTTP2_CONNECTION_WINDOW, HTTP2_STREAM_WINDOW, Server,
    ServerError, ServerOptions,
};
