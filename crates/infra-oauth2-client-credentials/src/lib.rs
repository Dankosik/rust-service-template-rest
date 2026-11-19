mod assertion;
mod credentials;
mod discovery;
mod error;
mod http_client;
mod options;
mod token;
mod token_endpoint;

#[cfg(test)]
mod tests;

// template:begin outbound-auth-grpc:oauth-grpc-module
#[cfg(feature = "grpc")]
pub mod grpc;
// template:end outbound-auth-grpc:oauth-grpc-module

pub use credentials::{
    EmptySubject, ExchangeTokens, OnBehalfOf, RefreshDriver, Registration, ServiceTokens,
};
pub use discovery::verify_oidc_metadata;
pub use error::{
    AcquisitionError, ConfigurationError, DiscoveryError, DiscoveryTransportError, Error,
    PreparationError, Rejection, TransportBuildFailure, TransportFailure,
};
pub use http_client::{
    ExchangeHttpClient, ExchangeRequestClient, ServiceHttpClient, ServiceRequestClient,
};
pub use options::{
    Algorithm, ExchangeTokenTarget, IssuerUrl, Options, PrivateKeyJwt, ServiceTokenTarget, TokenUrl,
};
