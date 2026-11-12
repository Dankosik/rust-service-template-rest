//! Bounded bearer authentication verification.
//!
//! This crate owns the sealed transition from an inbound bearer envelope to a
//! verified identity. HTTP routing, configuration loading, and authorization
//! policy remain with their existing owners.
//!
//! Transports enter only through [`Verifier::authenticate_with_context`]: parse one bearer
//! envelope, run the prepared engine, and record the transport outcome even on cancellation.
//! It returns a sealed [`Principal`] or a closed [`Failure`].

mod authenticate;
mod bearer;
mod claims;
// template:begin oidc-introspection:authn-introspection-module
mod introspection;
// template:end oidc-introspection:authn-introspection-module
// template:begin oidc-jwt:authn-jwks-module
mod jwks;
// template:end oidc-jwt:authn-jwks-module
// template:begin oidc-jwt:authn-jwt-module
mod jwt;
// template:end oidc-jwt:authn-jwt-module
mod provider;
// template:begin oidc-jwt:authn-refresh-module
mod refresh;
// template:end oidc-jwt:authn-refresh-module
#[cfg(test)]
mod test_metrics;

#[cfg(test)]
#[path = "../../../test/fixtures/tls.rs"]
mod tls;

use std::sync::Arc;

use operation_context::OperationContext;

struct JsonObject<T>(T);

impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for JsonObject<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor<T>(std::marker::PhantomData<T>);

        impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for ObjectVisitor<T> {
            type Value = JsonObject<T>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(JsonObject)
            }
        }

        deserializer.deserialize_map(ObjectVisitor(std::marker::PhantomData))
    }
}

pub use authenticate::{AUTHN_VERIFICATIONS_METRIC, Transport};
pub use bearer::{BearerToken, parse_bearer};
// template:begin oidc-introspection:authn-introspection-prepare-export
pub use introspection::{IntrospectionCacheOptions, IntrospectionOptions, prepare_introspection};
// template:end oidc-introspection:authn-introspection-prepare-export
// template:begin oidc-jwt:authn-jwt-prepare-export
pub use jwks::JwtAlgorithm;
pub use jwt::{JwtOptions, RefreshTask, TokenProfile, prepare_jwt};
// template:end oidc-jwt:authn-jwt-prepare-export
pub use provider::{EndpointUrl, HISTOGRAMS, IssuerUrl, ProviderFailure};

/// The fixed authentication outcomes each inbound transport maps to its response.
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
    // template:begin oidc-jwt:authn-jwt-preparation-phases
    Discovery,
    Jwks,
    // template:end oidc-jwt:authn-jwt-preparation-phases
}

/// A closed preparation reason. These variants never retain dependency errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparationReason {
    InvalidUrl,
    ClientBuild,
    /// An option is empty or outside its accepted range.
    InvalidOption,
    // template:begin oidc-jwt:authn-jwt-preparation-reasons
    Fetch(ProviderFailure),
    Timeout,
    /// A provider document could not be read.
    Parse,
    IssuerMismatch,
    NoUsableKeys,
    // template:end oidc-jwt:authn-jwt-preparation-reasons
}

/// Only an issuer mismatch names values: the configured and discovered issuer URLs.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PreparationError {
    #[error("authentication preparation failed during {phase:?}: {reason:?}")]
    Failed {
        phase: PreparationPhase,
        reason: PreparationReason,
    },
    // template:begin oidc-jwt:authn-jwt-issuer-mismatch
    #[error(
        "authentication preparation failed during Discovery: IssuerMismatch \
         (configured issuer {configured:?}, discovered issuer {discovered:?})"
    )]
    IssuerMismatch {
        configured: String,
        discovered: String,
    },
    // template:end oidc-jwt:authn-jwt-issuer-mismatch
}

impl PreparationError {
    pub(crate) const fn new(phase: PreparationPhase, reason: PreparationReason) -> Self {
        Self::Failed { phase, reason }
    }

    pub(crate) const fn invalid_options() -> Self {
        Self::new(PreparationPhase::Options, PreparationReason::InvalidOption)
    }

    #[must_use]
    pub const fn phase(&self) -> PreparationPhase {
        match self {
            Self::Failed { phase, .. } => *phase,
            // template:begin oidc-jwt:authn-jwt-issuer-mismatch-phase
            Self::IssuerMismatch { .. } => PreparationPhase::Discovery,
            // template:end oidc-jwt:authn-jwt-issuer-mismatch-phase
        }
    }

