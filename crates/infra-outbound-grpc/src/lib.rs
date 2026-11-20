mod call;
mod client;
mod error;
mod observe;
mod protocol;
// template:begin service-auth:outbound-grpc-service-auth-module
mod service_auth;
// template:end service-auth:outbound-grpc-service-auth-module
mod tls;

pub mod transport {
    pub use crate::client::{PreparedCall, bind_context};
}

pub use client::{Client, ClientBuilder, ClientSecurity, ClientTimeout, WithContext};
pub use error::{ClientError, ClientTlsError};
pub use observe::HISTOGRAMS;
// template:begin service-auth:outbound-grpc-service-auth-exports
pub use service_auth::ServiceTokenClient;
// template:end service-auth:outbound-grpc-service-auth-exports
pub use tls::{ClientIdentity, ClientTlsMaterial};
