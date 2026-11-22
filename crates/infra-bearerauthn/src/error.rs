// template:begin oidc-jwt:authn-error-provider-import
use crate::ProviderFailure;
// template:end oidc-jwt:authn-error-provider-import

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum, thiserror::Error)]
#[error("{}", self.as_str())]
pub enum Failure {
    Missing,
    Malformed,
    Invalid,
    Unavailable,
}

impl Failure {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "bearer authentication is required",
            Self::Malformed => "bearer authentication is malformed",
            Self::Invalid => "bearer authentication is invalid",
            Self::Unavailable => "bearer authentication is unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("authentication URL must be an HTTPS URL without userinfo, fragment, or whitespace")]
pub struct InvalidUrl;

// template:begin oidc-introspection:authn-introspection-cache-options-error
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("introspection cache capacity or TTL is outside the allowed bounds")]
pub struct InvalidCacheOptions;
// template:end oidc-introspection:authn-introspection-cache-options-error

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid authentication option")]
pub struct OptionsError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("authentication preparation failed during Client: ClientBuild")]
pub struct ClientBuildError;

// template:begin oidc-jwt:authn-jwt-preparation-errors
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DiscoveryError {
    #[error("Fetch({0:?})")]
    Fetch(ProviderFailure),
    #[error("Parse")]
    Parse,
    #[error("InvalidUrl")]
    InvalidJwksUrl,
    #[error("IssuerMismatch (configured issuer {configured:?}, discovered issuer {discovered:?})")]
    IssuerMismatch {
        configured: String,
        discovered: String,
    },
}

impl DiscoveryError {
    pub(crate) const fn reason_label(&self) -> &'static str {
        match self {
            Self::Fetch(_) => "fetch",
            Self::Parse => "parse",
            Self::InvalidJwksUrl => "invalid_jwks_url",
            Self::IssuerMismatch { .. } => "issuer_mismatch",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum JwksError {
    #[error("Fetch({0:?})")]
    Fetch(ProviderFailure),
    #[error("Parse")]
    Parse,
    #[error("NoUsableKeys")]
    NoUsableKeys,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparationPhase {
    Client,
    Discovery,
    Jwks,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PreparationError {
    #[error("{}", ClientBuildError)]
    ClientBuild,
    #[error("authentication preparation failed during Discovery: {0}")]
    Discovery(#[from] DiscoveryError),
    #[error("authentication preparation failed during Jwks: {0}")]
    Jwks(#[from] JwksError),
    #[error("authentication preparation failed during {0:?}: Timeout")]
    Timeout(PreparationPhase),
}

impl From<ClientBuildError> for PreparationError {
    fn from(_: ClientBuildError) -> Self {
        Self::ClientBuild
    }
}

impl PreparationError {
    #[must_use]
    pub const fn phase(&self) -> PreparationPhase {
        match self {
            Self::ClientBuild => PreparationPhase::Client,
            Self::Discovery(_) => PreparationPhase::Discovery,
            Self::Jwks(_) => PreparationPhase::Jwks,
            Self::Timeout(phase) => *phase,
        }
    }
}
// template:end oidc-jwt:authn-jwt-preparation-errors

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
    StaleJwks,
    RefreshWorkerLost,
    // template:end oidc-jwt:authn-jwt-reasons
    // template:begin oidc-introspection:authn-introspection-reasons
    Provider(crate::provider::ProviderReason),
    Inactive,
    Capacity,
    // template:end oidc-introspection:authn-introspection-reasons
}

impl VerificationReason {
    pub(crate) const fn label(self) -> &'static str {
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
            Self::StaleJwks => "stale_jwks",
            Self::RefreshWorkerLost => "refresh_worker_lost",
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
