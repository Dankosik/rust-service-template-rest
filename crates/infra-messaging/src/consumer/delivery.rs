use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::jetstream::message::Info;
use async_nats::jetstream::{AckKind, Message};
use async_nats::{HeaderMap, Request, Subject};
use futures_util::FutureExt as _;
use operation_context::{Deadline, OperationContext, Stopped};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use super::ConsumerError;
use crate::budget::BROKER_OPERATION_BUDGET;
use crate::error::{HandlerError, PublishError, PublishRejection};
use crate::messaging::Shared;
use crate::observe::{OutcomeLabel as _, Outcomes, outcome_label, record_span_outcome};
use crate::producer::send_publication;
use crate::registry::{DispatchError, Registry};
use crate::topology::ConsumerAdmission;
use crate::wire;

pub(super) const HANDLER_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
];
const MAX_DELIVERIES: usize = RETRY_DELAYS.len() + 1;
const SETTLEMENT_RETRY_DELAY: Duration = Duration::from_secs(30);
const HANDLER_TOTAL: &str = "messaging_handler_total";
const HANDLER_DURATION_SECONDS: &str = "messaging_handler_duration_seconds";
const DEAD_LETTER_TOTAL: &str = "messaging_dead_letter_total";
const LAST_DEAD_LETTER_TIMESTAMP_SECONDS: &str = "messaging_last_dead_letter_timestamp_seconds";
const SETTLEMENT_FAILURES_TOTAL: &str = "messaging_settlement_failures_total";

#[derive(Debug)]
pub(super) struct DeliveryHandler {
    shared: Arc<Shared>,
    registry: Registry,
    metrics: HandlerMetrics,
    pub(super) abort: CancellationToken,
    pub(super) filter_subject: String,
    span_name: String,
    dead_letter_subject: Subject,
    dead_letter_stream: String,
}

const UNREGISTERED: &str = "unregistered";

#[derive(Debug)]
struct EventMetrics {
    event_type: &'static str,
    outcomes: Outcomes<Outcome>,
}

#[derive(Debug)]
struct HandlerMetrics {
    registered: foldhash::HashMap<&'static str, EventMetrics>,
    unregistered: EventMetrics,
}

impl HandlerMetrics {
    fn register(registry: &Registry) -> Self {
        let event = |event_type| EventMetrics {
            event_type,
            outcomes: Outcomes::register(
                HANDLER_TOTAL,
                HANDLER_DURATION_SECONDS,
                "outcome",
                Some(event_type),
            ),
        };
        Self {
            registered: registry
                .handled_routes()
                .map(|(key, _)| (key.event_type, event(key.event_type)))
                .collect(),
            unregistered: event(UNREGISTERED),
        }
    }

    fn get(&self, event_type: &str) -> &EventMetrics {
        self.registered
            .get(event_type)
            .unwrap_or(&self.unregistered)
    }
}

