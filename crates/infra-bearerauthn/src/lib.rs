//! Bounded bearer authentication verification.
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
// template:begin oidc-jwt:authn-jwt-task-imports
use std::{future::Future, pin::Pin};
// template:end oidc-jwt:authn-jwt-task-imports
// template:begin oidc-introspection:authn-concurrency-import
use std::num::NonZeroUsize;
// template:end oidc-introspection:authn-concurrency-import

use tokio::time::Instant;

pub use bearer::{BearerToken, parse_bearer};
// template:begin oidc-introspection:authn-introspection-prepare-export
pub use introspection::prepare_introspection;
// template:end oidc-introspection:authn-introspection-prepare-export
// template:begin oidc-jwt:authn-jwt-prepare-export
pub use jwt::prepare_jwt;
// template:end oidc-jwt:authn-jwt-prepare-export
pub use provider::ProviderUrl;

/// The fixed authentication outcomes exposed to the HTTP adapter.
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
    #[error("bearer authentication exceeded the request deadline")]
    Timeout,
}

/// A safe preparation phase for operator diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparationPhase {
    Options,
    Client,
    Discovery,
    Jwks,
}

/// A closed preparation reason. These variants never retain dependency errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparationReason {
    InvalidUrl,
    Client,
    Fetch,
    Parse,
    IssuerMismatch,
    NoUsableKeys,
}

/// A safe preparation error for bootstrap's startup diagnostics.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("authentication preparation failed during {phase:?}: {reason:?}{context}")]
pub struct PreparationError {
    phase: PreparationPhase,
    reason: PreparationReason,
    context: PreparationContext,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PreparationContext(Option<Box<SafeContext>>);

#[derive(Clone, Debug, Eq, PartialEq)]
struct SafeContext {
    issuer: String,
    audiences: Vec<String>,
    endpoint: Option<String>,
    discovered_issuer: Option<String>,
}

impl fmt::Display for PreparationContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(context) = &self.0 {
            write!(
                formatter,
                "; issuer={:?}; audiences={:?}; endpoint={:?}; discovered_issuer={:?}",
                context.issuer, context.audiences, context.endpoint, context.discovered_issuer
            )?;
        }
        Ok(())
    }
}

impl PreparationError {
    pub(crate) const fn new(phase: PreparationPhase, reason: PreparationReason) -> Self {
        Self {
            phase,
            reason,
            context: PreparationContext(None),
        }
    }

    pub(crate) fn with_context(
        mut self,
        issuer: &ProviderUrl,
        audiences: &[String],
        endpoint: Option<&str>,
    ) -> Self {
        let discovered_issuer = self.context.0.and_then(|context| context.discovered_issuer);
        let mut safe_audiences = audiences
            .iter()
            .take(8)
            .map(|value| {
                if !value.is_empty()
                    && value.len() <= 128
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-._:/".contains(&byte))
                {
                    value.clone()
                } else {
                    "<unsafe_or_overlong>".to_owned()
                }
            })
            .collect::<Vec<_>>();
        if audiences.len() > 8 {
            safe_audiences.push("<additional_audiences>".to_owned());
        }
        self.context = PreparationContext(Some(Box::new(SafeContext {
            issuer: safe_url_context(issuer.as_str()),
            audiences: safe_audiences,
            endpoint: endpoint.map(safe_url_context),
            discovered_issuer,
        })));
        self
    }

    // template:begin oidc-jwt:authn-discovered-issuer-context
    pub(crate) fn with_discovered_issuer(mut self, issuer: &str) -> Self {
        if let Some(context) = &mut self.context.0 {
            context.discovered_issuer = Some(safe_url_context(issuer));
        }
        self
    }
    // template:end oidc-jwt:authn-discovered-issuer-context

    /// The failed bounded preparation stage.
    #[must_use]
    pub const fn phase(&self) -> PreparationPhase {
        self.phase
    }

    /// The safe closed reason for the failed stage.
    #[must_use]
    pub const fn reason(&self) -> PreparationReason {
        self.reason
    }

    /// The bounded configured issuer, when preparation had admitted options.
    #[must_use]
    pub fn issuer(&self) -> Option<&str> {
        self.context
            .0
            .as_ref()
            .map(|context| context.issuer.as_str())
    }

    /// Bounded configured audience context; unsafe values are replaced by a reason.
    #[must_use]
    pub fn audiences(&self) -> &[String] {
        self.context
            .0
            .as_ref()
            .map_or(&[], |context| context.audiences.as_slice())
    }

    /// The bounded endpoint associated with the failed phase.
    #[must_use]
    pub fn endpoint(&self) -> Option<&str> {
        self.context
            .0
            .as_ref()
            .and_then(|context| context.endpoint.as_deref())
    }

    /// Safe discovered issuer evidence for an issuer mismatch.
    #[must_use]
    pub fn discovered_issuer(&self) -> Option<&str> {
        self.context
            .0
            .as_ref()
            .and_then(|context| context.discovered_issuer.as_deref())
    }
}

