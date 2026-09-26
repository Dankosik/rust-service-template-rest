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

use crate::error::{MessagingError, PublishError};
use crate::messaging::{BROKER_OPERATION_BUDGET, Shared};
use crate::prepared::{PreparedEvent, PublishAck};
use crate::wire::{
    encode_prepared, encoded_header_bytes, subject_matches, valid_subject, validate_encoded_message,
};

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
    /// Returns an error for invalid or oversized intent, a subject outside the
    /// admitted stream, or a draining dependency.
    pub fn prepare<T: EventPayload>(
        &self,
        subject: impl Into<String>,
        event: &Event<T>,
    ) -> Result<PreparedEvent, MessagingError> {
        if self.shared.draining.load(Ordering::Acquire) {
            return Err(MessagingError::Draining);
        }
        let prepared = PreparedEvent::prepare(subject, event, self.shared.max_payload_bytes)?;
        if !self
            .shared
            .source_subjects
            .iter()
            .any(|filter| subject_matches(filter, prepared.subject()))
        {
            return Err(MessagingError::Topology);
        }
        Ok(prepared)
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
        let result = if self.shared.draining.load(Ordering::Acquire)
            || self.shared.failed.load(Ordering::Acquire)
        {
            Err(PublishError::Rejected)
        } else {
            match encode_prepared(event) {
                Ok(headers) => {
                    publish_raw(
                        &self.shared,
                        &event.subject,
                        headers,
                        event.payload.clone(),
                        &self.shared.source_stream,
                        deadline,
                        cancel,
                    )
                    .await
                }
                Err(_) => Err(PublishError::Rejected),
            }
        };
        let outcome = match &result {
            Ok(_) => "acknowledged",
            Err(PublishError::Rejected) => "rejected",
            Err(PublishError::Ambiguous) => "ambiguous",
        };
        metrics::counter!("messaging_publish_total", "result" => outcome).increment(1);
        metrics::histogram!("messaging_publish_duration_seconds", "result" => outcome)
            .record(started.elapsed().as_secs_f64());
        result
    }
}

/// Shared exchange for a producer or a previously admitted DLQ settlement.
/// Settlements remain allowed after consumer admission begins draining.
pub(crate) async fn publish_raw(
    shared: &Shared,
    subject: &str,
    mut headers: HeaderMap,
    payload: Bytes,
    expected_stream: &str,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<PublishAck, PublishError> {
    let deadline = deadline.min(Instant::now() + BROKER_OPERATION_BUDGET);
    if cancel.is_cancelled() || Instant::now() >= deadline || !valid_subject(subject) {
        return Err(PublishError::Rejected);
    }
    let stream_max = if expected_stream == shared.source_stream {
        if shared.draining.load(Ordering::Acquire) || shared.failed.load(Ordering::Acquire) {
            return Err(PublishError::Rejected);
        }
        if !shared
            .source_subjects
            .iter()
            .any(|filter| subject_matches(filter, subject))
        {
            return Err(PublishError::Rejected);
        }
        shared.source_max_message_size
    } else if shared.dlq_stream.as_deref() == Some(expected_stream) {
        shared.dlq_max_message_size
    } else {
        return Err(PublishError::Rejected);
    };
    headers.insert(async_nats::header::NATS_EXPECTED_STREAM, expected_stream);
    validate_encoded_message(subject, &headers, payload.len(), shared.max_payload_bytes)
        .map_err(|_| PublishError::Rejected)?;
    let message_bytes = payload.len().saturating_add(encoded_header_bytes(&headers));
    if subject.len().saturating_add(message_bytes) > stream_max
        || message_bytes > shared.client.max_payload()
    {
        return Err(PublishError::Rejected);
    }
    // No await precedes this admission check. Once the exchange is polled,
    // cancellation or timeout may race a queued publish and is ambiguous.
    if cancel.is_cancelled() || Instant::now() >= deadline {
        return Err(PublishError::Rejected);
    }
    let mut dispatched = false;
    let result = {
        let exchange = async {
            dispatched = true;
            let ack = shared
                .jetstream
                .send_publish(
                    subject.to_owned(),
                    PublishMessage::build()
                        .headers(headers)
                        .payload(payload)
                        .expected_stream(expected_stream),
                )
                .await
                .map_err(|error| classify_publish(&error))?;
            let ack = ack.await.map_err(|error| classify_publish(&error))?;
            if ack.stream != expected_stream {
                return Err(PublishError::Ambiguous);
            }
            Ok(PublishAck {
                stream: ack.stream,
                sequence: ack.sequence,
                duplicate: ack.duplicate,
            })
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            () = tokio::time::sleep_until(deadline) => None,
            result = exchange => Some(result),
        }
    };
    result.unwrap_or(Err(if dispatched {
        PublishError::Ambiguous
    } else {
        PublishError::Rejected
    }))
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