outcome_label! {
    enum Outcome {
        Success => "success",
        Permanent => "permanent",
        Retryable => "retryable",
        TimedOut => "timeout",
        Panicked => "panic",
        Unhandled => "unhandled",
        Undecodable => "undecodable",
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Settlement {
    Acknowledge,
    DeadLetter(wire::DeadLetterReason),
    Redeliver(Duration),
}

fn settlement(outcome: Outcome, delivery_attempt: usize) -> Settlement {
    match outcome {
        Outcome::Success => Settlement::Acknowledge,
        Outcome::Permanent | Outcome::Unhandled | Outcome::Undecodable => {
            Settlement::DeadLetter(wire::DeadLetterReason::Permanent)
        }
        Outcome::Retryable | Outcome::TimedOut | Outcome::Panicked => {
            match RETRY_DELAYS.get(delivery_attempt.saturating_sub(1)) {
                Some(&delay) => Settlement::Redeliver(delay),
                None => Settlement::DeadLetter(wire::DeadLetterReason::Exhausted),
            }
        }
    }
}

fn build_dead_letter_message(
    source: &Message,
    info: &Info<'_>,
    dead_letter_stream: &str,
    reason: wire::DeadLetterReason,
) -> Option<async_nats::jetstream::message::PublishMessage> {
    let empty = HeaderMap::new();
    let original = source.headers.as_ref().unwrap_or(&empty);
    let transfer_id = wire::dead_letter_id(
        info.stream,
        info.stream_sequence,
        info.published,
        wire::header_value(original, wire::header_name::NATS_MSG_ID),
    )
    .ok()?;
    let headers = wire::dead_letter_headers(
        original,
        source.subject.as_str(),
        dead_letter_stream,
        &transfer_id,
        reason,
    );
    Some(
        async_nats::jetstream::message::PublishMessage::build()
            .headers(headers)
            .payload(source.payload.clone()),
    )
}

impl DeliveryHandler {
    pub(super) fn new(
        shared: Arc<Shared>,
        registry: Registry,
        admission: ConsumerAdmission,
    ) -> Self {
        let filter_subject = admission.options.filter_subject;
        Self {
            shared,
            metrics: HandlerMetrics::register(&registry),
            registry,
            abort: CancellationToken::new(),
            span_name: format!("process {filter_subject}"),
            filter_subject,
            dead_letter_subject: admission.options.dlq_subject.into(),
            dead_letter_stream: admission.dead_letter.stream,
        }
    }

    pub(super) async fn handle(&self, message: Message) -> Result<(), ConsumerError> {
        let Some((info, delivery_attempt)) = message.info().ok().and_then(|info| {
            let attempt = usize::try_from(info.delivered).ok()?;
            Some((info, attempt))
        }) else {
            tracing::warn!(
                messaging.destination.template = self.filter_subject.as_str(),
                "messaging_delivery_metadata_missing"
            );
            return Ok(());
        };
        let empty = HeaderMap::new();
        let headers = message.headers.as_ref().unwrap_or(&empty);
        let envelope = if message.payload.len() > self.shared.max_payload_bytes {
            None
        } else {
            wire::decode_envelope_ref(message.subject.as_ref(), headers, &message.payload).ok()
        };
        let Some(envelope) = envelope else {
            return self
                .dead_letter(
                    &message,
                    &info,
                    wire::DeadLetterReason::Malformed,
                    UNREGISTERED,
                )
                .await;
        };
        let metrics = self.metrics.get(envelope.event_type);
        if delivery_attempt > MAX_DELIVERIES {
            return self
                .dead_letter(
                    &message,
                    &info,
                    wire::DeadLetterReason::Exhausted,
                    metrics.event_type,
                )
                .await;
        }

        let span = tracing::info_span!(
            "messaging_process",
            otel.name = self.span_name.as_str(),
            otel.kind = "consumer",
            messaging.system.name = "nats",
            messaging.operation.type = "process",
            messaging.operation.name = "process",
            messaging.destination.template = self.filter_subject.as_str(),
            outcome = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        crate::trace::set_remote_parent(&span, headers);
        self.process(&message, &info, envelope, metrics, delivery_attempt)
            .instrument(span)
            .await
    }

    async fn process(
        &self,
        message: &Message,
        info: &Info<'_>,
        envelope: wire::InboundEnvelopeRef<'_>,
        metrics: &EventMetrics,
        delivery_attempt: usize,
    ) -> Result<(), ConsumerError> {
        let started = Instant::now();
        let outcome = run_handler(
            &self.registry,
            message.subject.as_ref(),
            envelope,
            &self.abort,
        )
        .await;
        metrics.outcomes.record(outcome, started.elapsed());
        let event_type = metrics.event_type;
        let label = outcome.label();
        record_span_outcome(
            &tracing::Span::current(),
            label,
            (outcome != Outcome::Success).then_some(label),
        );
        match outcome {
            Outcome::Success => {}
            Outcome::Panicked => tracing::error!(
                messaging.destination.template = self.filter_subject.as_str(),
                event_type,
                attempt = delivery_attempt,
                outcome = label,
                "messaging_delivery_failed"
            ),
            _ => tracing::warn!(
                messaging.destination.template = self.filter_subject.as_str(),
                event_type,
                attempt = delivery_attempt,
                outcome = label,
                "messaging_delivery_failed"
            ),
        }

        match settlement(outcome, delivery_attempt) {
            Settlement::Acknowledge => {
                acknowledge(
                    &self.shared.connection.client,
                    message,
                    &self.filter_subject,
                )
                .await;
            }
            Settlement::DeadLetter(reason) => {
                return self.dead_letter(message, info, reason, event_type).await;
            }
            Settlement::Redeliver(delay) => {
                redeliver_after(message, delay, &self.filter_subject).await;
            }
        }
        Ok(())
    }

    async fn dead_letter(
        &self,
        source: &Message,
        info: &Info<'_>,
        reason: wire::DeadLetterReason,
        event_type: &'static str,
    ) -> Result<(), ConsumerError> {
        let Some(message) =
            build_dead_letter_message(source, info, &self.dead_letter_stream, reason)
        else {
            tracing::error!(
                messaging.destination.template = self.filter_subject.as_str(),
                reason = reason.label(),
                "messaging_dead_letter_identity_unavailable"
            );
            redeliver_after(source, SETTLEMENT_RETRY_DELAY, &self.filter_subject).await;
            return Ok(());
        };
        let reason = reason.label();

        let deadline = Deadline::after(BROKER_OPERATION_BUDGET);
        let result = send_publication(
            &self.shared,
            self.dead_letter_subject.clone(),
            message,
            Some(deadline),
            &self.abort,
        )
        .await;
        let outcome = match &result {
            Ok(_) => "accepted",
            Err(PublishError::Ambiguous) => "ambiguous",
            Err(PublishError::Rejected(_)) => "rejected",
        };
        metrics::counter!(
            DEAD_LETTER_TOTAL,
            "event_type" => event_type,
            "reason" => reason,
            "outcome" => outcome
        )
        .increment(1);
        if let Ok(timestamp) = SystemTime::now().duration_since(UNIX_EPOCH) {
            metrics::gauge!(
                LAST_DEAD_LETTER_TIMESTAMP_SECONDS,
                "event_type" => event_type,
                "reason" => reason,
                "outcome" => outcome
            )
            .set(timestamp.as_secs_f64());
        }
        let Err(failure) = result else {
            acknowledge(&self.shared.connection.client, source, &self.filter_subject).await;
            return Ok(());
        };
        tracing::error!(
            messaging.destination.template = self.filter_subject.as_str(),
            event_type,
            reason,
            outcome,
            "messaging_dead_letter_transfer_failed"
        );
        match failure {
            PublishError::Rejected(PublishRejection::BrokerSizeLimit) => {
                Err(ConsumerError::DeadLetterTooLarge {
                    sequence: info.stream_sequence,
                })
            }
            PublishError::Rejected(PublishRejection::BrokerStreamSealed) => {
                Err(ConsumerError::DeadLetterStreamSealed {
                    sequence: info.stream_sequence,
                })
            }
            PublishError::Rejected(
                PublishRejection::InvalidEnvelope(_)
                | PublishRejection::Unavailable
                | PublishRejection::ContextStopped
                | PublishRejection::WindowFull
                | PublishRejection::BrokerRefused,
            )
            | PublishError::Ambiguous => {
                redeliver_after(source, SETTLEMENT_RETRY_DELAY, &self.filter_subject).await;
                Ok(())
            }
        }
    }
}

async fn run_handler(
    registry: &Registry,
    subject: &str,
    envelope: wire::InboundEnvelopeRef<'_>,
    cancel: &CancellationToken,
) -> Outcome {
    let deadline = Deadline::after(HANDLER_TIMEOUT);
    let context = OperationContext::new(Some(deadline), cancel.child_token());
    let _ended = context.cancellation().drop_guard_ref();
    let dispatch = registry.dispatch(subject, envelope, context.clone());
    let stopped = |reason| match reason {
        Stopped::Deadline => Outcome::TimedOut,
        Stopped::Cancelled => Outcome::Retryable,
    };
    let result = match context.run(AssertUnwindSafe(dispatch).catch_unwind()).await {
        Ok(result) => result,
        Err(reason) => return stopped(reason),
    };
    match result {
        Ok(Ok(())) => Outcome::Success,
        Ok(Err(DispatchError::Handler(HandlerError::Permanent))) => Outcome::Permanent,
        Ok(Err(DispatchError::Handler(HandlerError::Retryable))) => Outcome::Retryable,
        Ok(Err(DispatchError::Unhandled)) => Outcome::Unhandled,
        Ok(Err(DispatchError::Undecodable)) => Outcome::Undecodable,
        Err(_) => Outcome::Panicked,
    }
}

async fn acknowledge(client: &async_nats::Client, source: &Message, filter_subject: &str) {
    let span = settlement_span(filter_subject, "ack");
    let error_type = if let Some(reply) = source.reply.clone() {
        let request = Request::new().timeout(None).payload(AckKind::Ack.into());
        match tokio::time::timeout(BROKER_OPERATION_BUDGET, client.send_request(reply, request))
            .instrument(span.clone())
            .await
        {
            Ok(Ok(_)) => {
                record_span_outcome(&span, "confirmed", None);
                return;
            }
            Ok(Err(_)) => "client_error",
            Err(_) => "timeout",
        }
    } else {
        "missing_reply"
    };
    record_span_outcome(&span, "unconfirmed", Some(error_type));
    span.in_scope(|| {
        tracing::warn!(
            messaging.destination.template = filter_subject,
            error.type = error_type,
            "messaging_source_ack_unconfirmed"
        );
    });
    metrics::counter!(SETTLEMENT_FAILURES_TOTAL, "operation" => "ack").increment(1);
}

pub(super) async fn redeliver_after(source: &Message, delay: Duration, filter_subject: &str) {
    let span = settlement_span(filter_subject, "nak");
    let error_type = match tokio::time::timeout(
        BROKER_OPERATION_BUDGET,
        source.ack_with(AckKind::Nak(Some(delay))),
    )
    .instrument(span.clone())
    .await
    {
        Ok(Ok(())) => {
            record_span_outcome(&span, "submitted", None);
            return;
        }
        Ok(Err(_)) => "client_error",
        Err(_) => "timeout",
    };
    record_span_outcome(&span, "failed", Some(error_type));
    span.in_scope(|| {
        tracing::warn!(
            messaging.destination.template = filter_subject,
            error.type = error_type,
            "messaging_redelivery_request_failed"
        );
    });
    metrics::counter!(SETTLEMENT_FAILURES_TOTAL, "operation" => "nak").increment(1);
}

fn settlement_span(filter_subject: &str, operation: &'static str) -> tracing::Span {
    tracing::info_span!(
        "messaging_settle",
        otel.name = operation,
        otel.kind = "client",
        messaging.system.name = "nats",
        messaging.operation.type = "settle",
        messaging.operation.name = operation,
        messaging.destination.template = filter_subject,
        outcome = tracing::field::Empty,
        error.type = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
    )
}

pub(super) fn describe_metrics() {
    use metrics::{Unit, describe_counter, describe_gauge, describe_histogram};

    describe_counter!(
        HANDLER_TOTAL,
        "Handled deliveries by event type and outcome"
    );
    describe_histogram!(
        HANDLER_DURATION_SECONDS,
        Unit::Seconds,
        "Handler run time by event type and outcome"
    );
    describe_counter!(
        DEAD_LETTER_TOTAL,
        "Dead-letter transfers by event type, reason and transfer outcome"
    );
    describe_gauge!(
        LAST_DEAD_LETTER_TIMESTAMP_SECONDS,
        Unit::Seconds,
        "Unix timestamp of the last dead-letter transfer by event type, reason and transfer outcome"
    );
    describe_counter!(
        SETTLEMENT_FAILURES_TOTAL,
        "Unconfirmed source acknowledgements and failed redelivery requests"
    );
}

#[cfg(test)]
mod tests {
    use domain_events::{Event, EventPayload};

    use super::*;
    use crate::registry::{EventHandler, Route};

    fn accept<T>(_: Event<T>, _: OperationContext) -> std::future::Ready<Result<(), HandlerError>> {
        std::future::ready(Ok(()))
    }

    #[derive(serde::Deserialize)]
    struct Created;

    impl EventPayload for Created {
        const EVENT_TYPE: &'static str = "order.created";
        const SCHEMA_VERSION: u16 = 1;
    }

    struct Shipped;

    impl EventPayload for Shipped {
        const EVENT_TYPE: &'static str = "order.shipped";
        const SCHEMA_VERSION: u16 = 1;
    }

    #[test]
    fn handler_metrics_carry_handled_event_types_and_no_publisher_chosen_text() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        metrics::with_local_recorder(&recorder, || {
            let created = Route::<Created>::new("orders.created");
            let mut registry = Registry::default();
            registry.declare(&created).unwrap();
            registry
                .declare(&Route::<Shipped>::new("orders.shipped"))
                .unwrap();
            registry.register(&created, accept).unwrap();
            let metrics = HandlerMetrics::register(&registry);

            let handled = metrics.get("order.created");
            assert_eq!(handled.event_type, "order.created");
            handled
                .outcomes
                .record(Outcome::Retryable, Duration::from_millis(5));
            for unhandled in ["order.shipped", "sentinel.publisher.chosen"] {
                let metrics = metrics.get(unhandled);
                assert_eq!(metrics.event_type, UNREGISTERED);
                metrics
                    .outcomes
                    .record(Outcome::Permanent, Duration::from_millis(5));
            }
        });

        let scrape = recorder.handle().render();
        for line in [
            r#"messaging_handler_total{outcome="retryable",event_type="order.created"} 1"#,
            r#"messaging_handler_total{outcome="permanent",event_type="unregistered"} 2"#,
        ] {
            assert!(scrape.contains(line), "{line} is missing from:\n{scrape}");
        }
        assert!(!scrape.contains("order.shipped") && !scrape.contains("sentinel"));
    }

    fn created_envelope() -> wire::InboundEnvelopeRef<'static> {
        wire::InboundEnvelopeRef {
            message_id: "event-1",
            event_type: Created::EVENT_TYPE,
            schema_version: Created::SCHEMA_VERSION,
            occurred_at: time::UtcDateTime::UNIX_EPOCH,
            payload: b"null",
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_handler_token_is_cancelled_when_its_delivery_ends() {
        struct OutcomeHandler {
            expected: &'static str,
            token_tx: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<CancellationToken>>>,
        }

        impl EventHandler<Created> for OutcomeHandler {
            async fn handle(
                &self,
                _event: Event<Created>,
                context: OperationContext,
            ) -> Result<(), HandlerError> {
                assert_eq!(context.remaining(), Some(HANDLER_TIMEOUT));
                self.token_tx
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(context.cancellation().clone())
                    .unwrap();
                if self.expected == "timeout" {
                    std::future::pending::<()>().await;
                }
                assert_ne!(self.expected, "panic", "handler panic fixture");
                Ok(())
            }
        }

        for expected in ["timeout", "success", "panic"] {
            let (token_tx, token_rx) = tokio::sync::oneshot::channel();
            let created = Route::<Created>::new("orders.created");
            let mut registry = Registry::default();
            registry.declare(&created).unwrap();
            registry
                .register(
                    &created,
                    OutcomeHandler {
                        expected,
                        token_tx: std::sync::Mutex::new(Some(token_tx)),
                    },
                )
                .unwrap();

            let delivery = CancellationToken::new();
            let outcome =
                run_handler(&registry, "orders.created", created_envelope(), &delivery).await;

            assert_eq!(outcome.label(), expected);
            let handler_token = token_rx.await.unwrap();
            assert!(handler_token.is_cancelled(), "{expected}");
            assert!(!delivery.is_cancelled(), "{expected}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_takes_precedence_over_the_handlers_final_result() {
        for (expected, result) in [
            ("timeout", Ok(())),
            ("timeout", Err(HandlerError::Permanent)),
            ("retryable", Ok(())),
        ] {
            let created = Route::<Created>::new("orders.created");
            let mut registry = Registry::default();
            registry.declare(&created).unwrap();
            registry
                .register(
                    &created,
                    move |_: Event<Created>, context: OperationContext| async move {
                        if expected == "timeout" {
                            let _ = tokio::time::advance(HANDLER_TIMEOUT).now_or_never();
                            assert_eq!(context.remaining(), Some(Duration::ZERO));
                        } else {
                            context.cancel();
                        }
                        result
                    },
                )
                .unwrap();
            let outcome = run_handler(
                &registry,
                "orders.created",
                created_envelope(),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(outcome.label(), expected);
        }
    }

    #[tokio::test]
    async fn a_cancelled_delivery_never_invokes_the_handler() {
        let created = Route::<Created>::new("orders.created");
        let mut registry = Registry::default();
        registry.declare(&created).unwrap();
        registry
            .register(&created, |_: Event<Created>, _: OperationContext| async {
                panic!("cancelled delivery reached the handler");
            })
            .unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let outcome = run_handler(&registry, "orders.created", created_envelope(), &cancel).await;
        assert_eq!(outcome.label(), "retryable");
    }
}
