//! Bearer-token authentication verification.
//!
//! This crate owns the sealed transition from an inbound bearer envelope to a
//! verified identity. HTTP routing, configuration loading, and authorization
//! policy remain with their existing owners.

mod bearer;
mod claims;
// template:begin oidc-introspection:authn-introspection-module
mod introspection;
// template:end oidc-introspection:authn-introspection-module
// template:begin oidc-jwt:authn-jwt-module
mod jwt;
// template:end oidc-jwt:authn-jwt-module
mod provider;
// template:begin oidc-jwt:authn-refresh-module
mod refresh;
// template:end oidc-jwt:authn-refresh-module

use std::fmt;
// template:begin oidc-jwt:authn-jwt-refresh-task-imports
use std::{future::Future, pin::Pin};
// template:end oidc-jwt:authn-jwt-refresh-task-imports

use tokio::time::Instant;

pub use bearer::{BearerToken, parse_bearer};
// template:begin oidc-introspection:authn-introspection-prepare-export
pub use introspection::prepare_introspection;
// template:end oidc-introspection:authn-introspection-prepare-export
// template:begin oidc-jwt:authn-jwt-prepare-export
pub use jwt::prepare_jwt;
// template:end oidc-jwt:authn-jwt-prepare-export

/// The only authentication outcomes exposed to the HTTP adapter.
///
/// The variants intentionally do not retain dependency, token, provider, or
/// endpoint details: those values are not safe to put in a response or log.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Failure {
    #[error("bearer authentication is required")]
    Missing,
    #[error("bearer authentication is malformed")]
    Malformed,
    #[error("bearer authentication is too large")]
    Oversize,
    #[error("bearer authentication is invalid")]
    Invalid,
    #[error("bearer authentication is unavailable")]
    Unavailable,
}

/// A verified identity. Construction remains crate-private so callers cannot
/// attach unverified request data as a principal.
#[derive(Clone, Eq, PartialEq)]
pub struct Principal {
    issuer: String,
    subject: Option<String>,
    client_id: Option<String>,
    #[allow(
        dead_code,
        reason = "retains verified expiry as sealed evidence; no downstream expiry policy exists"
    )]
    expiry_epoch_seconds: u64,
}

impl Principal {
    pub(crate) fn new(
        issuer: String,
        subject: Option<String>,
        client_id: Option<String>,
        expiry_epoch_seconds: u64,
    ) -> Self {
        Self {
            issuer,
            subject,
            client_id,
            expiry_epoch_seconds,
        }
    }

    /// The exact issuer that the selected verifier accepted.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The verified subject, when the accepted token profile supplied one.
    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        self.subject.as_deref()
    }

    /// The verified client identity, when the accepted token profile supplied one.
    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }
}

impl fmt::Debug for Principal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Principal([VERIFIED_IDENTITY_REDACTED])")
    }
}

// template:begin oidc-jwt:authn-token-profile
/// JWT profile rules selected by configuration before the verifier is built.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TokenProfile {
    #[default]
    ResourceServer,
    Rfc9068,
}
// template:end oidc-jwt:authn-token-profile

// template:begin oidc-jwt:authn-jwt-options
/// Bootstrap input for OIDC discovery and JWT verification.
#[derive(Clone, Eq, PartialEq)]
pub struct JwtOptions {
    pub issuer: String,
    pub audience: String,
    pub token_profile: TokenProfile,
}

impl fmt::Debug for JwtOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JwtOptions([REDACTED])")
    }
}
// template:end oidc-jwt:authn-jwt-options

// template:begin oidc-introspection:authn-introspection-options
/// Bootstrap input for RFC 7662 token introspection.
#[derive(Clone)]
pub struct IntrospectionOptions {
    pub issuer: String,
    pub audience: String,
    pub endpoint: String,
    pub client_id: String,
    pub client_secret: secrecy::SecretString,
}

impl fmt::Debug for IntrospectionOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IntrospectionOptions([REDACTED])")
    }
}
// template:end oidc-introspection:authn-introspection-options

