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
    #[error("messaging operation was cancelled")]
    Cancelled,
    /// The resource is shutting down or its consumer has failed, so it takes
    /// no new work.
    #[error("messaging resource is draining or its consumer failed")]
    Unavailable,
    /// The configured payload limit is zero or cannot fit one publication, or
    /// the server, a stream or the consumer's pull budget cannot carry what
    /// the configured limits admit.
    #[error("messaging resource bounds are invalid")]
    Bounds,
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
    #[error("encoded headers exceed {} KiB", crate::wire::HEADER_LIMIT_BYTES / 1024)]
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
    #[error("dead-letter record has no verifiable original message ID")]
    DeadLetterMessageIdMissing,
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
