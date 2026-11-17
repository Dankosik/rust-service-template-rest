mod assertion;
mod credentials;
mod error;
mod http_client;
mod options;
mod request_admission;
mod token;
mod token_endpoint;

#[cfg(test)]
mod tests;

// template:begin outbound-auth-grpc:oauth-grpc-module
#[cfg(feature = "grpc")]
pub mod grpc;
// template:end outbound-auth-grpc:oauth-grpc-module

pub use credentials::{Credentials, OnBehalfOf, RefreshDriver};
pub use error::{AcquisitionError, ConfigurationError, Error, Rejection};
pub use http_client::AuthenticatedClient;
pub use options::{Algorithm, Options};
pub use request_admission::IdentityMode;
