use std::error::Error as _;
use std::sync::Arc;

use async_nats::ToSubject;
use async_nats::jetstream::ErrorCode;
use async_nats::jetstream::context::PublishErrorKind;
use async_nats::jetstream::message::PublishMessage;
use operation_context::{Deadline, OperationContext};
use tokio::time::Instant;
use tracing::Instrument as _;

use crate::budget::BROKER_OPERATION_BUDGET;
use crate::error::{EnvelopeError, PublishError, PublishRejection};
use crate::messaging::Shared;
use crate::observe::{OutcomeLabel as _, Outcomes, outcome_label, record_span_outcome};
use crate::prepared::PreparedEvent;
use crate::wire::{encode_prepared, validate_header_bytes};

outcome_label! {
    enum PublishOutcome {
        Acknowledged => "acknowledged",
        Rejected => "rejected",
        Ambiguous => "ambiguous",
    }
}

const PUBLISH_TOTAL: &str = "messaging_publish_total";
const PUBLISH_DURATION_SECONDS: &str = "messaging_publish_duration_seconds";
const PUBLISH_ADMISSION_REFUSED_TOTAL: &str = "messaging_publish_admission_refused_total";

#[derive(Debug)]
pub(crate) struct PublishMetrics {
    outcomes: Outcomes<PublishOutcome>,
    admission_refused: metrics::Counter,
}

