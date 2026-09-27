//! Governed native gRPC transport.
//!
//! Application crates use generated contracts and [`Services`].  This crate
//! owns the policy surrounding those generated services: registration,
//! authentication, validation, terminal status provenance, resource lifetime,
//! listener security, and outbound custody.

#![forbid(unsafe_code)]

mod body;
mod call;
mod client;
mod codec;
mod error;
mod health;
mod observe;
mod registration;
mod server;
mod status;
mod tls;
mod validation;

pub use client::{Client, ClientSecurity, ClientTlsMaterial, Operation};
pub use error::Error;
pub use registration::Services;
pub use server::{
    BoundServer, PreparedServer, RunningServer, Server, ServerOptions, ServerSecurity,
};
pub use status::classified_status;
pub use tls::ServerTlsMaterial;

#[doc(hidden)]
pub use registration::{Cardinality, Method, ServiceDescriptor};

/// Source-level policy interface emitted alongside generated contracts.
///
/// It is intentionally hidden from normal application documentation: feature
/// adapters use their generated native trait, while generated code alone uses
/// this seam to prevent a method from bypassing the transport boundary.
#[doc(hidden)]
pub mod generated {
    pub use crate::call::{GovernedService, GuardedStream, guard_stream, guard_unary};
    pub use crate::codec::{BoundedClientCodec, ValidatedCodec};
    pub use crate::registration::{Cardinality, Method, ServiceDescriptor};
}
