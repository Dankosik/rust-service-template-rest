//! Native gRPC transport over tonic and the shared HTTP listener.
//!
//! Application crates register generated services and serve them through
//! [`router()`]. This crate owns authentication, deadlines, health, listener
//! security, and outbound channels.

#![forbid(unsafe_code)]

mod call;
mod client;
mod error;
mod health;
mod observe;
mod router;
// template:begin service-auth:grpc-service-auth-module
mod service_auth;
// template:end service-auth:grpc-service-auth-module
mod status;
mod tls;

pub use client::{
    Client, ClientIdentity, ClientSecurity, ClientTimeout, ClientTlsMaterial, PreparedCall,
};
pub use error::Error;
pub use observe::{CLIENT_HANDLING_SECONDS, HANDLING_SECONDS_BUCKETS, SERVER_HANDLING_SECONDS};
pub use router::{
    Limits, ResponseContext, Services, grpc_timeout, metadata_grpc_timeout, router, server_options,
};
// template:begin service-auth:grpc-service-auth-exports
pub use infra_serviceauth::Caller;
pub use service_auth::{SERVICE_AUTH_CALLS_METRIC, ServiceTokenClient};
// template:end service-auth:grpc-service-auth-exports
pub use status::{ERROR_DOMAIN, Failure};
pub use tls::{ServerTlsMaterial, server_tls_config};
