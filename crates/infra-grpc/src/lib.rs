#![forbid(unsafe_code)]

// template:begin authn:grpc-authn-module
mod authn;
// template:end authn:grpc-authn-module
mod call;
mod client;
mod error;
mod health;
mod observe;
mod protocol;
mod router;
// template:begin service-auth:grpc-service-auth-module
mod service_auth;
// template:end service-auth:grpc-service-auth-module
mod services;
mod status;
mod tls;

pub use client::{
    Client, ClientIdentity, ClientSecurity, ClientTimeout, ClientTlsMaterial, PreparedCall,
};
pub use error::{Error, TlsMaterialError};
pub use observe::HISTOGRAMS;
pub use protocol::grpc_timeout;
pub use router::{Limits, ResponseContext, router, server_options};
pub use services::Services;
// template:begin service-auth:grpc-service-auth-exports
pub use infra_serviceauth::Caller;
pub use service_auth::{REFLECTION_PATHS, ServiceTokenClient};
// template:end service-auth:grpc-service-auth-exports
pub use status::{ERROR_DOMAIN, Failure};
pub use tls::{ServerTlsMaterial, server_tls_config};
