use std::error::Error as _;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use async_nats::jetstream::ErrorCode;
use async_nats::jetstream::context::PublishErrorKind;
use async_nats::jetstream::message::PublishMessage;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use crate::error::PublishError;
use crate::messaging::{BROKER_OPERATION_BUDGET, Shared};
use crate::prepared::{PreparedEvent, PublishAck};
use crate::wire::{HEADER_LIMIT_BYTES, encode_prepared, encoded_header_bytes};

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
            match source_message(
                event,
                &span,
                &self.shared.source_stream,
                self.shared.max_payload_bytes,
            ) {
                Ok(message) => {
                    publish(&self.shared, &event.subject, message, deadline, cancel)
                        .instrument(span.clone())
                        .await
                }
                Err(error) => Err(error),
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

/// Finalizes the source/outbox envelope before checking this resource's wire bounds.
fn source_message(
    event: &PreparedEvent,
    span: &tracing::Span,
    expected_stream: &str,
    max_payload_bytes: usize,
) -> Result<PublishMessage, PublishError> {
    if event.payload.len() > max_payload_bytes {
        return Err(PublishError::Rejected);
    }
    let mut headers = encode_prepared(event).map_err(|_| PublishError::Rejected)?;
    // The consumer's delivery span continues this trace.
    crate::trace::inject(span, &mut headers);
    headers.insert(async_nats::header::NATS_EXPECTED_STREAM, expected_stream);
    if encoded_header_bytes(&headers) > HEADER_LIMIT_BYTES {
        return Err(PublishError::Rejected);
    }
    Ok(PublishMessage::build()
        .headers(headers)
        .payload(event.payload.clone()))
}

/// Sends the finalized message unchanged and awaits its `JetStream` acknowledgement.
/// The native client enforces broker size and shared publication admission.
pub(crate) async fn publish(
    shared: &Shared,
    subject: &str,
    message: PublishMessage,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<PublishAck, PublishError> {
    let deadline = deadline.min(Instant::now() + BROKER_OPERATION_BUDGET);
    if cancel.is_cancelled() || Instant::now() >= deadline {
        return Err(PublishError::Rejected);
    }
    let exchange = async {
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "fixed valid event fixture")]
mod tests {
    use domain_events::{Event, EventPayload};

    use super::*;

    #[derive(serde::Serialize)]
    struct Created;

    impl EventPayload for Created {
        const EVENT_TYPE: &'static str = "order.created";
        const SCHEMA_VERSION: u16 = 1;
    }

    #[test]
    fn final_expected_stream_header_counts_toward_the_receiving_limit() {
        let event = Event {
            id: "event-1".to_owned(),
            occurred_at: time::UtcDateTime::from_unix_timestamp(1_700_000_000)
                .expect("fixed timestamp is valid"),
            payload: Created,
        };
        let prepared = PreparedEvent::prepare("orders.created", &event, 4)
            .expect("null fits four payload bytes");
        // Native NATS header framing is the independent wire-size oracle.
        let fixed = concat!(
            "NATS/1.0\r\n",
            "Message-Id: event-1\r\n",
            "Event-Type: order.created\r\n",
            "Event-Schema: v1\r\n",
            "Created-At: 2023-11-14T22:13:20Z\r\n",
            "Nats-Msg-Id: event-1\r\n",
            "Nats-Expected-Stream: \r\n",
            "\r\n",
        );
        let stream = "s".repeat(8192 - fixed.len());
        assert!(source_message(&prepared, &tracing::Span::none(), &stream, 4).is_ok());
        assert!(matches!(
            source_message(&prepared, &tracing::Span::none(), &(stream + "s"), 4),
            Err(PublishError::Rejected)
        ));
    }
}
