//! One delivery: its typed handler run under the handler time limit, the
//! settlement that outcome selects, and the dead-letter transfer.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use async_nats::HeaderMap;
use async_nats::jetstream::{AckKind, Message};
use futures_util::FutureExt as _;
use operation_context::{Deadline, OperationContext, Stopped};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use crate::error::HandlerError;
use crate::messaging::{BROKER_OPERATION_BUDGET, Shared};
use crate::observe::{OutcomeLabel as _, Outcomes, outcome_label};
use crate::producer::send_publication;
use crate::registry::{DispatchError, Registry};
use crate::wire;

pub(super) const HANDLER_TIMEOUT: Duration = Duration::from_secs(30);
/// Delays before the second to fifth delivery; the fifth failure dead-letters.
const RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
];
const MAX_DELIVERIES: usize = RETRY_DELAYS.len() + 1;
/// Redelivery delay after a dead-letter publication fails.
const SETTLEMENT_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Runs and settles the deliveries of one admitted consumer.
#[derive(Debug)]
pub(super) struct DeliveryHandler {
    pub(super) shared: Arc<Shared>,
    registry: Registry,
    metrics: HandlerMetrics,
    /// The durable's filter: the low-cardinality name of what it processes.
    filter_subject: String,
    /// `process <filter>`, the exported name of every delivery span.
    span_name: String,
    dlq_subject: String,
    dlq_stream: String,
}

/// `event_type` label of a delivery whose type has no registered handler,
/// including a malformed envelope. Its own header text is publisher-chosen
/// and never becomes a label.
const UNREGISTERED: &str = "unregistered";

/// Handler outcome metrics of one event type.
#[derive(Debug)]
struct EventMetrics {
    event_type: &'static str,
    outcomes: Outcomes<Outcome>,
}

