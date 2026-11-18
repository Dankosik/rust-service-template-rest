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

pub use credentials::{ExchangeTokens, OnBehalfOf, RefreshDriver, Registration, ServiceTokens};
pub use error::{AcquisitionError, ConfigurationError, Error, Rejection};
pub use http_client::{AuthenticatedClient, RequestClient};
pub use options::{Algorithm, ExchangeTokenTarget, Options, ServiceTokenTarget};
