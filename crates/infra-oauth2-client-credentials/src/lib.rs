//! Private `OAuth2` client credentials for bounded outbound integrations.
//!
//! Composition prepares one immutable credential owner and binds it to a
//! resource client. Neither access tokens, assertions, nor raw provider
//! errors leave it. The owner authenticates to its authorization server only
//! with a signed client assertion (RFC 7523), never a shared secret, and can
//! exchange a verified inbound token (RFC 8693) for one addressed to an
//! integration.

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
