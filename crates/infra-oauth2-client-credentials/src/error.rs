use std::{borrow::Cow, fmt};

/// Safe admission failure identifying only the option and its static cause.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("OAuth2 {key}: {reason}")]
pub struct ConfigurationError {
    pub key: &'static str,
    pub reason: &'static str,
}

/// Closed acquisition outcomes. Raw protocol and transport errors are discarded.
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
    /// The provider answered 5xx or 429, or did not answer within the
    /// adapter's own attempt bound, or the credential owner has stopped.
    #[error("OAuth2 credentials are unavailable")]
    Unavailable,
    #[error("OAuth2 provider acquisition capacity is exhausted")]
    AtCapacity,
    /// The provider refused the request, with its registered error code.
    #[error("OAuth2 provider rejected the request: {0}")]
    Rejected(Rejection),
    #[error("OAuth2 token response is invalid")]
    InvalidResponse,
    #[error("OAuth2 client assertion could not be signed")]
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

/// Why the provider refused a token request: its `error` code (RFC 6749
/// section 5.2, RFC 8693 section 2.2.2) when it is one of the registered
/// ones. The code is the only part of the provider's answer that is kept.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rejection {
    InvalidRequest,
    InvalidClient,
    InvalidGrant,
    UnauthorizedClient,
    UnsupportedGrantType,
    InvalidScope,
    InvalidTarget,
    /// No `error` code, or one outside the registered set.
    Other,
}

impl Rejection {
    /// The registered variants, in the order their codes are documented.
    const REGISTERED: [Self; 7] = [
        Self::InvalidRequest,
        Self::InvalidClient,
        Self::InvalidGrant,
        Self::UnauthorizedClient,
        Self::UnsupportedGrantType,
        Self::InvalidScope,
        Self::InvalidTarget,
    ];

    pub(crate) fn from_body(body: &[u8]) -> Self {
        #[derive(serde::Deserialize)]
        struct ErrorResponse<'a> {
            #[serde(borrow)]
            error: Cow<'a, str>,
        }
        let Ok(response) = serde_json::from_slice::<ErrorResponse<'_>>(body) else {
            return Self::Other;
        };
        Self::REGISTERED
            .into_iter()
            .find(|rejection| rejection.code() == Some(&*response.error))
            .unwrap_or(Self::Other)
    }

    /// The registered wire code, or `None` for [`Self::Other`].
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

/// Authentication failures are distinct from the existing resource transport.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("caller Authorization header conflicts with OAuth2 authentication")]
    AuthorizationConflict,
    /// A token exchange was needed but the request carries no subject token.
    #[error("request carries no subject to act on behalf of")]
    SubjectRequired,
    #[error(transparent)]
    Acquisition(#[from] AcquisitionError),
    #[error(transparent)]
    Resource(#[from] infra_outbound_http::Error),
}
