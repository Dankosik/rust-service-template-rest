#![forbid(unsafe_code)]

// template:begin authn:grpc-authn-module
mod authn;
// template:end authn:grpc-authn-module
mod call;
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

// template:begin authn:grpc-authn-exports
pub use authn::VerifiedPrincipal;
// template:end authn:grpc-authn-exports
pub use error::{Error, TlsMaterialError};
pub use observe::HISTOGRAMS;
pub use router::{Limits, ResponseContext, router, server_options};
// template:begin service-auth:grpc-service-auth-exports
pub use service_auth::{Caller, REFLECTION_PATHS};
// template:end service-auth:grpc-service-auth-exports
pub use services::Services;
pub use status::{ERROR_DOMAIN, Failure};
pub use tls::{ServerTlsMaterial, server_tls_config};
