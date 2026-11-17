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
mod principal;
mod provider;
// template:begin oidc-jwt:authn-refresh-module
mod refresh;
// template:end oidc-jwt:authn-refresh-module
#[cfg(test)]
mod test_helpers;
#[cfg(test)]
mod test_metrics;

use std::sync::{Arc, OnceLock};

use enum_map::EnumMap;
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

pub use authenticate::Transport;
pub(crate) use bearer::{BearerToken, parse_bearer};
// template:begin oidc-introspection:authn-introspection-prepare-export
pub use introspection::{IntrospectionCacheOptions, IntrospectionOptions, prepare_introspection};
// template:end oidc-introspection:authn-introspection-prepare-export
// template:begin oidc-jwt:authn-jwt-prepare-export
pub use jwks::JwtAlgorithm;
pub use jwt::{JwtOptions, RefreshTask, TokenProfile, prepare_jwt};
// template:end oidc-jwt:authn-jwt-prepare-export
pub use principal::{Actor, ClaimAccessError, Principal};
pub use provider::{EndpointUrl, HISTOGRAMS, IssuerUrl, ProviderFailure};

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum, thiserror::Error)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparationPhase {
    Options,
    Client,
    // template:begin oidc-jwt:authn-jwt-preparation-phases
    Discovery,
    Jwks,
    // template:end oidc-jwt:authn-jwt-preparation-phases
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparationReason {
    InvalidUrl,
    ClientBuild,
    InvalidOption,
    // template:begin oidc-jwt:authn-jwt-preparation-reasons
    Fetch(ProviderFailure),
    Timeout,
    Parse,
    IssuerMismatch,
    NoUsableKeys,
    // template:end oidc-jwt:authn-jwt-preparation-reasons
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
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
    // template:end oidc-jwt:authn-jwt-reasons
    // template:begin oidc-introspection:authn-introspection-reasons
    Provider(provider::ProviderReason),
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
            Self::Clock => "clock",
            // template:begin oidc-jwt:authn-jwt-reason-labels
            Self::Header => "header",
            Self::Algorithm => "algorithm",
            Self::Profile => "profile",
            Self::Signature => "signature",
            Self::UnknownKey => "unknown_key",
            // template:end oidc-jwt:authn-jwt-reason-labels
            // template:begin oidc-introspection:authn-introspection-reason-labels
            Self::Provider(failure) => failure.label(),
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

pub(crate) fn unix_now() -> Result<std::time::Duration, VerificationError> {
    unix_time(std::time::SystemTime::now())
}

fn unix_time(now: std::time::SystemTime) -> Result<std::time::Duration, VerificationError> {
    now.duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| VerificationError::unavailable(VerificationReason::Clock))
}

const TOKEN_VERIFICATIONS_METRIC: &str = "authn_token_verifications_total";

fn record_verification(
    counters: &VerificationCounters,
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
    counters.refused[error.reason]
        .get_or_init(|| {
            metrics::counter!(
                TOKEN_VERIFICATIONS_METRIC,
                "mode" => mode,
                "outcome" => "failure",
                "reason" => reason
            )
        })
        .increment(1);
    tracing::debug!(mode, reason, "authn_verification_failed");
    Err(error.failure)
}

struct VerificationCounters {
    mode: &'static str,
    verified: metrics::Counter,
    refused: EnumMap<VerificationReason, OnceLock<metrics::Counter>>,
    transport: authenticate::TransportCounters,
}

impl VerificationCounters {
    fn new(mode: &'static str) -> Arc<Self> {
        metrics::describe_counter!(
            TOKEN_VERIFICATIONS_METRIC,
            "Token verification decisions by engine and closed reason"
        );
        Arc::new(Self {
            mode,
            verified: metrics::counter!(TOKEN_VERIFICATIONS_METRIC, "mode" => mode, "outcome" => "success", "reason" => "verified"),
            refused: EnumMap::default(),
            transport: authenticate::TransportCounters::new(),
        })
    }
}

// template:begin oidc-introspection:authn-introspection-test-support
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use tokio_util::sync::CancellationToken;

    use super::{Failure, provider};

    pub use crate::introspection::prepare_introspection_with_fixture;

    #[derive(derive_more::Debug)]
    #[debug("FixtureTransport([REDACTED])")]
    pub struct FixtureTransport {
        provider: provider::ProviderClient,
    }

    impl FixtureTransport {
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

#[derive(Clone, derive_more::Debug)]
#[debug("Verifier([REDACTED])")]
pub struct Verifier {
    engine: Engine,
    counters: Arc<VerificationCounters>,
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
    fn new(engine: Engine) -> Self {
        let mode = match &engine {
            // template:begin oidc-jwt:authn-jwt-engine-mode
            Engine::Jwt(_) => "jwt",
            // template:end oidc-jwt:authn-jwt-engine-mode
            // template:begin oidc-introspection:authn-introspection-engine-mode
            Engine::Introspection(_) => "introspection",
            // template:end oidc-introspection:authn-introspection-engine-mode
        };
        Self {
            engine,
            counters: VerificationCounters::new(mode),
        }
    }

    // template:begin oidc-jwt:authn-jwt-verifier
    pub(crate) fn jwt(engine: jwt::JwtVerifier) -> Self {
        Self::new(Engine::Jwt(Arc::new(engine)))
    }
    // template:end oidc-jwt:authn-jwt-verifier

    // template:begin oidc-introspection:authn-introspection-verifier
    pub(crate) fn introspection(engine: Arc<introspection::IntrospectionVerifier>) -> Self {
        Self::new(Engine::Introspection(engine))
    }
    // template:end oidc-introspection:authn-introspection-verifier

    #[cfg(feature = "test-support")]
    pub async fn verify_fixture_authorization(
        &self,
        authorization: &[u8],
    ) -> Result<Principal, Failure> {
        let token = parse_bearer([authorization])?;
        self.verify(&token).await
    }

    #[cfg(any(test, feature = "test-support"))]
    async fn verify(&self, token: &BearerToken<'_>) -> Result<Principal, Failure> {
        self.verify_with_context(
            token,
            &OperationContext::with_timeout(provider::PROVIDER_TIMEOUT),
        )
        .await
    }

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
        let result = tokio::select! {
            biased;
            result = verification => result,
            _ = context.wait_stopped() => return Err(Failure::Unavailable),
        };
        context.check().map_err(|_| Failure::Unavailable)?;
        record_verification(&self.counters, result)
    }
}
