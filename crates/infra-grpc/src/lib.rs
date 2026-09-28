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

pub use client::{Client, ClientIdentity, ClientSecurity, ClientTlsMaterial};
pub use error::Error;
pub use observe::{CLIENT_HANDLING_SECONDS, HANDLING_SECONDS_BUCKETS, SERVER_HANDLING_SECONDS};
pub use router::{CALL_DEADLINE_CAP, Services, grpc_timeout, router, server_options};
pub use status::classified_status;
pub use tls::{ServerTlsMaterial, server_tls_config};