/// Handler outcome metrics per handled event type, so an operator can tell
/// which one fails or runs slowly. The label values are the payload types'
/// constants, a set closed at admission; schema versions of one type share
/// its label.
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
                "messaging_handler_total",
                "messaging_handler_duration_seconds",
                "outcome",
                Some(event_type),
            ),
        };
        Self {
            registered: registry
                .handled()
                .map(|route| (route.key.0, event(route.key.0)))
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
    /// How one admitted delivery ended.
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

/// How a delivery is settled after its handler ran.
#[derive(Debug, PartialEq, Eq)]
enum Settlement {
    Acknowledge,
    DeadLetter(wire::DeadLetterReason),
    Redeliver(Duration),
}

/// The settlement of attempt `delivery_attempt` (from 1) that ended with `outcome`.
fn settlement(outcome: Outcome, delivery_attempt: usize) -> Settlement {
    match outcome {
        Outcome::Success => Settlement::Acknowledge,
        // The dead-letter reason is the Go wire vocabulary, which has one
        // word for every delivery that a retry cannot help.
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

/// Builds the dead-letter transfer message; `None` when the source has no
/// transfer identity.
fn transfer_message(
    source: &Message,
    dlq_stream: &str,
    reason: wire::DeadLetterReason,
) -> Option<async_nats::jetstream::message::PublishMessage> {
    let empty = HeaderMap::new();
    let original = source.headers.as_ref().unwrap_or(&empty);
    let transfer_id = source.info().ok().and_then(|info| {
        wire::dead_letter_id(
            info.stream,
            info.stream_sequence,
            info.published,
            wire::header_value(original, wire::name::NATS_MSG_ID),
        )
        .ok()
    })?;
    let headers = wire::dead_letter_headers(
        original,
        source.subject.as_str(),
        dlq_stream,
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
        filter_subject: String,
        dlq_subject: String,
        dlq_stream: String,
    ) -> Self {
        Self {
            shared,
            metrics: HandlerMetrics::register(&registry),
            registry,
            span_name: format!("process {filter_subject}"),
            filter_subject,
            dlq_subject,
            dlq_stream,
        }
    }

    /// Runs the handler for one delivery and settles it when the delivery
    /// carries `JetStream` metadata. A handler failure stays with the
    /// delivery: it is logged, counted and retried by the broker. Without
    /// metadata the attempt count and deterministic DLQ identity are
    /// unavailable, so the delivery is logged and left unacknowledged for
    /// ack-wait redelivery.
    pub(super) async fn handle(&self, message: Message, cancel: CancellationToken) {
        let Some(delivery_attempt) = message
            .info()
            .ok()
            .and_then(|info| usize::try_from(info.delivered).ok())
        else {
            // The attempt count and deterministic DLQ identity both need
            // delivery metadata; leave the delivery for ack-wait redelivery.
            tracing::warn!(
                subject = message.subject.as_str(),
                "messaging_delivery_metadata_missing"
            );
            return;
        };
        let empty = HeaderMap::new();
        let headers = message.headers.as_ref().unwrap_or(&empty);
        let envelope = if message.payload.len() > self.shared.max_payload_bytes {
            None
        } else {
            wire::decode_envelope(message.subject.as_ref(), headers, message.payload.clone()).ok()
        };
        let Some(envelope) = envelope else {
            return self
                .dead_letter(
                    &message,
                    wire::DeadLetterReason::Malformed,
                    UNREGISTERED,
                    &cancel,
                )
                .await;
        };
        let metrics = self.metrics.get(envelope.event_type());
        // A delivery the broker redelivered after its last settlement was
        // lost, such as an unconfirmed dead-letter acknowledgement.
        if delivery_attempt > MAX_DELIVERIES {
            return self
                .dead_letter(
                    &message,
                    wire::DeadLetterReason::Exhausted,
                    metrics.event_type,
                    &cancel,
                )
                .await;
        }

        let span = tracing::info_span!(
            "messaging_process",
            otel.name = self.span_name.as_str(),
            otel.kind = "consumer",
            messaging.system = "nats",
            messaging.operation.type = "process",
            messaging.operation.name = "process",
            messaging.destination.name = message.subject.as_str(),
            messaging.destination.template = self.filter_subject.as_str(),
            outcome = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        crate::trace::set_remote_parent(&span, headers);
        self.process(&message, envelope, metrics, delivery_attempt, &cancel)
            .instrument(span)
            .await;
    }

    /// Runs the typed handler inside the delivery span and settles the source.
    async fn process(
        &self,
        message: &Message,
        envelope: wire::InboundEnvelope,
        metrics: &EventMetrics,
        delivery_attempt: usize,
        cancel: &CancellationToken,
    ) {
        let started = Instant::now();
        let outcome = run_handler(&self.registry, message.subject.as_ref(), envelope, cancel).await;
        metrics.outcomes.record(outcome, started.elapsed());
        let event_type = metrics.event_type;
        let label = outcome.label();
        let span = tracing::Span::current();
        span.record("outcome", label);
        if outcome != Outcome::Success {
            span.record("error.type", label);
            span.record("otel.status_code", "ERROR");
        }
        // A handler's cause stays with the handler, which logs it inside this
        // span; the adapter reports only its closed outcome vocabulary.
        match outcome {
            Outcome::Success => {}
            Outcome::Panicked => tracing::error!(
                subject = message.subject.as_str(),
                event_type,
                attempt = delivery_attempt,
                outcome = label,
                "messaging_delivery_failed"
            ),
            _ => tracing::warn!(
                subject = message.subject.as_str(),
                event_type,
                attempt = delivery_attempt,
                outcome = label,
                "messaging_delivery_failed"
            ),
        }

        match settlement(outcome, delivery_attempt) {
            Settlement::Acknowledge => acknowledge(&self.shared.client, message).await,
            Settlement::DeadLetter(reason) => {
                self.dead_letter(message, reason, event_type, cancel).await;
            }
            Settlement::Redeliver(delay) => redeliver_after(message, delay).await,
        }
    }

    /// Copies the source to the dead-letter stream, then acknowledges it.
    /// A failed transfer keeps the source for a later redelivery.
    async fn dead_letter(
        &self,
        source: &Message,
        reason: wire::DeadLetterReason,
        event_type: &'static str,
        cancel: &CancellationToken,
    ) {
        let Some(message) = transfer_message(source, &self.dlq_stream, reason) else {
            tracing::error!(
                subject = source.subject.as_str(),
                reason = reason.as_str(),
                "messaging_dead_letter_identity_unavailable"
            );
            return redeliver_after(source, SETTLEMENT_RETRY_DELAY).await;
        };
        let reason = reason.as_str();

        // DLQ copies malformed source bytes: native send enforces broker bounds.
        let result = send_publication(
            &self.shared,
            &self.dlq_subject,
            message,
            Instant::now() + BROKER_OPERATION_BUDGET,
            cancel,
        )
        .await;
        let outcome = match &result {
            Ok(_) => "accepted",
            Err(crate::PublishError::Ambiguous) => "ambiguous",
            Err(crate::PublishError::Rejected) => "rejected",
        };
        metrics::counter!(
            "messaging_dead_letter_total",
            "event_type" => event_type,
            "reason" => reason,
            "outcome" => outcome
        )
        .increment(1);
        if result.is_ok() {
            acknowledge(&self.shared.client, source).await;
        } else {
            tracing::error!(
                subject = source.subject.as_str(),
                event_type,
                reason,
                outcome,
                "messaging_dead_letter_transfer_failed"
            );
            redeliver_after(source, SETTLEMENT_RETRY_DELAY).await;
        }
    }
}

/// Runs the typed handler of one delivery under the handler time limit.
///
/// The handler context is cancelled when the invocation ends, by return, time
/// limit or panic, so work the handler started with it stops too.
async fn run_handler(
    registry: &Registry,
    subject: &str,
    envelope: wire::InboundEnvelope,
    cancel: &CancellationToken,
) -> Outcome {
    let deadline = Deadline::new(Instant::now(), HANDLER_TIMEOUT);
    let handler_cancel = cancel.child_token();
    let _ended = handler_cancel.clone().drop_guard();
    let context = OperationContext::new(Some(deadline), handler_cancel);
    let dispatch = registry.dispatch(subject, envelope, context.clone());
    let stopped = |reason| match reason {
        Stopped::Deadline => Outcome::TimedOut,
        Stopped::Cancelled => Outcome::Retryable,
    };
    let result = tokio::select! {
        biased;
        reason = context.wait_stopped() => return stopped(reason),
        result = AssertUnwindSafe(dispatch).catch_unwind() => result,
    };
    match result {
        // A final synchronous poll can cross the cutoff before returning Ready.
        Ok(Ok(())) => context.stopped().map_or(Outcome::Success, stopped),
        Ok(Err(DispatchError::Handler(HandlerError::Permanent))) => Outcome::Permanent,
        Ok(Err(DispatchError::Handler(HandlerError::Retryable))) => Outcome::Retryable,
        Ok(Err(DispatchError::Unhandled)) => Outcome::Unhandled,
        Ok(Err(DispatchError::Undecodable)) => Outcome::Undecodable,
        Err(_) => Outcome::Panicked,
    }
}

/// Confirms the consumer ACK. A lost request may redeliver; a lost reply may
/// hide an already settled delivery. Neither proves the source was deleted.
///
/// The confirmation travels through the client's shared request inbox under
/// its `BROKER_OPERATION_BUDGET` request timeout; `Message::double_ack`
/// subscribes and unsubscribes a new inbox per call.
async fn acknowledge(client: &async_nats::Client, source: &Message) {
    let confirmed = match source.reply.clone() {
        Some(reply) => client.request(reply, AckKind::Ack.into()).await.is_ok(),
        None => false,
    };
    if !confirmed {
        tracing::warn!(
            subject = source.subject.as_str(),
            "messaging_source_ack_unconfirmed"
        );
        metrics::counter!("messaging_settlement_failures_total", "operation" => "ack").increment(1);
    }
}

/// Asks the broker to redeliver the source after `delay`; if that request is
/// lost, ack wait redelivers it anyway.
pub(super) async fn redeliver_after(source: &Message, delay: Duration) {
    let requested = tokio::time::timeout(
        BROKER_OPERATION_BUDGET,
        source.ack_with(AckKind::Nak(Some(delay))),
    )
    .await;
    if !matches!(requested, Ok(Ok(()))) {
        tracing::warn!(
            subject = source.subject.as_str(),
            "messaging_redelivery_request_failed"
        );
        metrics::counter!("messaging_settlement_failures_total", "operation" => "nak").increment(1);
    }
}

/// Describes the delivery metrics to the installed recorder. Repeating it is
/// harmless.
pub(super) fn describe_metrics() {
    use metrics::{Unit, describe_counter, describe_histogram};

    describe_counter!(
        "messaging_handler_total",
        "Handled deliveries by event type and outcome"
    );
    describe_histogram!(
        "messaging_handler_duration_seconds",
        Unit::Seconds,
        "Handler run time by event type and outcome"
    );
    describe_counter!(
        "messaging_dead_letter_total",
        "Dead-letter transfers by event type, reason and transfer outcome"
    );
    describe_counter!(
        "messaging_settlement_failures_total",
        "Unconfirmed source acknowledgements and failed redelivery requests"
    );
}

#[cfg(test)]
mod tests {
    use domain_events::EventPayload;

    use super::*;
    use crate::registry::Route;

    #[test]
    fn handler_outcomes_are_listed_in_index_order() {
        crate::observe::assert_all_in_index_order::<Outcome>();
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
            let mut registry = Registry::try_new([
                Route::new::<Created>("orders.created"),
                Route::new::<Shipped>("orders.shipped"),
            ])
            .unwrap();
            registry
                .register::<Created, _, _>(|_, _| async { Ok(()) })
                .unwrap();
            let metrics = HandlerMetrics::register(&registry);

            let handled = metrics.get("order.created");
            assert_eq!(handled.event_type, "order.created");
            handled
                .outcomes
                .record(Outcome::Retryable, Duration::from_millis(5));
            // A routed type without a handler and a type no route knows.
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

    fn created_envelope() -> wire::InboundEnvelope {
        wire::InboundEnvelope {
            message_id: "event-1".to_owned(),
            event_type: Created::EVENT_TYPE.to_owned(),
            schema_version: Created::SCHEMA_VERSION,
            occurred_at: time::OffsetDateTime::UNIX_EPOCH,
            payload: bytes::Bytes::from_static(b"null"),
        }
    }

    /// A handler hands its context cancellation to work it starts. That work must hear when
    /// the delivery is over, or it outlives a handler the adapter gave up on.
    #[tokio::test(start_paused = true)]
    #[expect(
        clippy::excessive_nesting,
        reason = "the outcome cancellation oracle keeps context extraction beside the returned async handler"
    )]
    async fn the_handler_token_is_cancelled_when_its_delivery_ends() {
        for expected in ["timeout", "success", "panic"] {
            let (token_tx, token_rx) = tokio::sync::oneshot::channel();
            let token_tx = std::sync::Mutex::new(Some(token_tx));
            let mut registry =
                Registry::try_new([Route::new::<Created>("orders.created")]).unwrap();
            registry
                .register::<Created, _, _>(move |_, context| {
                    let token_tx = token_tx.lock().unwrap().take();
                    async move {
                        assert_eq!(context.remaining(), Some(HANDLER_TIMEOUT));
                        token_tx
                            .unwrap()
                            .send(context.cancellation().clone())
                            .unwrap();
                        if expected == "timeout" {
                            std::future::pending::<()>().await;
                        }
                        assert_ne!(expected, "panic", "handler panic fixture");
                        Ok(())
                    }
                })
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
    async fn a_stopped_handler_cannot_report_ready_success() {
        for expected in ["timeout", "retryable"] {
            let mut registry =
                Registry::try_new([Route::new::<Created>("orders.created")]).unwrap();
            registry
                .register::<Created, _, _>(move |_, context| async move {
                    if expected == "timeout" {
                        // advance moves the clock before yielding; dropping that
                        // yield keeps the handler's final poll synchronous and ready.
                        let _ = tokio::time::advance(HANDLER_TIMEOUT).now_or_never();
                        assert_eq!(context.remaining(), Some(Duration::ZERO));
                    } else {
                        context.cancel();
                    }
                    Ok(())
                })
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
        let mut registry = Registry::try_new([Route::new::<Created>("orders.created")]).unwrap();
        registry
            .register::<Created, _, _>(|_, _| async {
                panic!("cancelled delivery reached the handler");
            })
            .unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let outcome = run_handler(&registry, "orders.created", created_envelope(), &cancel).await;
        assert_eq!(outcome.label(), "retryable");
    }
}