    #[must_use]
    pub const fn reason(&self) -> PreparationReason {
        match self {
            Self::Failed { reason, .. } => *reason,
            // template:begin oidc-jwt:authn-jwt-issuer-mismatch-reason
            Self::IssuerMismatch { .. } => PreparationReason::IssuerMismatch,
            // template:end oidc-jwt:authn-jwt-issuer-mismatch-reason
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
    Clock,
    // template:begin oidc-jwt:authn-jwt-reasons
    Header,
    Algorithm,
    Profile,
    Signature,
    UnknownKey,
    Refresh,
    // template:end oidc-jwt:authn-jwt-reasons
    // template:begin oidc-introspection:authn-introspection-reasons
    Provider(ProviderFailure),
    Inactive,
    Capacity,
    CacheState,
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
            Self::Clock => "clock",
            // template:begin oidc-jwt:authn-jwt-reason-labels
            Self::Header => "header",
            Self::Algorithm => "algorithm",
            Self::Profile => "profile",
            Self::Signature => "signature",
            Self::UnknownKey => "unknown_key",
            Self::Refresh => "refresh",
            // template:end oidc-jwt:authn-jwt-reason-labels
            // template:begin oidc-introspection:authn-introspection-reason-labels
            Self::Provider(failure) => failure.label(),
            Self::Inactive => "inactive",
            Self::Capacity => "capacity",
            Self::CacheState => "introspection_cache_state",
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
    pub(crate) const fn unavailable(reason: VerificationReason) -> Self {
        Self {
            failure: Failure::Unavailable,
            reason,
        }
    }

    pub(crate) const fn invalid(reason: VerificationReason) -> Self {
        Self {
            failure: Failure::Invalid,
            reason,
        }
    }
}

/// Usable time since the Unix epoch; clock failure cannot supply trust evidence.
pub(crate) fn unix_now() -> Result<std::time::Duration, VerificationError> {
    unix_time(std::time::SystemTime::now())
}

fn unix_time(now: std::time::SystemTime) -> Result<std::time::Duration, VerificationError> {
    now.duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| VerificationError::unavailable(VerificationReason::Clock))
}

const TOKEN_VERIFICATIONS_METRIC: &str = "authn_token_verifications_total";

fn record_verification(
    counters: &SuccessCounters,
    result: Result<Principal, VerificationError>,
) -> Result<Principal, Failure> {
    let error = match result {
        Ok(principal) => {
            counters.verified.increment(1);
            return Ok(principal);
        }
        Err(error) => error,
    };
    let mode = counters.mode;
    let reason = error.reason.label();
    metrics::counter!(TOKEN_VERIFICATIONS_METRIC, "mode" => mode, "outcome" => "failure", "reason" => reason)
        .increment(1);
    tracing::debug!(mode, reason, "authn_verification_failed");
    Err(error.failure)
}

/// Success counters, registered once at preparation: a success is the hot
/// path, and a registry lookup per request costs more than the increment.
/// Bootstrap installs the recorder before it prepares a verifier.
struct SuccessCounters {
    /// The engine label every verification decision of this verifier carries.
    mode: &'static str,
    verified: metrics::Counter,
    transport: authenticate::TransportSuccessCounters,
}

impl SuccessCounters {
    fn new(mode: &'static str) -> Arc<Self> {
        metrics::describe_counter!(
            TOKEN_VERIFICATIONS_METRIC,
            "Token verification decisions by engine and closed reason"
        );
        Arc::new(Self {
            mode,
            verified: metrics::counter!(TOKEN_VERIFICATIONS_METRIC, "mode" => mode, "outcome" => "success", "reason" => "verified"),
            transport: authenticate::TransportSuccessCounters::new(),
        })
    }
}

/// A verified identity. Construction remains crate-private so callers cannot
/// attach unverified request data as a principal.
#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("Principal([VERIFIED_IDENTITY_REDACTED])")]
pub struct Principal {
    identity: Arc<Identity>,
    // Keep expiry outside the shared allocation for the const accessor.
    expiry_epoch_seconds: u64,
}

/// The verified evidence every engine supplies for one principal.
pub(crate) struct Identity {
    pub(crate) issuer: String,
    pub(crate) subject: Option<String>,
    pub(crate) client_id: Option<String>,
    pub(crate) scopes: claims::Scopes,
    pub(crate) payload: String,
    pub(crate) access_token: secrecy::SecretString,
    pub(crate) actor: Option<Actor>,
}

// `secrecy::SecretString` intentionally has no `PartialEq`, so this compares
// its exposed text alongside the other verified fields.
impl PartialEq for Identity {
    fn eq(&self, other: &Self) -> bool {
        use secrecy::ExposeSecret;
        let Self {
            issuer,
            subject,
            client_id,
            scopes,
            payload,
            access_token,
            actor,
        } = self;
        issuer == &other.issuer
            && subject == &other.subject
            && client_id == &other.client_id
            && scopes == &other.scopes
            && payload == &other.payload
            && access_token.expose_secret() == other.access_token.expose_secret()
            && actor == &other.actor
    }
}

impl Eq for Identity {}

impl Principal {
    pub(crate) fn new(identity: Identity, expiry_epoch_seconds: u64) -> Self {
        Self {
            identity: Arc::new(identity),
            expiry_epoch_seconds,
        }
    }

