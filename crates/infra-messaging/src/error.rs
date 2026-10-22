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
    /// The resource is shutting down or has failed and takes no new work.
    #[error("messaging resource is draining")]
    Draining,
    #[error("messaging envelope is invalid: {0}")]
    Envelope(&'static str),
    /// Payload or consumer-concurrency bounds are zero or cannot fit one publication.
    #[error("messaging resource bounds are invalid")]
    Bounds,
}

/// A conclusive or uncertain publication outcome.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("messaging publication was rejected")]
    Rejected,
    /// The broker's answer was lost, so the event may or may not be stored.
    #[error("messaging publication acknowledgement is ambiguous")]
    Ambiguous,
}

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
///
/// It carries no cause: the adapter never logs feature errors. Log the cause
/// in the handler, which runs inside the delivery span, so the record shares
/// its trace with the adapter's `messaging_delivery_failed` event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HandlerError {
    /// Delivery failed transiently and should be redelivered.
    #[error("event handler requested retry")]
    Retryable,
    /// Delivery can never succeed and should go to the dead-letter subject.
    #[error("event handler permanently rejected the event")]
    Permanent,
}
