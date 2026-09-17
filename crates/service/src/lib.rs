//! The service's API contract, assembled from the routers the transport and
//! feature crates own. The `service` binary serves it (`main.rs` and
//! `bootstrap`), the `openapi` binary renders it, and the integration tests
//! hold the committed document to it.

pub mod api;
