use std::{error::Error as StdError, fmt};

use http::StatusCode;
use operation_context::Stopped;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("OAuth2 {key}: {reason}")]
pub struct ConfigurationError {
    pub key: &'static str,
    pub reason: &'static str,
}

impl ConfigurationError {
    pub(crate) const fn resource_origin() -> Self {
        Self {
            key: "resource_origin",
            reason: "must match the transport origin registered for these credentials",
        }
    }
}

#[derive(derive_more::Debug, thiserror::Error)]
pub enum PreparationError {
    #[error(transparent)]
    Configuration(#[from] ConfigurationError),
    #[error(transparent)]
    Transport(#[from] TransportBuildFailure),
}

impl PreparationError {
    #[must_use]
    pub const fn diagnostic_cause(&self) -> Option<&infra_outbound_http::BuildError> {
        match self {
            Self::Configuration(_) => None,
            Self::Transport(error) => Some(&error.cause),
        }
    }
}

#[derive(derive_more::Debug, thiserror::Error)]
#[error("OAuth2 transport construction failed for {endpoint} ({cause})")]
pub struct TransportBuildFailure {
    endpoint: &'static str,
    #[debug(skip)]
    cause: infra_outbound_http::BuildError,
}

impl TransportBuildFailure {
    pub(crate) const fn token_url(cause: infra_outbound_http::BuildError) -> Self {
        Self {
            endpoint: "token_url",
            cause,
        }
    }

    pub(crate) const fn issuer(cause: infra_outbound_http::BuildError) -> Self {
        Self {
            endpoint: "issuer",
            cause,
        }
    }
}

#[derive(derive_more::Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error(transparent)]
    Preparation(#[from] PreparationError),
    #[error("OAuth2 discovery: {0}")]
    Metadata(&'static str),
    #[error("OAuth2 discovery: issuer discovery returned HTTP {0}")]
    Status(StatusCode),
    #[error(transparent)]
    Transport(DiscoveryTransportError),
    #[error("OAuth2 discovery: invalid issuer metadata")]
    Decode(#[debug(skip)] serde_json::Error),
}

impl From<ConfigurationError> for DiscoveryError {
    fn from(error: ConfigurationError) -> Self {
        Self::Preparation(PreparationError::from(error))
    }
}

impl From<TransportBuildFailure> for DiscoveryError {
    fn from(error: TransportBuildFailure) -> Self {
        Self::Preparation(PreparationError::from(error))
    }
}

impl DiscoveryError {
    #[must_use]
    pub fn diagnostic_cause(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Preparation(error) => Some(error.diagnostic_cause()?),
            Self::Transport(error) => Some(&error.cause),
            Self::Decode(error) => Some(error),
            Self::Metadata(_) | Self::Status(_) => None,
        }
    }
}

#[derive(derive_more::Debug, thiserror::Error)]
#[error("OAuth2 discovery: issuer discovery request failed: {}", .cause.diagnostic_reason())]
pub struct DiscoveryTransportError {
    #[source]
    kind: TransportFailure,
    #[debug(skip)]
    cause: infra_outbound_http::Error,
}

impl DiscoveryTransportError {
    pub(crate) fn new(cause: infra_outbound_http::Error) -> Self {
        Self {
            kind: TransportFailure::from(&cause),
            cause,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> TransportFailure {
        self.kind
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TransportFailure {
    #[error("operation timed out")]
    Timeout,
    #[error("operation was cancelled")]
    Cancelled,
    #[error("response body exceeded its limit")]
    ResponseLimit,
    #[error("transport failed")]
    Transport,
}

impl From<infra_outbound_http::Error> for TransportFailure {
    fn from(error: infra_outbound_http::Error) -> Self {
        Self::from(&error)
    }
}

impl From<&infra_outbound_http::Error> for TransportFailure {
    fn from(error: &infra_outbound_http::Error) -> Self {
        match error {
            infra_outbound_http::Error::Timeout => Self::Timeout,
            infra_outbound_http::Error::Cancelled => Self::Cancelled,
            infra_outbound_http::Error::ResponseBodyTooLarge => Self::ResponseLimit,
            infra_outbound_http::Error::InvalidTarget
            | infra_outbound_http::Error::DispatchRefused
            | infra_outbound_http::Error::Transport { .. } => Self::Transport,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum, thiserror::Error)]
pub enum AcquisitionError {
    #[error("OAuth2 acquisition deadline elapsed")]
    Timeout,
    #[error("OAuth2 acquisition was cancelled")]
    Cancelled,
    #[error("OAuth2 token transport failed")]
    Transport,
    #[error("OAuth2 token response exceeded its limit")]
    ResponseLimit,
    #[error("OAuth2 credentials are unavailable")]
    Unavailable,
    #[error("OAuth2 provider acquisition capacity is exhausted")]
    AtCapacity,
    #[error("OAuth2 provider rejected the request: {0}")]
    Rejected(Rejection),
    #[error("OAuth2 token response is invalid")]
    InvalidResponse,
    #[error("OAuth2 client assertion could not be created")]
    Assertion,
}

impl AcquisitionError {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Transport => "transport",
            Self::ResponseLimit => "limit",
            Self::Unavailable => "unavailable",
            Self::AtCapacity => "capacity",
            Self::Rejected(rejection) => rejection.label(),
            Self::InvalidResponse => "invalid",
            Self::Assertion => "assertion",
        }
    }
}

impl From<Stopped> for AcquisitionError {
    fn from(stopped: Stopped) -> Self {
        match stopped {
            Stopped::Deadline => Self::Timeout,
            Stopped::Cancelled => Self::Cancelled,
        }
    }
}

impl From<TransportFailure> for AcquisitionError {
    fn from(failure: TransportFailure) -> Self {
        match failure {
            TransportFailure::Timeout => Self::Timeout,
            TransportFailure::Cancelled => Self::Cancelled,
            TransportFailure::ResponseLimit => Self::ResponseLimit,
            TransportFailure::Transport => Self::Transport,
        }
    }
}

/// RFC 6749 §5.2, RFC 8693 §2.2.2
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, enum_map::Enum)]
#[serde(field_identifier, rename_all = "snake_case")]
pub enum Rejection {
    InvalidRequest,
    InvalidClient,
    InvalidGrant,
    UnauthorizedClient,
    UnsupportedGrantType,
    InvalidScope,
    InvalidTarget,
    #[serde(other)]
    Other,
}

impl Rejection {
    pub(crate) fn from_body(body: &[u8]) -> Self {
        #[derive(Deserialize)]
        struct ErrorResponse {
            error: Rejection,
        }
        serde_json::from_slice::<ErrorResponse>(body).map_or(Self::Other, |response| response.error)
    }

    fn code(self) -> Option<&'static str> {
        match self {
            Self::InvalidRequest => Some("invalid_request"),
            Self::InvalidClient => Some("invalid_client"),
            Self::InvalidGrant => Some("invalid_grant"),
            Self::UnauthorizedClient => Some("unauthorized_client"),
            Self::UnsupportedGrantType => Some("unsupported_grant_type"),
            Self::InvalidScope => Some("invalid_scope"),
            Self::InvalidTarget => Some("invalid_target"),
            Self::Other => None,
        }
    }

    fn label(self) -> &'static str {
        self.code().unwrap_or("rejected")
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code().unwrap_or("no registered error code"))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("caller Authorization header conflicts with OAuth2 authentication")]
    AuthorizationConflict,
    #[error(transparent)]
    Acquisition(#[from] AcquisitionError),
    #[error(transparent)]
    Resource(#[from] infra_outbound_http::Error),
}
