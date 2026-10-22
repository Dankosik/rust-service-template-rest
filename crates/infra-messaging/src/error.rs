use std::time::Duration;

/// A bounded adapter failure safe to report at composition boundaries.
#[derive(Debug, thiserror::Error)]
pub enum MessagingError {
    /// A setting is invalid; the text names which one.
    #[error("messaging configuration is invalid: {0}")]
    Configuration(&'static str),
    /// The broker could not be reached.
    #[error("messaging broker connection failed")]
    Connection,
    /// The broker rejected the credentials.
    #[error("messaging broker authentication failed")]
    Authentication,
    /// The configured stream or consumer is missing or unusable.
    #[error("messaging broker topology is unavailable")]
    Topology,
    /// The operation did not finish within its time budget.
    #[error("messaging operation exceeded its {budget:?} budget")]
    TimedOut {
        /// Time budget that was exceeded.
        budget: Duration,
    },
    /// The caller's cancellation token fired before the operation was sent.
    #[error("messaging operation was cancelled before dispatch")]
    Cancelled,
    /// The resource is shutting down or has failed and takes no new work.
    #[error("messaging resource is draining")]
    Draining,
    /// An event envelope is malformed or exceeds its bounds; the text names the rule.
    #[error("messaging envelope is invalid: {0}")]
    Envelope(&'static str),
    /// Payload or consumer-concurrency bounds are zero or cannot fit one publication.
    #[error("messaging resource bounds are invalid")]
    Bounds,
}

/// A conclusive or uncertain publication outcome.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// The broker refused the publication.
    #[error("messaging publication was rejected")]
    Rejected,
    /// The broker's answer was lost, so the event may or may not be stored.
    #[error("messaging publication acknowledgement is ambiguous")]
    Ambiguous,
}

/// A route or typed-handler registration failure.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The route is malformed; the text names the rule.
    #[error("event route is invalid: {0}")]
    InvalidRoute(&'static str),
    /// A route for this event type and schema version is already registered.
    #[error("duplicate route for {event_type} v{schema_version}")]
    DuplicateRoute {
        /// Event type of the duplicate route.
        event_type: String,
        /// Schema version of the duplicate route.
        schema_version: u16,
    },
    /// A handler needs a route that is not registered.
    #[error("route missing for {event_type} v{schema_version}")]
    MissingRoute {
        /// Event type of the missing route.
        event_type: String,
        /// Schema version of the missing route.
        schema_version: u16,
    },
    /// A handler for this event type and schema version is already registered.
    #[error("duplicate handler for {event_type} v{schema_version}")]
    DuplicateHandler {
        /// Event type of the duplicate handler.
        event_type: String,
        /// Schema version of the duplicate handler.
        schema_version: u16,
    },
    /// The registry has no typed handlers, so there is nothing to consume.
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
