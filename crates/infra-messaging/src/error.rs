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
    Envelope(EnvelopeError),
    /// The configured payload limit is zero or cannot fit one publication, or
    /// the server, a stream or the consumer's pull budget cannot carry what
    /// the configured limits admit.
    #[error("messaging resource bounds are invalid")]
    Bounds,
}

impl From<EnvelopeError> for MessagingError {
    fn from(error: EnvelopeError) -> Self {
        Self::Envelope(error)
    }
}

/// Why an event cannot become, or be read from, a wire envelope.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("subject is invalid")]
    Subject,
    #[error("dead-letter original subject is invalid")]
    DeadLetterSubject,
    #[error("event identity is invalid")]
    Identity,
    #[error("event payload cannot be serialized")]
    Serialization,
    #[error("payload exceeds configured maximum")]
    PayloadTooLarge,
    #[error("encoded headers exceed 8 KiB")]
    HeadersTooLarge,
    #[error("required header {0} is missing")]
    MissingHeader(&'static str),
    #[error("event schema is invalid")]
    Schema,
    #[error("creation time is invalid")]
    CreatedAtInvalid,
    #[error("creation time is required")]
    CreatedAtRequired,
    #[error("creation time is out of range")]
    CreatedAtOutOfRange,
    #[error("dead-letter creation time is required")]
    DeadLetterCreatedAtRequired,
    #[error("event route is not registered for {event_type} v{schema_version}")]
    MissingRoute {
        event_type: &'static str,
        schema_version: u16,
    },
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
