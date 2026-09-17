//! HTTP transport adapter.
//!
//! Owns the router, the health probe handlers, and the server lifecycle.
//! Does not own business rules, configuration loading, or process lifecycle;
//! the composition root in the `service` crate wires those together.

mod health;
mod router;
mod server;

pub use health::Readiness;
pub use router::router;
pub use server::{Server, ServerError};
