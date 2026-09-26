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

use std::{fmt, future::Future, pin::Pin, sync::Arc};

pub use bearer::{BearerToken, parse_bearer};
// template:begin oidc-introspection:authn-introspection-prepare-export
pub use introspection::{IntrospectionCacheOptions, IntrospectionOptions, prepare_introspection};
// template:end oidc-introspection:authn-introspection-prepare-export
// template:begin oidc-jwt:authn-jwt-prepare-export
pub use jwt::{JwtAlgorithm, JwtOptions, RefreshTask, TokenProfile, prepare_jwt};
// template:end oidc-jwt:authn-jwt-prepare-export
pub use provider::ProviderUrl;

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

/// A safe preparation error with closed diagnostics. An issuer mismatch also
/// names the configured and discovered issuer URLs.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("authentication preparation failed during {phase:?}: {reason:?}{}", IssuerContext(.issuers.as_deref()))]
pub struct PreparationError {
    phase: PreparationPhase,
    reason: PreparationReason,
    issuers: Option<Box<(String, String)>>,
}

struct IssuerContext<'a>(Option<&'a (String, String)>);

impl fmt::Display for IssuerContext<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some((configured, discovered)) => write!(
                formatter,
                " (configured issuer {configured:?}, discovered issuer {discovered:?})"
            ),
            None => Ok(()),
        }
    }
}

impl PreparationError {
    pub(crate) const fn new(phase: PreparationPhase, reason: PreparationReason) -> Self {
        Self {
            phase,
            reason,
            issuers: None,
        }
    }

    /// The failed preparation stage.
    #[must_use]
    pub const fn phase(&self) -> PreparationPhase {
        self.phase
    }

    /// The closed reason for the failed stage.
    #[must_use]
    pub const fn reason(&self) -> PreparationReason {
        self.reason
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

pub(crate) fn describe_verification() {
    metrics::describe_counter!(
        "authn_token_verifications_total",
        "Token verification decisions by engine and closed reason"
    );
}

pub(crate) fn record_verification(
    mode: &'static str,
    result: Result<Principal, VerificationError>,
) -> Result<Principal, Failure> {
    let reason = result
        .as_ref()
        .map_or_else(|error| error.reason.label(), |_| "verified");
    let outcome = if result.is_ok() { "success" } else { "failure" };
    metrics::counter!("authn_token_verifications_total", "mode" => mode, "outcome" => outcome, "reason" => reason).increment(1);
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
    payload: Arc<str>,
}

impl Principal {
    pub(crate) fn new(
        issuer: String,
        subject: Option<String>,
        client_id: Option<String>,
        scopes: Vec<String>,
        expiry_epoch_seconds: u64,
        payload: Arc<str>,
    ) -> Self {
        Self {
            issuer,
            subject,
            client_id,
            scopes,
            expiry_epoch_seconds,
            payload,
        }
    }

    /// Deserializes immutable claims from the accepted provider evidence.
    ///
    /// # Errors
    /// Returns a sanitized error when the application type cannot read the claims.
    pub fn claims<T: serde::de::DeserializeOwned>(&self) -> Result<T, ClaimAccessError> {
        let value: serde_json::Value =
            serde_json::from_str(&self.payload).map_err(|_| ClaimAccessError::InvalidShape)?;
        serde_json::from_value(value).map_err(|_| ClaimAccessError::InvalidShape)
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

/// Typed claim access failed without exposing the application type or payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ClaimAccessError {
    #[error("verified claims do not match the requested type")]
    InvalidShape,
}

// template:begin oidc-introspection:authn-retained-payload
impl Principal {
    pub(crate) fn retained_bytes(&self) -> Option<usize> {
        let bytes = self
            .payload
            .len()
            .checked_add(self.issuer.capacity())?
            .checked_add(self.subject.as_ref().map_or(0, String::capacity))?
            .checked_add(self.client_id.as_ref().map_or(0, String::capacity))?
            .checked_add(
                self.scopes
                    .capacity()
                    .checked_mul(std::mem::size_of::<String>())?,
            )?;
        self.scopes
            .iter()
            .try_fold(bytes, |bytes, scope| bytes.checked_add(scope.capacity()))
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

    /// Fresh CA and named leaf material for normal TLS verification in tests.
    pub struct TlsMaterial {
        pub root_der: Vec<u8>,
        pub certificate_der: Vec<u8>,
        pub private_key_der: Vec<u8>,
    }

    impl fmt::Debug for TlsMaterial {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("TlsMaterial([REDACTED])")
        }
    }

    /// Generates a CA and leaf valid around the current wall clock.
    ///
    /// # Panics
    /// Panics when fixture key generation or certificate signing fails.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "unavailable fixture material is a test setup failure"
    )]
    pub fn tls_material(host: &str) -> TlsMaterial {
        use rcgen::{
            BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa,
            KeyPair, KeyUsagePurpose,
        };
        let now = time::OffsetDateTime::now_utc();
        let mut ca = CertificateParams::default();
        ca.not_before = now - time::Duration::days(1);
        ca.not_after = now + time::Duration::days(7);
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let issuer =
            CertifiedIssuer::self_signed(ca, KeyPair::generate().expect("generate fixture CA key"))
                .expect("sign fixture CA");
        let leaf_key = KeyPair::generate().expect("generate fixture leaf key");
        let mut leaf = CertificateParams::new(vec![host.to_owned()]).expect("fixture DNS SAN");
        leaf.not_before = now - time::Duration::days(1);
        leaf.not_after = now + time::Duration::days(7);
        leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = leaf
            .signed_by(&leaf_key, &issuer)
            .expect("sign fixture leaf");
        TlsMaterial {
            root_der: issuer.der().to_vec(),
            certificate_der: certificate.der().to_vec(),
            private_key_der: leaf_key.serialize_der(),
        }
    }

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

pub(crate) trait Engine: Send + Sync {
    fn verify<'a>(
        &'a self,
        token: &'a BearerToken<'_>,
    ) -> Pin<Box<dyn Future<Output = Result<Principal, Failure>> + Send + 'a>>;
}

/// A prepared real authentication engine.
#[derive(Clone)]
pub struct Verifier(Arc<dyn Engine>);

impl Verifier {
    pub(crate) fn new(engine: impl Engine + 'static) -> Self {
        Self(Arc::new(engine))
    }

    /// Verifies a syntactically accepted bearer token.
    ///
    /// # Errors
    /// Returns [`Failure`] for invalid token evidence or an unavailable provider.
    pub async fn verify(&self, token: &BearerToken<'_>) -> Result<Principal, Failure> {
        self.0.verify(token).await
    }
}

impl fmt::Debug for Verifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Verifier([REDACTED])")
    }
}
