mod authenticate;
mod bearer;
mod claims;
mod error;
// template:begin oidc-introspection:authn-introspection-module
mod introspection;
// template:end oidc-introspection:authn-introspection-module
// template:begin oidc-jwt:authn-jwks-module
mod jwks;
// template:end oidc-jwt:authn-jwks-module
// template:begin oidc-jwt:authn-jwt-module
mod jwt;
// template:end oidc-jwt:authn-jwt-module
mod principal;
mod provider;
// template:begin oidc-jwt:authn-refresh-module
mod refresh;
// template:end oidc-jwt:authn-refresh-module
#[cfg(test)]
mod test_helpers;
#[cfg(test)]
mod test_metrics;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
mod verifier;

pub use authenticate::Transport;
pub(crate) use bearer::parse_bearer;
pub use bearer::{DelegationCredential, PresentedToken};
pub(crate) use claims::JsonObject;
// template:begin oidc-introspection:authn-introspection-error-export
pub use error::InvalidCacheOptions;
// template:end oidc-introspection:authn-introspection-error-export
// template:begin oidc-jwt:authn-jwt-error-exports
pub use error::{DiscoveryError, JwksError, PreparationError, PreparationPhase};
// template:end oidc-jwt:authn-jwt-error-exports
pub use error::{ClientBuildError, Failure, InvalidUrl, OptionsError};
pub(crate) use error::{VerificationError, VerificationReason};
// template:begin oidc-introspection:authn-introspection-prepare-export
pub use introspection::{IntrospectionCacheOptions, IntrospectionOptions, prepare_introspection};
// template:end oidc-introspection:authn-introspection-prepare-export
// template:begin oidc-jwt:authn-jwt-prepare-export
pub use jwks::JwtAlgorithm;
pub use jwt::{JwtOptions, RefreshDriver, TokenProfile, prepare_jwt};
// template:end oidc-jwt:authn-jwt-prepare-export
pub use principal::{Actor, ClaimAccessError, Principal, PrincipalIdentity};
pub use provider::{EndpointUrl, HISTOGRAMS, IssuerUrl, ProviderFailure};
pub use verifier::Verifier;
pub(crate) use verifier::unix_now;
#[cfg(test)]
pub(crate) use verifier::unix_time;
