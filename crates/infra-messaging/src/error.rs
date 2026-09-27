use std::time::Duration;

/// A bounded adapter failure safe to report at composition boundaries.
#[derive(Debug, thiserror::Error)]
pub enum MessagingError {
    #[error("messaging configuration is invalid: {0}")]
    Configuration(&'static str),
    #[error("messaging broker connection failed")]
    Connection,
    #[error("messaging broker authentication failed")]
    Authentication,
    #[error("messaging broker topology is unavailable")]
    Topology,
    #[error("messaging operation exceeded its {budget:?} budget")]
    TimedOut { budget: Duration },
    #[error("messaging operation was cancelled before dispatch")]
    Cancelled,
    #[error("messaging resource is draining")]
    Draining,
    #[error("messaging envelope is invalid: {0}")]
    Envelope(&'static str),
    #[error("messaging resource bounds are invalid")]
    Bounds,
    #[error("messaging resource closed before completion")]
    Closed,
}

/// A conclusive or uncertain publication outcome.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("messaging publication was rejected")]
    Rejected,
    #[error("messaging publication acknowledgement is ambiguous")]
    Ambiguous,
}

/// A route or typed-handler registration failure.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("event route is invalid: {0}")]
    InvalidRoute(&'static str),
    #[error("duplicate route for {event_type} v{schema_version}")]
    DuplicateRoute {
        event_type: String,
        schema_version: u16,
    },
    #[error("route missing for {event_type} v{schema_version}")]
    MissingRoute {
        event_type: String,
        schema_version: u16,
    },
    #[error("duplicate handler for {event_type} v{schema_version}")]
    DuplicateHandler {
        event_type: String,
        schema_version: u16,
    },
    #[error("no typed event handlers are registered")]
    Empty,
}

/// A handler's settled classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HandlerError {
    #[error("event handler requested retry")]
    Retryable,
    #[error("event handler permanently rejected the event")]
    Permanent,
}

impl HandlerError {
    #[must_use]
    pub const fn retryable() -> Self {
        Self::Retryable
    }

    #[must_use]
    pub const fn permanent() -> Self {
        Self::Permanent
    }
}
