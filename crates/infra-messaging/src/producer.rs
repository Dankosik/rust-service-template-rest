use std::error::Error as _;
use std::sync::Arc;

use async_nats::jetstream::ErrorCode;
use async_nats::jetstream::context::PublishErrorKind;
use async_nats::jetstream::message::PublishMessage;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use crate::error::PublishError;
use crate::messaging::{Shared, within_operation_budget};
use crate::observe::{OutcomeLabel as _, outcome_label};
use crate::prepared::PreparedEvent;
use crate::wire::{encode_prepared, validate_header_bytes};

outcome_label! {
    /// How one publication ended.
    pub(crate) enum PublishOutcome {
        Acknowledged => "acknowledged",
        Rejected => "rejected",
        Ambiguous => "ambiguous",
    }
}

/// A clonable producer admitted by one live messaging resource.
#[derive(Clone, Debug)]
pub struct Producer {
    pub(crate) shared: Arc<Shared>,
}

/// Confirmed broker publication acknowledgment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishAck {
    pub stream: String,
    pub sequence: u64,
    /// True when the broker recognized the publication as a duplicate and stored nothing new.
    pub duplicate: bool,
}

impl Producer {
    /// Dispatches and awaits one confirmed `JetStream` acknowledgment under one
    /// deadline, at most one [`BROKER_OPERATION_BUDGET`](crate::messaging::BROKER_OPERATION_BUDGET)
    /// away.
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
        let result = self.dispatch(event, &span, deadline, cancel).await;
        let outcome = match &result {
            Ok(_) => PublishOutcome::Acknowledged,
            Err(PublishError::Rejected) => PublishOutcome::Rejected,
            Err(PublishError::Ambiguous) => PublishOutcome::Ambiguous,
        };
        let label = outcome.label();
        span.record("outcome", label);
        if result.is_err() {
            span.record("error.type", label);
            span.record("otel.status_code", "ERROR");
            span.in_scope(|| {
                tracing::warn!(
                    subject = event.subject.as_str(),
                    outcome = label,
                    "messaging_publish_failed"
                );
            });
        }
        self.shared
            .publish_metrics
            .record(outcome, started.elapsed());
        result
    }

    async fn dispatch(
        &self,
        event: &PreparedEvent,
        span: &tracing::Span,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<PublishAck, PublishError> {
        if self.shared.refuses_work() {
            return Err(PublishError::Rejected);
        }
        let message = source_message(
            event,
            span,
            &self.shared.source_stream,
            self.shared.max_payload_bytes,
        )?;
        send_publication(&self.shared, &event.subject, message, deadline, cancel)
            .instrument(span.clone())
            .await
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
    validate_header_bytes(&headers).map_err(|_| PublishError::Rejected)?;
    Ok(PublishMessage::build()
        .headers(headers)
        .payload(event.payload.clone()))
}

/// Sends the finalized message unchanged and awaits its `JetStream` acknowledgement.
/// The native client enforces broker size and shared publication admission.
///
/// Unlike [`Producer::publish`], this records no span, outcome metric or
/// drain check; the caller owns those. A full publication window still counts
/// toward `messaging_publish_admission_refused_total`.
pub(crate) async fn send_publication(
    shared: &Shared,
    subject: &str,
    message: PublishMessage,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<PublishAck, PublishError> {
    let deadline = within_operation_budget(deadline);
    if cancel.is_cancelled() || Instant::now() >= deadline {
        return Err(PublishError::Rejected);
    }
    let classify = |error: async_nats::jetstream::context::PublishError| {
        classify_publish(&error, &shared.publish_admission_refused)
    };
    let exchange = async {
        let ack = shared
            .jetstream
            .send_publish(subject.to_owned(), message)
            .await
            .map_err(classify)?
            .await
            .map_err(classify)?;
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

/// `Rejected` only where the broker or client proves nothing was stored: a
/// closed list of kinds and codes. Every other failure may follow a stored
/// publication, so it is `Ambiguous`.
fn classify_publish(
    error: &async_nats::jetstream::context::PublishError,
    admission_refused: &metrics::Counter,
) -> PublishError {
    match error.kind() {
        PublishErrorKind::StreamNotFound
        | PublishErrorKind::WrongLastMessageId
        | PublishErrorKind::WrongLastSequence
        | PublishErrorKind::MaxPayloadExceeded => PublishError::Rejected,
        PublishErrorKind::MaxAckPending => {
            admission_refused.increment(1);
            PublishError::Rejected
        }
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

/// Describes the publication metrics to the installed recorder. Repeating it
/// is harmless.
pub(crate) fn describe_metrics() {
    use metrics::{Unit, describe_counter, describe_histogram};

    describe_counter!(
        "messaging_publish_admission_refused_total",
        "Native publication admission refusals because all ACK permits are occupied"
    );
    describe_counter!(
        "messaging_publish_total",
        "Publications by result: acknowledged, rejected or ambiguous"
    );
    describe_histogram!(
        "messaging_publish_duration_seconds",
        Unit::Seconds,
        "Time from publication start to its result"
    );
}

#[cfg(test)]
mod tests {
    use domain_events::{Event, EventPayload};

    use super::*;

    #[test]
    fn publish_results_are_listed_in_index_order() {
        crate::observe::assert_all_in_index_order::<PublishOutcome>();
    }

    #[derive(serde::Serialize)]
    struct Created;

    impl EventPayload for Created {
        const EVENT_TYPE: &'static str = "order.created";
        const SCHEMA_VERSION: u16 = 1;
    }

    #[test]
    fn only_native_capacity_refusal_counts_as_admission_refusal() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        metrics::with_local_recorder(&recorder, || {
            let refused = metrics::counter!("messaging_publish_admission_refused_total");
            for (kind, rejected) in [
                (PublishErrorKind::StreamNotFound, true),
                (PublishErrorKind::WrongLastMessageId, true),
                (PublishErrorKind::WrongLastSequence, true),
                (PublishErrorKind::MaxPayloadExceeded, true),
                (PublishErrorKind::MaxAckPending, true),
                (PublishErrorKind::Other, false),
                (PublishErrorKind::TimedOut, false),
                (PublishErrorKind::BrokenPipe, false),
            ] {
                let error = async_nats::jetstream::context::PublishError::new(kind);
                assert_eq!(
                    matches!(classify_publish(&error, &refused), PublishError::Rejected),
                    rejected
                );
            }
        });
        assert!(
            recorder
                .handle()
                .render()
                .contains("messaging_publish_admission_refused_total 1")
        );
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
