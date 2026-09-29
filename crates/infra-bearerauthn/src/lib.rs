//! Bounded bearer authentication verification.
//!
//! This crate owns the sealed transition from an inbound bearer envelope to a
//! verified identity. HTTP routing, configuration loading, and authorization
//! policy remain with their existing owners.

mod authenticate;
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

#[cfg(test)]
#[path = "../../../test/fixtures/tls.rs"]
mod tls;

use std::{fmt, sync::Arc};

pub use authenticate::AUTHN_VERIFICATIONS_METRIC;
pub use bearer::{BearerToken, parse_bearer};
// template:begin oidc-introspection:authn-introspection-prepare-export
pub use introspection::{IntrospectionCacheOptions, IntrospectionOptions, prepare_introspection};
// template:end oidc-introspection:authn-introspection-prepare-export
// template:begin oidc-jwt:authn-jwt-prepare-export
pub use jwt::{JwtAlgorithm, JwtOptions, RefreshTask, TokenProfile, prepare_jwt};
// template:end oidc-jwt:authn-jwt-prepare-export
pub use provider::{EndpointUrl, IssuerUrl};

/// The fixed authentication outcomes exposed to the HTTP adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Failure {
    #[error("bearer authentication is required")]
    Missing,
    #[error("bearer authentication is malformed")]
    Malformed,
    #[error("bearer authentication is invalid")]
    Invalid,
    #[error("bearer authentication is unavailable")]
    Unavailable,
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

/// A safe preparation error with closed diagnostics. Only an issuer mismatch
/// names values: the configured and the discovered issuer URLs.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PreparationError {
    #[error("authentication preparation failed during {phase:?}: {reason:?}")]
    Failed {
        phase: PreparationPhase,
        reason: PreparationReason,
    },
    #[error(
        "authentication preparation failed during Discovery: IssuerMismatch \
         (configured issuer {configured:?}, discovered issuer {discovered:?})"
    )]
    IssuerMismatch {
        configured: String,
        discovered: String,
    },
}

impl PreparationError {
    pub(crate) const fn new(phase: PreparationPhase, reason: PreparationReason) -> Self {
        Self::Failed { phase, reason }
    }

    /// The failed preparation stage.
    #[must_use]
    pub const fn phase(&self) -> PreparationPhase {
        match self {
            Self::Failed { phase, .. } => *phase,
            Self::IssuerMismatch { .. } => PreparationPhase::Discovery,
        }
    }

    /// The closed reason for the failed stage.
    #[must_use]
    pub const fn reason(&self) -> PreparationReason {
        match self {
            Self::Failed { reason, .. } => *reason,
            Self::IssuerMismatch { .. } => PreparationReason::IssuerMismatch,
        }
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
    Scope,
    // template:begin oidc-jwt:authn-jwt-reasons
    Header,
    Algorithm,
    Profile,
    Signature,
    UnknownKey,
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
            Self::Scope => "scope",
            // template:begin oidc-jwt:authn-jwt-reason-labels
            Self::Header => "header",
            Self::Algorithm => "algorithm",
            Self::Profile => "profile",
            Self::Signature => "signature",
            Self::UnknownKey => "unknown_key",
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

pub(crate) fn describe_verification() {
    metrics::describe_counter!(
        "authn_token_verifications_total",
        "Token verification decisions by engine and closed reason"
    );
}

fn record_verification(
    mode: &'static str,
    verified: &metrics::Counter,
    result: Result<Principal, VerificationError>,
) -> Result<Principal, Failure> {
    let error = match result {
        Ok(principal) => {
            verified.increment(1);
            return Ok(principal);
        }
        Err(error) => error,
    };
    let reason = error.reason.label();
    metrics::counter!("authn_token_verifications_total", "mode" => mode, "outcome" => "failure", "reason" => reason)
        .increment(1);
    tracing::debug!(mode, reason, "authn_verification_failed");
    Err(error.failure)
}

/// Success counters, registered once at preparation: a success is the hot
/// path, and a registry lookup per request costs more than the increment.
/// Bootstrap installs the recorder before it prepares a verifier.
struct SuccessCounters {
    verified: metrics::Counter,
    transport: authenticate::TransportSuccessCounters,
}

impl SuccessCounters {
    fn new(mode: &'static str) -> Arc<Self> {
        Arc::new(Self {
            verified: metrics::counter!("authn_token_verifications_total", "mode" => mode, "outcome" => "success", "reason" => "verified"),
            transport: authenticate::TransportSuccessCounters::new(),
        })
    }
}

/// A verified identity. Construction remains crate-private so callers cannot
/// attach unverified request data as a principal.
#[derive(Clone, Eq, PartialEq)]
pub struct Principal {
    identity: Arc<Identity>,
    // Keep expiry outside the shared allocation for the const accessor.
    expiry_epoch_seconds: u64,
}

struct Identity {
    issuer: String,
    subject: Option<String>,
    client_id: Option<String>,
    scopes: Vec<String>,
    payload: String,
    access_token: secrecy::SecretString,
    actor: Option<Actor>,
}

// `secrecy::SecretString` intentionally has no `PartialEq`, so this compares
// its exposed text alongside the other verified fields.
impl PartialEq for Identity {
    fn eq(&self, other: &Self) -> bool {
        use secrecy::ExposeSecret;
        self.issuer == other.issuer
            && self.subject == other.subject
            && self.client_id == other.client_id
            && self.scopes == other.scopes
            && self.payload == other.payload
            && self.access_token.expose_secret() == other.access_token.expose_secret()
            && self.actor == other.actor
    }
}

impl Eq for Identity {}

impl Principal {
    #[allow(
        clippy::too_many_arguments,
        reason = "one crate-private constructor for the verified evidence every engine reads"
    )]
    pub(crate) fn new(
        issuer: String,
        subject: Option<String>,
        client_id: Option<String>,
        scopes: Vec<String>,
        expiry_epoch_seconds: u64,
        payload: String,
        access_token: secrecy::SecretString,
        actor: Option<Actor>,
    ) -> Self {
        Self {
            identity: Arc::new(Identity {
                issuer,
                subject,
                client_id,
                scopes,
                payload,
                access_token,
                actor,
            }),
            expiry_epoch_seconds,
        }
    }

