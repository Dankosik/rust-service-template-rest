//! HTTP transport adapter.
//!
//! Owns the hardened middleware chain, the problem envelope, the health
//! probe handlers with their OpenAPI contract, and a bounded server over
//! hyper. It does not own business rules, configuration loading, or process
//! lifecycle; the composition root in the `service` crate wires those
//! together and assembles the full API document.
//!
//! Most of the chain is `tower-http`; the template-owned pieces and the
//! contract decisions are recorded in `docs/architecture/http.md`.

pub mod problem;

mod access_log;
mod harden;
mod probes;
mod request_id;
mod router;
mod server;

pub use harden::{
    HTTP_METRICS_NAMES, HTTP_REQUESTS_DURATION_SECONDS, HardenOptions, SHED_REQUESTS_METRIC, harden,
};
pub use problem::{Code, InvalidParam, Problem};
pub use request_id::{REQUEST_ID_HEADER, request_id};
pub use router::router;
pub use server::{CONNECTIONS_REFUSED_METRIC, Drained, Server, ServerError, ServerOptions};
