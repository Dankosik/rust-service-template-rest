mod call;
mod client;
mod error;
mod observe;
mod protocol;
// template:begin service-auth:outbound-grpc-service-auth-module
mod service_auth;
// template:end service-auth:outbound-grpc-service-auth-module
mod tls;

pub use client::{
    Client, ClientIdentity, ClientSecurity, ClientTimeout, ClientTlsMaterial, PreparedCall,
};
pub use error::{ClientError, TlsMaterialError};
pub use observe::HISTOGRAMS;
pub use protocol::grpc_timeout;
// template:begin service-auth:outbound-grpc-service-auth-exports
pub use service_auth::ServiceTokenClient;
// template:end service-auth:outbound-grpc-service-auth-exports