    /// Deserializes immutable claims from the accepted provider evidence.
    /// Duplicate custom members use the last value, before decoding the requested type.
    ///
    /// # Errors
    /// Returns a sanitized error when the application type cannot read the claims.
    pub fn claims<T: serde::de::DeserializeOwned>(&self) -> Result<T, ClaimAccessError> {
        // The Value pass preserves last-member-wins semantics. Decoding T directly
        // from JSON would reject duplicate fields in application structs.
        let value: serde_json::Value = serde_json::from_str(&self.identity.payload)
            .map_err(|_| ClaimAccessError::InvalidShape)?;
        serde_json::from_value(value).map_err(|_| ClaimAccessError::InvalidShape)
    }

    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.identity.issuer
    }

    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        self.identity.subject.as_deref()
    }

    /// The first non-empty `client_id`, `azp`, `appid`, or `cid` claim of a
    /// JWT, or the `client_id` member of an introspection response. An empty
    /// value is absent. A principal always has a subject, a client id, or both.
    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.identity.client_id.as_deref()
    }

    /// The normalized, exact-case scopes from verified token evidence.
    #[must_use]
    pub fn scopes(&self) -> &[String] {
        self.identity.scopes.as_slice()
    }

    /// True when every scope in `required` was granted; an empty `required`
    /// is any authenticated caller.
    #[must_use]
    pub fn has_scopes(&self, required: &[String]) -> bool {
        self.identity.scopes.contains_all(required)
    }

    /// The verified `exp` claim as Unix time in seconds, without leeway.
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

/// The current actor delegated to act on a subject's behalf (RFC 8693 §4.1).
/// `may_act` is an authorization-server input, not receiver evidence, and is
/// never exposed here.
#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("Actor([REDACTED])")]
pub struct Actor {
    subject: String,
    client_id: Option<String>,
}

impl Actor {
    pub(crate) fn new(subject: String, client_id: Option<String>) -> Self {
        Self { subject, client_id }
    }

    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
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
    pub(crate) fn payload_len(&self) -> usize {
        self.identity.payload.len()
    }
}
// template:end oidc-introspection:authn-retained-payload

// template:begin oidc-introspection:authn-introspection-test-support
/// Fixture-only transport custody for tests that exercise a real verifier.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use tokio_util::sync::CancellationToken;

    use super::{Failure, provider};

    pub use crate::introspection::prepare_introspection_with_fixture;

    /// Fixture-only custody for a real verifier transport. It cannot construct
    /// a principal or bypass verification.
    #[derive(derive_more::Debug)]
    #[debug("FixtureTransport([REDACTED])")]
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
        pub fn try_new(
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
}
// template:end oidc-introspection:authn-introspection-test-support

/// A prepared real authentication engine.
#[derive(Clone, derive_more::Debug)]
#[debug("Verifier([REDACTED])")]
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
    pub(crate) fn introspection(engine: Arc<introspection::IntrospectionVerifier>) -> Self {
        Self {
            engine: Engine::Introspection(engine),
            counters: SuccessCounters::new("introspection"),
        }
    }
    // template:end oidc-introspection:authn-introspection-verifier

    /// Fixture-only: verifies under a fresh 3-second budget that is not linked
    /// to any caller's deadline or cancellation. Production code has a caller
    /// context and uses [`Verifier::authenticate_with_context`].
    ///
    /// # Errors
    /// Returns [`Failure`] for invalid token evidence or an unavailable provider.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn verify(&self, token: &BearerToken<'_>) -> Result<Principal, Failure> {
        self.verify_with_context(
            token,
            &OperationContext::with_timeout(provider::PROVIDER_TIMEOUT),
        )
        .await
    }

    /// Verifies a token while spending the caller's original operation budget.
    /// A stopped caller leaves shared key refresh and other cache-fill waiters running.
    ///
    /// # Errors
    /// Returns [`Failure::Unavailable`] when the caller stops, including when
    /// synchronous verification finishes after its cutoff. Other failures retain
    /// the existing invalid-evidence and provider classifications.
    pub(crate) async fn verify_with_context(
        &self,
        token: &BearerToken<'_>,
        context: &OperationContext,
    ) -> Result<Principal, Failure> {
        context.check().map_err(|_| Failure::Unavailable)?;
        let verification = async {
            match &self.engine {
                // template:begin oidc-jwt:authn-jwt-verify
                Engine::Jwt(engine) => engine.verify(token).await,
                // template:end oidc-jwt:authn-jwt-verify
                // template:begin oidc-introspection:authn-introspection-verify
                Engine::Introspection(auth) => auth.verify(token, context).await,
                // template:end oidc-introspection:authn-introspection-verify
            }
        };
        // Verification first: a ready engine then never arms the deadline timer.
        let result = tokio::select! {
            biased;
            result = verification => result,
            _ = context.wait_stopped() => return Err(Failure::Unavailable),
        };
        // A ready synchronous engine may consume the remaining budget in one poll.
        // A late failure is no more valid as credential evidence than late success.
        context.check().map_err(|_| Failure::Unavailable)?;
        record_verification(&self.counters, result)
    }
}
