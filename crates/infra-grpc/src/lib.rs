// template:begin authn:grpc-authn-module
mod authn;
// template:end authn:grpc-authn-module
mod call;
mod error;
mod health;
mod observe;
mod protocol;
mod request;
mod router;
// template:begin service-auth:grpc-service-auth-module
mod service_auth;
// template:end service-auth:grpc-service-auth-module
mod services;
mod status;
mod tls;

// template:begin authn:grpc-authn-exports
pub use infra_bearerauthn::{
    Actor, ClaimAccessError, DelegationCredential, Principal, PrincipalIdentity,
};
// template:end authn:grpc-authn-exports
pub use error::{Error, TlsMaterialError, TlsRequiredError};
pub use observe::HISTOGRAMS;
pub use request::{RequestExt, ResponseContext};
pub use router::{Application, Authentication, Limits, ServerOptions, server_options};
// template:begin service-auth:grpc-service-auth-exports
pub use infra_serviceauth::Caller;
// template:end service-auth:grpc-service-auth-exports
pub use services::{ListenerService, ReflectionRegistration, ServiceRegistration, Services};
pub use status::{ERROR_DOMAIN, Failure};
pub use tls::{ServerTlsMaterial, server_tls_config};