    /// Deserializes immutable claims from the accepted provider evidence.
    ///
    /// # Errors
    /// Returns a sanitized error when the application type cannot read the claims.
    pub fn claims<T: serde::de::DeserializeOwned>(&self) -> Result<T, ClaimAccessError> {
        let value: serde_json::Value = serde_json::from_str(&self.identity.payload)
            .map_err(|_| ClaimAccessError::InvalidShape)?;
        serde_json::from_value(value).map_err(|_| ClaimAccessError::InvalidShape)
    }

    /// The exact issuer that the selected verifier accepted.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.identity.issuer
    }

    /// The verified subject, when the accepted token profile supplied one.
    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        self.identity.subject.as_deref()
    }

    /// The verified client identity, when the accepted token profile supplied one.
    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.identity.client_id.as_deref()
    }

    /// The normalized, exact-case scopes from verified token evidence.
    #[must_use]
    pub fn scopes(&self) -> &[String] {
        &self.identity.scopes
    }

    /// The verified `exp` claim in unsigned epoch seconds.
    #[must_use]
    pub const fn expires_at(&self) -> u64 {
        self.expiry_epoch_seconds
    }

    /// The exact bearer token text this principal was verified from. It is
    /// the subject token of an RFC 8693 exchange; never forward it as an
    /// outbound `Authorization` header.
    #[must_use]
    pub fn access_token(&self) -> &secrecy::SecretString {
        &self.identity.access_token
    }

    /// The outermost RFC 8693 `act` claim's identity, when the verified
    /// evidence named one. Nested actors are not exposed.
    #[must_use]
    pub fn actor(&self) -> Option<&Actor> {
        self.identity.actor.as_ref()
    }
}

impl fmt::Debug for Principal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Principal([VERIFIED_IDENTITY_REDACTED])")
    }
}

/// The current actor delegated to act on a subject's behalf (RFC 8693 §4.1).
/// `may_act` is an authorization-server input, not receiver evidence, and is
/// never exposed here.
#[derive(Clone, Eq, PartialEq)]
pub struct Actor {
    subject: String,
    client_id: Option<String>,
}

impl Actor {
    pub(crate) fn new(subject: String, client_id: Option<String>) -> Self {
        Self { subject, client_id }
    }

    /// The actor's non-empty `sub`.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// The actor's `client_id`, when the evidence supplied one.
    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }
}

impl fmt::Debug for Actor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Actor([REDACTED])")
    }
}

/// Typed claim access failed without exposing the application type or payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ClaimAccessError {
    #[error("verified claims do not match the requested type")]
    InvalidShape,
}

// template:begin oidc-introspection:authn-retained-payload
impl Principal {
    /// The verified provider evidence this principal retains. Its other fields
    /// are configuration or copies of values inside this payload.
    pub(crate) fn payload_len(&self) -> usize {
        self.identity.payload.len()
    }
}
// template:end oidc-introspection:authn-retained-payload

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
        ///
        /// # Errors
        ///
        /// Returns [`Failure::Unavailable`] for an invalid mapping or certificate,
        /// or when client preparation fails.
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

/// A prepared real authentication engine.
#[derive(Clone)]
pub struct Verifier {
    engine: Engine,
    counters: Arc<SuccessCounters>,
}

#[derive(Clone)]
enum Engine {
    // template:begin oidc-jwt:authn-jwt-engine
    Jwt(Arc<jwt::JwtVerifier>),
    // template:end oidc-jwt:authn-jwt-engine
    // template:begin oidc-introspection:authn-introspection-engine
    Introspection(Arc<introspection::IntrospectionVerifier>),
    // template:end oidc-introspection:authn-introspection-engine
}

impl Verifier {
    // template:begin oidc-jwt:authn-jwt-verifier
    pub(crate) fn jwt(engine: jwt::JwtVerifier) -> Self {
        Self {
            engine: Engine::Jwt(Arc::new(engine)),
            counters: SuccessCounters::new("jwt"),
        }
    }
    // template:end oidc-jwt:authn-jwt-verifier

    // template:begin oidc-introspection:authn-introspection-verifier
    pub(crate) fn introspection(engine: introspection::IntrospectionVerifier) -> Self {
        Self {
            engine: Engine::Introspection(Arc::new(engine)),
            counters: SuccessCounters::new("introspection"),
        }
    }
    // template:end oidc-introspection:authn-introspection-verifier

    /// Verifies a syntactically accepted bearer token.
    ///
    /// # Errors
    /// Returns [`Failure`] for invalid token evidence or an unavailable provider.
    pub async fn verify(&self, token: &BearerToken<'_>) -> Result<Principal, Failure> {
        let (mode, result) = match &self.engine {
            // template:begin oidc-jwt:authn-jwt-verify
            Engine::Jwt(engine) => ("jwt", engine.verify(token).await),
            // template:end oidc-jwt:authn-jwt-verify
            // template:begin oidc-introspection:authn-introspection-verify
            Engine::Introspection(engine) => ("introspection", engine.verify(token).await),
            // template:end oidc-introspection:authn-introspection-verify
        };
        record_verification(mode, &self.counters.verified, result)
    }
}

impl fmt::Debug for Verifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Verifier([REDACTED])")
    }
}