impl PublishMetrics {
    pub(crate) fn register() -> Self {
        Self {
            outcomes: Outcomes::register(PUBLISH_TOTAL, PUBLISH_DURATION_SECONDS, "result", None),
            admission_refused: metrics::counter!(PUBLISH_ADMISSION_REFUSED_TOTAL),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Producer {
    pub(crate) shared: Arc<Shared>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishAck {
    pub stream: String,
    pub sequence: u64,
    pub duplicate: bool,
}

impl Producer {
    pub async fn publish(
        &self,
        event: &PreparedEvent,
        context: &OperationContext,
    ) -> Result<PublishAck, PublishError> {
        self.publish_after_admission(event, context, std::future::ready(Ok(())))
            .await
    }

    pub(crate) async fn publish_after_admission(
        &self,
        event: &PreparedEvent,
        context: &OperationContext,
        admission: impl Future<Output = Result<(), PublishError>>,
    ) -> Result<PublishAck, PublishError> {
        let started = Instant::now();
        let span = tracing::info_span!(
            "messaging_publish",
            otel.name = format!("publish {}", event.subject()),
            otel.kind = "producer",
            messaging.system.name = "nats",
            messaging.operation.type = "send",
            messaging.operation.name = "publish",
            messaging.destination.name = event.subject(),
            outcome = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        let result = self
            .send_prepared(event, &span, context, admission)
            .instrument(span.clone())
            .await;
        let outcome = match &result {
            Ok(_) => PublishOutcome::Acknowledged,
            Err(PublishError::Rejected(_)) => PublishOutcome::Rejected,
            Err(PublishError::Ambiguous) => PublishOutcome::Ambiguous,
        };
        let label = outcome.label();
        record_span_outcome(&span, label, result.is_err().then_some(label));
        if let Err(error) = &result {
            span.in_scope(|| {
                tracing::warn!(
                    subject = event.subject(),
                    outcome = label,
                    reason = %error,
                    "messaging_publish_failed"
                );
            });
        }
        self.shared
            .publish_metrics
            .outcomes
            .record(outcome, started.elapsed());
        result
    }

    async fn send_prepared(
        &self,
        event: &PreparedEvent,
        span: &tracing::Span,
        context: &OperationContext,
        admission: impl Future<Output = Result<(), PublishError>>,
    ) -> Result<PublishAck, PublishError> {
        if self.shared.refuses_work() {
            return Err(PublishRejection::Unavailable.into());
        }
        let message = source_message(
            event,
            span,
            &self.shared.source_stream,
            self.shared.max_payload_bytes,
        )?;
        admission.await?;
        send_publication(
            &self.shared,
            event.subject_handle().clone(),
            message,
            context,
        )
        .await
    }
}

fn source_message(
    event: &PreparedEvent,
    span: &tracing::Span,
    expected_stream: &str,
    max_payload_bytes: usize,
) -> Result<PublishMessage, PublishRejection> {
    if event.payload().len() > max_payload_bytes {
        return Err(EnvelopeError::PayloadTooLarge.into());
    }
    let mut headers = encode_prepared(event)?;
    crate::trace::inject(span, &mut headers);
    headers.insert(async_nats::header::NATS_EXPECTED_STREAM, expected_stream);
    validate_header_bytes(&headers)?;
    Ok(PublishMessage::build()
        .headers(headers)
        .payload(event.payload().clone()))
}

pub(crate) async fn send_publication(
    shared: &Shared,
    subject: impl ToSubject,
    message: PublishMessage,
    context: &OperationContext,
) -> Result<PublishAck, PublishError> {
    let local = Deadline::after(BROKER_OPERATION_BUDGET);
    let context = OperationContext::new(
        Some(context.effective_deadline(local)),
        context.cancellation().clone(),
    );
    if context.stopped().is_some() {
        return Err(PublishRejection::ContextStopped.into());
    }
    let exchange = async {
        let ack = shared
            .jetstream
            .send_publish(subject, message)
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
    let result = tokio::select! {
        biased;
        _ = context.wait_stopped() => Err(PublishError::Ambiguous),
        result = exchange => result,
    };
    if matches!(
        result,
        Err(PublishError::Rejected(PublishRejection::WindowFull))
    ) {
        shared.publish_metrics.admission_refused.increment(1);
    }
    result
}

fn classify_publish(error: &async_nats::jetstream::context::PublishError) -> PublishError {
    match error.kind() {
        PublishErrorKind::StreamNotFound
        | PublishErrorKind::WrongLastMessageId
        | PublishErrorKind::WrongLastSequence => PublishRejection::BrokerRefused.into(),
        PublishErrorKind::MaxPayloadExceeded => PublishRejection::BrokerSizeLimit.into(),
        PublishErrorKind::MaxAckPending => PublishRejection::WindowFull.into(),
        PublishErrorKind::Other => {
            let code = error
                .source()
                .and_then(|source| source.downcast_ref::<async_nats::jetstream::Error>())
                .map(async_nats::jetstream::Error::error_code);
            match code {
                Some(
                    ErrorCode::STREAM_MESSAGE_EXCEEDS_MAXIMUM
                    | ErrorCode::STREAM_HEADER_EXCEEDS_MAXIMUM,
                ) => PublishRejection::BrokerSizeLimit.into(),
                Some(ErrorCode::STREAM_SEALED) => PublishRejection::BrokerStreamSealed.into(),
                Some(
                    ErrorCode::BAD_REQUEST
                    | ErrorCode::ACCOUNT_RESOURCES_EXCEEDED
                    | ErrorCode::INSUFFICIENT_RESOURCES
                    | ErrorCode::STORAGE_RESOURCES_EXCEEDED
                    | ErrorCode::JETSTREAM_NOT_ENABLED
                    | ErrorCode::JETSTREAM_NOT_ENABLED_FOR_ACCOUNT
                    | ErrorCode::STREAM_NOT_MATCH
                    | ErrorCode::STREAM_MISMATCH
                    | ErrorCode::STREAM_LIMITS
                    | ErrorCode::STREAM_STORE_FAILED
                    | ErrorCode::STREAM_WRONG_LAST_MESSAGE_ID
                    | ErrorCode::STREAM_WRONG_LAST_SEQUENCE,
                ) => PublishRejection::BrokerRefused.into(),
                _ => PublishError::Ambiguous,
            }
        }
        PublishErrorKind::TimedOut | PublishErrorKind::BrokenPipe => PublishError::Ambiguous,
    }
}

pub(crate) fn describe_metrics() {
    use metrics::{Unit, describe_counter, describe_histogram};

    describe_counter!(
        PUBLISH_ADMISSION_REFUSED_TOTAL,
        "Native publication admission refusals because all ACK permits are occupied"
    );
    describe_counter!(
        PUBLISH_TOTAL,
        "Publications by result: acknowledged, rejected or ambiguous"
    );
    describe_histogram!(
        PUBLISH_DURATION_SECONDS,
        Unit::Seconds,
        "Time from publication start to its result"
    );
}

#[cfg(test)]
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
    fn only_native_capacity_refusal_counts_as_admission_refusal() {
        for (kind, expected) in [
            (
                PublishErrorKind::StreamNotFound,
                PublishRejection::BrokerRefused.into(),
            ),
            (
                PublishErrorKind::WrongLastMessageId,
                PublishRejection::BrokerRefused.into(),
            ),
            (
                PublishErrorKind::WrongLastSequence,
                PublishRejection::BrokerRefused.into(),
            ),
            (
                PublishErrorKind::MaxPayloadExceeded,
                PublishRejection::BrokerSizeLimit.into(),
            ),
            (
                PublishErrorKind::MaxAckPending,
                PublishRejection::WindowFull.into(),
            ),
            (PublishErrorKind::Other, PublishError::Ambiguous),
            (PublishErrorKind::TimedOut, PublishError::Ambiguous),
            (PublishErrorKind::BrokenPipe, PublishError::Ambiguous),
        ] {
            let error = async_nats::jetstream::context::PublishError::new(kind);
            assert_eq!(classify_publish(&error), expected);
        }
    }

    #[test]
    fn final_expected_stream_header_counts_toward_the_receiving_limit() {
        let event = Event {
            id: "event-1".to_owned(),
            occurred_at: time::UtcDateTime::from_unix_timestamp(1_700_000_000)
                .expect("fixed timestamp is valid"),
            payload: Created,
        };
        let prepared = crate::Registry::default()
            .declare_publication(&crate::Route::new("orders.created"), 4)
            .unwrap()
            .prepare(&event)
            .expect("null fits four payload bytes");
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
        std::assert_matches!(
            source_message(&prepared, &tracing::Span::none(), &(stream + "s"), 4),
            Err(PublishRejection::InvalidEnvelope(
                EnvelopeError::HeadersTooLarge
            ))
        );
    }
}