// template:begin oidc-jwt:authn-refresh-task
/// The process-owned task that keeps a JWT verifier's installed key set fresh.
pub type RefreshTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
// template:end oidc-jwt:authn-refresh-task

/// Fixture-only transport custody for tests that exercise a real verifier.
/// It is compiled only by this crate's tests or its default-off test-support
/// feature, and cannot construct a principal or bypass verification.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use tokio_util::{sync::CancellationToken, task::TaskTracker};

    use super::{Failure, fmt, provider};

    // template:begin oidc-introspection:authn-test-support-introspection-prepare
    pub use crate::introspection::prepare_introspection_with_fixture;
    // template:end oidc-introspection:authn-test-support-introspection-prepare

    /// Fixture-only custody for a real verifier transport. It is unavailable
    /// from ordinary builds and has no principal-construction or bypass API.
    pub struct FixtureTransport {
        provider: provider::ProviderClient,
    }

    impl FixtureTransport {
        /// Creates the sole fixture mapping: one named TLS endpoint to loopback.
        ///
        /// # Errors
        ///
        /// Returns [`Failure::Unavailable`] for an invalid fixture root, host,
        /// address, or fixture transport setup.
        pub fn new(
            tracker: TaskTracker,
            cancel: CancellationToken,
            fixture_host: &str,
            fixture_addr: std::net::SocketAddr,
            fixture_root_der: &[u8],
        ) -> Result<Self, Failure> {
            provider::new_fixture_client(
                tracker,
                cancel,
                fixture_host,
                fixture_addr,
                fixture_root_der,
            )
            .map(|provider| Self { provider })
        }

        pub(crate) fn into_provider(self) -> provider::ProviderClient {
            self.provider
        }
    }

    impl fmt::Debug for FixtureTransport {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("FixtureTransport([REDACTED])")
        }
    }
}

/// A prepared authentication engine.
#[derive(Clone)]
pub enum Verifier {
    Disabled,
    // template:begin oidc-jwt:authn-verifier-jwt-variant
    Jwt(jwt::JwtVerifier),
    // template:end oidc-jwt:authn-verifier-jwt-variant
    // template:begin oidc-introspection:authn-verifier-introspection-variant
    Introspection(introspection::IntrospectionVerifier),
    // template:end oidc-introspection:authn-verifier-introspection-variant
}

impl Verifier {
    /// Returns the deliberately unavailable verifier used by disabled profiles
    /// and pure OpenAPI document assembly.
    #[must_use]
    pub const fn disabled() -> Self {
        Self::Disabled
    }

    /// Verifies a syntactically accepted bearer token before the enclosing
    /// request deadline.
    ///
    /// # Errors
    ///
    /// Returns the fixed authentication failure class without retaining token,
    /// claim, credential, key, or provider details.
    pub async fn verify(
        &self,
        token: &BearerToken<'_>,
        deadline: Instant,
    ) -> Result<Principal, Failure> {
        match self {
            Self::Disabled => Err(Failure::Unavailable),
            // template:begin oidc-jwt:authn-verifier-jwt-dispatch
            Self::Jwt(verifier) => verifier.verify(token, deadline).await,
            // template:end oidc-jwt:authn-verifier-jwt-dispatch
            // template:begin oidc-introspection:authn-verifier-introspection-dispatch
            Self::Introspection(verifier) => verifier.verify(token, deadline).await,
            // template:end oidc-introspection:authn-verifier-introspection-dispatch
        }
    }
}

impl fmt::Debug for Verifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("Verifier::Disabled"),
            // template:begin oidc-jwt:authn-verifier-jwt-debug
            Self::Jwt(_) => formatter.write_str("Verifier::Jwt(..)"),
            // template:end oidc-jwt:authn-verifier-jwt-debug
            // template:begin oidc-introspection:authn-verifier-introspection-debug
            Self::Introspection(_) => formatter.write_str("Verifier::Introspection(..)"),
            // template:end oidc-introspection:authn-verifier-introspection-debug
        }
    }
}
