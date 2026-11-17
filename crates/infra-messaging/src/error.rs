use std::time::Duration;

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
    #[error("messaging resource is draining or its consumer failed")]
    Unavailable,
    #[error("messaging resource bounds are invalid")]
    Bounds,
}

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
    #[error("event payload structure is invalid")]
    PayloadStructure,
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
    #[error("typed route is not registered for {event_type} v{schema_version}")]
    MissingRoute {
        event_type: &'static str,
        schema_version: u16,
    },
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PublishError {
    #[error("messaging publication was rejected: {0}")]
    Rejected(#[from] PublishRejection),
    #[error("messaging publication acknowledgement is ambiguous")]
    Ambiguous,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PublishRejection {
    #[error("invalid publication envelope: {0}")]
    InvalidEnvelope(#[source] EnvelopeError),
    #[error("messaging resource is draining or its consumer failed")]
    Unavailable,
    #[error("publication context stopped before dispatch")]
    ContextStopped,
    #[error("publication acknowledgement window is full")]
    WindowFull,
    #[error("message exceeds the broker's current size limit")]
    BrokerSizeLimit,
    #[error("broker refused publication")]
    BrokerRefused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HandlerError {
    #[error("event handler requested retry")]
    Retryable,
    #[error("event handler permanently rejected the event")]
    Permanent,
}
