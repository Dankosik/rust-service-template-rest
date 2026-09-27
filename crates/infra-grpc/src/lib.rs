//! Native gRPC transport over tonic and the shared HTTP listener.
//!
//! Application crates register generated services and serve them through
//! [`router`]. This crate owns authentication, deadlines, health, listener
//! security, and outbound channels.

#![forbid(unsafe_code)]

mod client;
mod error;
mod health;
mod observe;
mod router;
mod status;
mod tls;

pub use client::{Client, ClientSecurity, ClientTlsMaterial};
pub use error::Error;
pub use router::{Services, UNARY_DEADLINE, grpc_timeout, router, server_options};
pub use status::classified_status;
pub use tls::{ServerTlsMaterial, server_tls_config};
