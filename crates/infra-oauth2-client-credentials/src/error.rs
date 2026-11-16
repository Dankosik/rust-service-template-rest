use std::fmt;

use serde::Deserialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("OAuth2 {key}: {reason}")]
pub struct ConfigurationError {
    pub key: &'static str,
    pub reason: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
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

/// RFC 6749 §5.2, RFC 8693 §2.2.2
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
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
        f.write_str(match self {
            Self::Other => "no registered error code",
            registered => registered.label(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("caller Authorization header conflicts with OAuth2 authentication")]
    AuthorizationConflict,
    #[error("request carries no subject to act on behalf of")]
    SubjectRequired,
    #[error(transparent)]
    Acquisition(#[from] AcquisitionError),
    #[error(transparent)]
    Resource(#[from] infra_outbound_http::Error),
}