fn safe_url_context(value: &str) -> String {
    if value.len() <= 256 && value.is_ascii() && ProviderUrl::parse(value).is_ok() {
        value.to_owned()
    } else {
        "<unsafe_or_overlong_url>".to_owned()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VerificationReason {
    MissingClaim,
    MalformedClaims,
    Issuer,
    Audience,
    Expired,
    NotYetValid,
    Identity,
    Scope,
    RequestTimeout,
    // template:begin oidc-jwt:authn-jwt-reasons
    Header,
    Algorithm,
    Profile,
    Signature,
    UnknownKey,
    AmbiguousKey,
    Refresh,
    // template:end oidc-jwt:authn-jwt-reasons
    // template:begin oidc-introspection:authn-introspection-reasons
    Provider,
    Inactive,
    Capacity,
    // template:end oidc-introspection:authn-introspection-reasons
}

impl VerificationReason {
    fn label(self) -> &'static str {
        match self {
            Self::MissingClaim => "missing_claim",
            Self::MalformedClaims => "malformed_claims",
            Self::Issuer => "issuer",
            Self::Audience => "audience",
            Self::Expired => "expired",
            Self::NotYetValid => "not_yet_valid",
            Self::Identity => "identity",
            Self::Scope => "scope",
            Self::RequestTimeout => "request_timeout",
            // template:begin oidc-jwt:authn-jwt-reason-labels
            Self::Header => "header",
            Self::Algorithm => "algorithm",
            Self::Profile => "profile",
            Self::Signature => "signature",
            Self::UnknownKey => "unknown_key",
            Self::AmbiguousKey => "ambiguous_key",
            Self::Refresh => "refresh",
            // template:end oidc-jwt:authn-jwt-reason-labels
            // template:begin oidc-introspection:authn-introspection-reason-labels
            Self::Provider => "provider",
            Self::Inactive => "inactive",
            Self::Capacity => "capacity",
            // template:end oidc-introspection:authn-introspection-reason-labels
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VerificationError {
    pub(crate) failure: Failure,
    pub(crate) reason: VerificationReason,
}

impl VerificationError {
    pub(crate) const fn new(failure: Failure, reason: VerificationReason) -> Self {
        Self { failure, reason }
    }
    pub(crate) const fn invalid(reason: VerificationReason) -> Self {
        Self::new(Failure::Invalid, reason)
    }
}

pub(crate) fn record_verification(
    mode: &'static str,
    result: Result<Principal, VerificationError>,
) -> Result<Principal, Failure> {
    let reason = result
        .as_ref()
        .map_or_else(|error| error.reason.label(), |_| "verified");
    let outcome = if result.is_ok() { "success" } else { "failure" };
    metrics::describe_counter!(
        "authn_verifications_total",
        "Authentication decisions by engine and closed reason"
    );
    metrics::counter!("authn_verifications_total", "mode" => mode, "outcome" => outcome, "reason" => reason).increment(1);
    if result.is_err() {
        tracing::debug!(mode, reason, "authn_verification_failed");
    }
    result.map_err(|error| error.failure)
}

/// A verified identity. Construction remains crate-private so callers cannot
/// attach unverified request data as a principal.
#[derive(Clone, Eq, PartialEq)]
pub struct Principal {
    issuer: String,
    subject: Option<String>,
    client_id: Option<String>,
    scopes: Vec<String>,
    expiry_epoch_seconds: u64,
}

impl Principal {
    pub(crate) fn new(
        issuer: String,
        subject: Option<String>,
        client_id: Option<String>,
        scopes: Vec<String>,
        expiry_epoch_seconds: u64,
    ) -> Self {
        Self {
            issuer,
            subject,
            client_id,
            scopes,
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

    /// The normalized, exact-case scopes from verified token evidence.
    #[must_use]
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }

    /// The verified `exp` claim in unsigned epoch seconds.
    #[must_use]
    pub const fn expires_at(&self) -> u64 {
        self.expiry_epoch_seconds
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

/// The closed JWT algorithms accepted by authentication configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum JwtAlgorithm {
    Rs256,
    Es256,
    Ps256,
    EdDsa,
}
// template:end oidc-jwt:authn-token-profile

// template:begin oidc-jwt:authn-jwt-options
/// Bootstrap input for OIDC discovery and JWT verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JwtOptions {
    pub issuer: ProviderUrl,
    pub audiences: Vec<String>,
    pub token_profile: TokenProfile,
    pub algorithms: Vec<JwtAlgorithm>,
}
// template:end oidc-jwt:authn-jwt-options

// template:begin oidc-introspection:authn-introspection-options
/// Bootstrap input for RFC 7662 token introspection.
#[derive(Clone)]
pub struct IntrospectionOptions {
    pub issuer: ProviderUrl,
    pub audiences: Vec<String>,
    pub endpoint: ProviderUrl,
    pub client_id: String,
    pub client_secret: secrecy::SecretString,
    pub provider_concurrency: NonZeroUsize,
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
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use tokio_util::sync::CancellationToken;

    use super::{Failure, fmt, provider};

    // template:begin oidc-introspection:authn-test-support-introspection-prepare
    pub use crate::introspection::prepare_introspection_with_fixture;
    // template:end oidc-introspection:authn-test-support-introspection-prepare

    /// Fixture-only custody for a real verifier transport. It cannot construct
    /// a principal or bypass verification.
    pub struct FixtureTransport {
        provider: provider::ProviderClient,
    }

    impl FixtureTransport {
        /// Creates the sole fixture mapping: one named TLS endpoint to loopback.
        pub fn new(
            fixture_host: &str,
            fixture_addr: std::net::SocketAddr,
            fixture_root_der: &[u8],
            cancel: CancellationToken,
        ) -> Result<Self, Failure> {
            provider::new_fixture_client(fixture_host, fixture_addr, fixture_root_der, cancel)
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
    #[must_use]
    pub const fn disabled() -> Self {
        Self::Disabled
    }

    /// Whether this verifier was prepared with an authentication engine.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Verifies a syntactically accepted bearer token before its absolute deadline.
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
