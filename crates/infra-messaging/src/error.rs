use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MessagingError {
    #[error("messaging configuration is invalid: {0}")]
    Configuration(&'static str),
    #[error("messaging broker connection failed")]
    Connection,
    #[error("messaging broker authentication failed")]
    Authentication,
    #[error("messaging broker topology is unavailable")]
    Topology,
    // template:begin outbox:messaging-outbox-source-error
    #[error("outbox source stream is unsafe: {0}")]
    OutboxSource(&'static str),
    // template:end outbox:messaging-outbox-source-error
    #[error("messaging operation exceeded its {budget:?} budget")]
    TimedOut { budget: Duration },
    #[error("messaging operation was cancelled")]
    Cancelled,
    #[error("messaging resource is draining or its consumer failed")]
    Unavailable,
    #[error("messaging resource bounds are invalid")]
    Bounds,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PrepareError {
    #[error("event payload cannot be serialized")]
    Serialization,
    #[error("payload exceeds configured maximum")]
    PayloadTooLarge,
    #[error("event payload structure is invalid")]
    PayloadStructure,
    #[error("event identity is invalid")]
    Identity,
    #[error("creation time is required")]
    CreatedAtRequired,
    #[error("creation time is out of range")]
    CreatedAtOutOfRange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PublishEnvelopeError {
    #[error("payload exceeds configured maximum")]
    PayloadTooLarge,
    #[error("encoded headers exceed {} KiB", crate::wire::HEADER_LIMIT_BYTES / 1024)]
    HeadersTooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PublishError {
    #[error("messaging publication was rejected: {0}")]
    Rejected(#[from] PublishRejection),
    #[error("messaging publication acknowledgement is ambiguous")]
    Ambiguous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PublishRejection {
    #[error("invalid publication envelope: {0}")]
    InvalidEnvelope(#[from] PublishEnvelopeError),
    #[error("messaging resource is draining or its consumer failed")]
    Unavailable,
    #[error("publication context stopped before dispatch")]
    ContextStopped,
    #[error("publication acknowledgement window is full")]
    WindowFull,
    #[error("message exceeds the broker's current size limit")]
    BrokerSizeLimit,
    #[error("publication destination stream is sealed")]
    BrokerStreamSealed,
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
