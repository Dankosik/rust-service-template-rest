use std::error::Error as _;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use async_nats::HeaderMap;
use async_nats::jetstream::ErrorCode;
use async_nats::jetstream::context::PublishErrorKind;
use async_nats::jetstream::message::PublishMessage;
use bytes::Bytes;
use domain_events::{Event, EventPayload};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use crate::error::{MessagingError, PublishError};
use crate::messaging::{BROKER_OPERATION_BUDGET, Shared};
use crate::prepared::{PreparedEvent, PublishAck};
use crate::wire::encode_prepared;

/// How one publication ended. The discriminant indexes [`PUBLISH_RESULTS`].
#[derive(Clone, Copy)]
enum PublishResult {
    Acknowledged = 0,
    Rejected = 1,
    Ambiguous = 2,
}

/// Metric label of each [`PublishResult`], indexed by its discriminant.
pub(crate) const PUBLISH_RESULTS: [&str; 3] = ["acknowledged", "rejected", "ambiguous"];

/// A clonable producer admitted by one live messaging resource.
#[derive(Clone, Debug)]
pub struct Producer {
    pub(crate) shared: Arc<crate::messaging::Shared>,
}

impl Producer {
    /// Serializes an explicitly routed typed event once into immutable intent.
    ///
    /// Prefer [`Registry::prepare`](crate::Registry::prepare) for normal
    /// composition-owned routing; this form is retained for stored redrive.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid or oversized intent or a draining dependency.
    pub fn prepare<T: EventPayload + serde::Serialize>(
        &self,
        subject: impl Into<String>,
        event: &Event<T>,
    ) -> Result<PreparedEvent, MessagingError> {
        if self.shared.draining.load(Ordering::Acquire) {
            return Err(MessagingError::Draining);
        }
        PreparedEvent::prepare(subject, event, self.shared.max_payload_bytes)
    }

    /// Dispatches and awaits one confirmed `JetStream` acknowledgment under one deadline.
    ///
    /// # Errors
    ///
    /// Returns `Rejected` for conclusive refusal and `Ambiguous` when dispatch
    /// may have happened without a conclusive acknowledgment.
    pub async fn publish(
        &self,
        event: &PreparedEvent,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<PublishAck, PublishError> {
        let started = Instant::now();
        let span = tracing::info_span!(
            "messaging_publish",
            otel.name = format!("publish {}", event.subject),
            otel.kind = "producer",
            messaging.system = "nats",
            messaging.operation.type = "send",
            messaging.operation.name = "publish",
            messaging.destination.name = event.subject.as_str(),
            outcome = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        let result = if self.shared.draining.load(Ordering::Acquire)
            || self.shared.failed.load(Ordering::Acquire)
        {
            Err(PublishError::Rejected)
        } else {
            match encode_prepared(event) {
                Ok(mut headers) => {
                    // The consumer's delivery span continues this trace.
                    crate::trace::inject(&span, &mut headers);
                    publish(
                        &self.shared,
                        &event.subject,
                        headers,
                        event.payload.clone(),
                        &self.shared.source_stream,
                        deadline,
                        cancel,
                    )
                    .instrument(span.clone())
                    .await
                }
                Err(_) => Err(PublishError::Rejected),
            }
        };
        let outcome = match &result {
            Ok(_) => PublishResult::Acknowledged,
            Err(PublishError::Rejected) => PublishResult::Rejected,
            Err(PublishError::Ambiguous) => PublishResult::Ambiguous,
        } as usize;
        span.record("outcome", PUBLISH_RESULTS[outcome]);
        if result.is_err() {
            span.record("error.type", PUBLISH_RESULTS[outcome]);
            span.record("otel.status_code", "ERROR");
            span.in_scope(|| {
                tracing::warn!(
                    subject = event.subject.as_str(),
                    outcome = PUBLISH_RESULTS[outcome],
                    "messaging_publish_failed"
                );
            });
        }
        self.shared
            .publish_metrics
            .record(outcome, started.elapsed());
        result
    }
}

/// Publishes one message and awaits the `JetStream` acknowledgement of
/// `expected_stream`. The broker refuses a subject, size or stream mismatch.
pub(crate) async fn publish(
    shared: &Shared,
    subject: &str,
    headers: HeaderMap,
    payload: Bytes,
    expected_stream: &str,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<PublishAck, PublishError> {
    let deadline = deadline.min(Instant::now() + BROKER_OPERATION_BUDGET);
    if cancel.is_cancelled() || Instant::now() >= deadline {
        return Err(PublishError::Rejected);
    }
    let exchange = async {
        let message = PublishMessage::build()
            .headers(headers)
            .payload(payload)
            .expected_stream(expected_stream);
        let ack = shared
            .jetstream
            .send_publish(subject.to_owned(), message)
            .await
            .map_err(|error| classify_publish(&error))?
            .await
            .map_err(|error| classify_publish(&error))?;
        Ok(PublishAck {
            stream: ack.stream,
            sequence: ack.sequence,
            duplicate: ack.duplicate,
        })
    };
    // Nothing awaits between the check above and the first poll of the
    // exchange, so a cancellation or timeout here may follow dispatch.
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(PublishError::Ambiguous),
        () = tokio::time::sleep_until(deadline) => Err(PublishError::Ambiguous),
        result = exchange => result,
    }
}

fn classify_publish(error: &async_nats::jetstream::context::PublishError) -> PublishError {
    match error.kind() {
        PublishErrorKind::StreamNotFound
        | PublishErrorKind::WrongLastMessageId
        | PublishErrorKind::WrongLastSequence
        | PublishErrorKind::MaxPayloadExceeded
        | PublishErrorKind::MaxAckPending => PublishError::Rejected,
        PublishErrorKind::Other => {
            let refused = error
                .source()
                .and_then(|source| source.downcast_ref::<async_nats::jetstream::Error>())
                .is_some_and(|error| {
                    matches!(
                        error.error_code(),
                        ErrorCode::BAD_REQUEST
                            | ErrorCode::ACCOUNT_RESOURCES_EXCEEDED
                            | ErrorCode::INSUFFICIENT_RESOURCES
                            | ErrorCode::STORAGE_RESOURCES_EXCEEDED
                            | ErrorCode::JETSTREAM_NOT_ENABLED
                            | ErrorCode::JETSTREAM_NOT_ENABLED_FOR_ACCOUNT
                            | ErrorCode::STREAM_MESSAGE_EXCEEDS_MAXIMUM
                            | ErrorCode::STREAM_HEADER_EXCEEDS_MAXIMUM
                            | ErrorCode::STREAM_NOT_MATCH
                            | ErrorCode::STREAM_MISMATCH
                            | ErrorCode::STREAM_LIMITS
                            | ErrorCode::STREAM_STORE_FAILED
                            | ErrorCode::STREAM_SEALED
                            | ErrorCode::STREAM_WRONG_LAST_MESSAGE_ID
                            | ErrorCode::STREAM_WRONG_LAST_SEQUENCE
                    )
                });
            if refused {
                PublishError::Rejected
            } else {
                PublishError::Ambiguous
            }
        }
        PublishErrorKind::TimedOut | PublishErrorKind::BrokenPipe => PublishError::Ambiguous,
    }
}
