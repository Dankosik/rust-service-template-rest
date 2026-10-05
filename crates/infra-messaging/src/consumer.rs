use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_nats::HeaderMap;
use async_nats::jetstream::consumer::pull;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, PullConsumer, ReplayPolicy};
use async_nats::jetstream::context::{ConsumerInfoError, ConsumerInfoErrorKind};
use async_nats::jetstream::stream::ConsumerErrorKind;
use async_nats::jetstream::{AckKind, Message};
use futures_util::{FutureExt as _, StreamExt as _};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use crate::error::{HandlerError, MessagingError};
use crate::messaging::{
    BROKER_OPERATION_BUDGET, ConsumerOptions, Outcomes, Shared, get_stream_failure,
    topology_failure,
};
use crate::producer::publish;
use crate::registry::{DispatchError, Registry};
use crate::wire::{self, HEADER_LIMIT_BYTES};

const HANDLER_TIMEOUT: Duration = Duration::from_secs(30);
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
/// The broker redelivers an unsettled message after the handler budget and
/// both settlement round trips.
const ACK_WAIT: Duration = HANDLER_TIMEOUT
    .saturating_add(BROKER_OPERATION_BUDGET)
    .saturating_add(BROKER_OPERATION_BUDGET)
    .saturating_add(Duration::from_secs(1));
/// Pause after a recoverable pull-stream error before polling again.
const STREAM_ERROR_BACKOFF: Duration = Duration::from_secs(1);
/// Lifetime of one capacity-reserved pull request.
const PULL_EXPIRES: Duration = Duration::from_secs(30);
/// Time past `PULL_EXPIRES` to wait for the broker's own expiry answer, which
/// ends an idle pull cleanly; without it the local timer races that answer
/// and reports a healthy idle expiry as a failed batch. It stays below the
/// SDK's expires-plus-five watchdog, which ends the batch silently, so a pull
/// the broker never answers still reaches the failed-batch arm.
const PULL_EXPIRY_GRACE: Duration = Duration::from_secs(2);
/// Redelivery delay of a delivery returned unhandled at drain. It outlasts
/// this replica's unsubscription, so the broker redelivers to another pull.
const RELEASE_DELAY: Duration = Duration::from_secs(1);

/// A bounded pull consumer admitted against an existing source stream.
#[derive(Debug)]
pub struct Consumer {
    pull: PullConsumer,
    concurrency: usize,
    delivery: Arc<Delivery>,
}

/// Terminal worker failure or incomplete shutdown. Reasons never contain input.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ConsumerError {
    #[error("messaging consumer could not start its pull stream")]
    Start,
    #[error("messaging durable consumer was deleted or replaced")]
    ConsumerLost,
    #[error("messaging consumer drain exceeded its shared deadline")]
    DrainTimedOut,
    #[error("messaging consumer task exited unexpectedly")]
    Close,
}

/// A running consumer owned and joined by the composition root.
#[derive(Debug)]
pub struct ConsumerHandle {
    stop: CancellationToken,
    force: CancellationToken,
    failure: watch::Receiver<Option<ConsumerError>>,
    task: Option<JoinHandle<Result<(), ConsumerError>>>,
    completed: Option<Result<(), ConsumerError>>,
}

/// Everything one delivery needs to run its handler and settle the source.
#[derive(Debug)]
struct Delivery {
    shared: Arc<Shared>,
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
    outcomes: Outcomes<7>,
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
                OUTCOME_LABELS,
                Some(event_type),
            ),
        };
        Self {
            registered: registry
                .handled()
                .map(|((event_type, _), _)| (event_type, event(event_type)))
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

/// How one admitted delivery ended. The discriminant indexes [`OUTCOME_LABELS`].
#[derive(Clone, Copy)]
enum Outcome {
    Success = 0,
    /// The handler rejected the event.
    Permanent = 1,
    Retryable = 2,
    TimedOut = 3,
    Panicked = 4,
    /// No handler claims this event type, schema version and subject.
    Unhandled = 5,
    /// The payload is not what the handler's type reads.
    Undecodable = 6,
}

/// Metric label of each [`Outcome`], indexed by its discriminant.
pub(crate) const OUTCOME_LABELS: [&str; 7] = [
    "success",
    "permanent",
    "retryable",
    "timeout",
    "panic",
    "unhandled",
    "undecodable",
];

impl Consumer {
    pub(crate) async fn admit(
        shared: Arc<Shared>,
        options: ConsumerOptions,
        registry: Registry,
    ) -> Result<Self, MessagingError> {
        // A route without a handler is publish-only and need not be consumed.
        if registry
            .handled()
            .any(|(_, subject)| !wire::subject_matches(&options.filter_subject, subject))
        {
            return Err(MessagingError::Configuration(
                "handled subject is outside the consumer filter",
            ));
        }
        let dlq_stream = shared.dlq_stream.clone().ok_or(MessagingError::Topology)?;
        let deadline = shared
            .startup_deadline
            .min(Instant::now() + BROKER_OPERATION_BUDGET);
        // The durable consumer is application-declared: `create_consumer`
        // creates it or updates its editable fields, and the broker refuses an
        // incompatible change. `max_ack_pending` keeps the broker default,
        // which bounds the durable across all replicas.
        let config = async_nats::jetstream::consumer::pull::Config {
            durable_name: Some(options.durable_name.clone()),
            filter_subject: options.filter_subject.clone(),
            deliver_policy: DeliverPolicy::All,
            ack_policy: AckPolicy::Explicit,
            ack_wait: ACK_WAIT,
            max_deliver: -1,
            replay_policy: ReplayPolicy::Instant,
            ..Default::default()
        };
        let admission = async {
            let stream = shared
                .jetstream
                .get_stream(&shared.source_stream)
                .await
                .map_err(|error| get_stream_failure(&error))?;
            stream.create_consumer(config).await.map_err(|error| {
                let broker = match error.kind() {
                    ConsumerErrorKind::JetStream(broker) => Some(broker),
                    _ => None,
                };
                topology_failure("create_consumer", &error, broker.as_ref())
            })
        };
        let pull = tokio::select! {
            biased;
            () = shared.startup_cancel.cancelled() => return Err(MessagingError::Cancelled),
            result = tokio::time::timeout_at(deadline, admission) => result
                .map_err(|_| MessagingError::TimedOut { budget: BROKER_OPERATION_BUDGET })??,
        };
        Ok(Self {
            pull,
            concurrency: options.concurrency,
            delivery: Arc::new(Delivery {
                shared,
                metrics: HandlerMetrics::register(&registry),
                registry,
                span_name: format!("process {}", options.filter_subject),
                filter_subject: options.filter_subject,
                dlq_subject: options.dlq_subject,
                dlq_stream,
            }),
        })
    }

    /// Starts admission under the root token; admitted work has its own lifetime.
    #[must_use]
    pub fn start(self, cancel: &CancellationToken) -> ConsumerHandle {
        let stop = cancel.child_token();
        let force = CancellationToken::new();
        let task_stop = stop.clone();
        let task_force = force.clone();
        let (failure_tx, failure) = watch::channel(None);
        let task = tokio::spawn(async move {
            let shared = Arc::clone(&self.delivery.shared);
            let result = self.run(task_stop, task_force).await;
            if let Err(error) = &result {
                shared.failed.store(true, Ordering::Release);
                tracing::error!(reason = %error, "messaging consumer stopped");
                let _ = failure_tx.send(Some(error.clone()));
            }
            result
        });
        ConsumerHandle {
            stop,
            force,
            failure,
            task: Some(task),
            completed: None,
        }
    }

    /// Reserves free handler/settlement slots before each one-shot pull.
    /// Drain returns received but unadmitted messages and joins admitted work;
    /// dropping the join set at forced shutdown aborts every delivery.
    async fn run(
        self,
        stop: CancellationToken,
        force: CancellationToken,
    ) -> Result<(), ConsumerError> {
        let mut deliveries = JoinSet::new();
        tokio::select! {
            biased;
            () = force.cancelled() => Err(ConsumerError::DrainTimedOut),
            result = self.consume(&mut deliveries, &stop, &force) => result,
        }
    }

    async fn consume(
        &self,
        deliveries: &mut JoinSet<()>,
        stop: &CancellationToken,
        force: &CancellationToken,
    ) -> Result<(), ConsumerError> {
        let envelope_bytes = self.delivery.shared.max_payload_bytes + HEADER_LIMIT_BYTES;
        'pulls: loop {
            while deliveries.len() == self.concurrency {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => break 'pulls,
                    joined = deliveries.join_next() => {
                        joined.transpose().map_err(|_| ConsumerError::Close)?;
                    }
                }
            }
            if stop.is_cancelled() {
                break;
            }
            // Pull requests address a durable by name, not creation identity.
            // A replacement between successful full batches has no waiting
            // request to terminate, so verify identity before every new pull.
            match durable_is_gone(&self.pull).await {
                Ok(false) => {}
                Ok(true) => return Err(ConsumerError::ConsumerLost),
                Err(_) => {
                    // Do not admit from a same-name replacement while the
                    // broker cannot confirm the durable's creation identity.
                    pull_failed();
                    tokio::time::sleep(STREAM_ERROR_BACKOFF).await;
                    continue;
                }
            }
            // Each unconsumed batch entry already owns a free slot. Completed
            // tasks can only increase capacity while this request is alive.
            let slots = self.concurrency - deliveries.len();
            let request = tokio::time::timeout(
                BROKER_OPERATION_BUDGET,
                self.pull
                    .batch()
                    .max_messages(slots)
                    .max_bytes(slots * envelope_bytes)
                    .expires(PULL_EXPIRES)
                    .messages(),
            );
            tokio::pin!(request);
            let mut messages = loop {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => break 'pulls,
                    joined = deliveries.join_next(), if !deliveries.is_empty() => {
                        joined.transpose().map_err(|_| ConsumerError::Close)?;
                    }
                    result = &mut request => if let Ok(Ok(messages)) = result {
                        break messages;
                    } else {
                        pull_failed();
                        if matches!(durable_is_gone(&self.pull).await, Ok(true)) {
                            return Err(ConsumerError::ConsumerLost);
                        }
                        tokio::time::sleep(STREAM_ERROR_BACKOFF).await;
                        continue 'pulls;
                    }
                }
            };
            // Batch has no missing-heartbeat watchdog. Bound unanswered pulls
            // here, after the broker's expiry answer is due and before the
            // SDK's own timer would end the batch as if it had answered.
            let expires = Instant::now() + PULL_EXPIRES + PULL_EXPIRY_GRACE;
            let mut remaining = slots;
            let mut failed = false;
            while remaining > 0 {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => {
                        release(messages).await;
                        break 'pulls;
                    }
                    joined = deliveries.join_next(), if !deliveries.is_empty() => {
                        joined.transpose().map_err(|_| ConsumerError::Close)?;
                    }
                    next = tokio::time::timeout_at(expires, messages.next()) => match next {
                        Ok(Some(Ok(message))) => {
                            remaining -= 1;
                            let delivery = Arc::clone(&self.delivery);
                            let cancel = force.child_token();
                            deliveries.spawn(async move { delivery.handle(message, cancel).await; });
                        }
                        Ok(None) => break,
                        Ok(Some(Err(_))) | Err(_) => {
                            failed = true;
                            break;
                        }
                    }
                }
            }
            if remaining > 0 {
                release(messages).await;
                if matches!(durable_is_gone(&self.pull).await, Ok(true)) {
                    return Err(ConsumerError::ConsumerLost);
                }
                if failed {
                    pull_failed();
                    tokio::time::sleep(STREAM_ERROR_BACKOFF).await;
                }
            }
        }
        while let Some(joined) = deliveries.join_next().await {
            joined.map_err(|_| ConsumerError::Close)?;
        }
        Ok(())
    }
}

/// Whether the broker answers that the durable, or its stream, no longer
/// exists, or was replaced with another creation or a push consumer.
/// A failed or unanswered lookup is not that answer.
async fn durable_is_gone(pull: &PullConsumer) -> Result<bool, ConsumerInfoError> {
    match pull.get_info().await {
        Ok(info) => {
            Ok(info.created != pull.cached_info().created || info.config.deliver_subject.is_some())
        }
        Err(error)
            if matches!(
                error.kind(),
                ConsumerInfoErrorKind::NotFound | ConsumerInfoErrorKind::StreamNotFound
            ) =>
        {
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

fn pull_failed() {
    // Batch errors may quote broker-controlled descriptions.
    tracing::warn!("messaging pull batch failed");
    metrics::counter!("messaging_consumer_stream_errors_total").increment(1);
}

/// Returns the deliveries the client buffered but no handler admitted.
/// Without this the broker holds each one until ack wait.
async fn release(mut messages: pull::Batch) {
    let mut prefetched = Vec::new();
    while let Some(Some(next)) = messages.next().now_or_never() {
        prefetched.extend(next);
    }
    // Ends this replica's pulls before the redelivery requests go out.
    drop(messages);
    for message in &prefetched {
        redeliver_after(message, RELEASE_DELAY).await;
    }
}

impl Delivery {
    /// Runs the handler for one delivery and settles it. Failures stay with
    /// this delivery: they are logged, counted and retried by the broker.
    async fn handle(&self, message: Message, cancel: CancellationToken) {
        let Some(delivered) = message
            .info()
            .ok()
            .and_then(|info| usize::try_from(info.delivered).ok())
        else {
            tracing::warn!("messaging delivery has no JetStream metadata");
            return;
        };
        let empty = HeaderMap::new();
        let headers = message.headers.as_ref().unwrap_or(&empty);
        let envelope = if message.payload.len() > self.shared.max_payload_bytes {
            Err(MessagingError::Bounds)
        } else {
            wire::decode_envelope(message.subject.as_ref(), headers, message.payload.clone())
        };
        let Ok(envelope) = envelope else {
            return self
                .dead_letter(&message, "malformed", UNREGISTERED, &cancel)
                .await;
        };
        let metrics = self.metrics.get(envelope.event_type());
        if delivered > MAX_DELIVERIES {
            return self
                .dead_letter(&message, "exhausted", metrics.event_type, &cancel)
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
        self.process(&message, envelope, metrics, delivered, &cancel)
            .instrument(span)
            .await;
    }

    /// Runs the typed handler inside the delivery span and settles the source.
    async fn process(
        &self,
        message: &Message,
        envelope: wire::InboundEnvelope,
        metrics: &EventMetrics,
        delivered: usize,
        cancel: &CancellationToken,
    ) {
        let started = Instant::now();
        let outcome = run_handler(&self.registry, message.subject.as_ref(), envelope, cancel).await;
        metrics.outcomes.record(outcome as usize, started.elapsed());
        let event_type = metrics.event_type;
        let label = OUTCOME_LABELS[outcome as usize];
        let span = tracing::Span::current();
        span.record("outcome", label);
        if !matches!(outcome, Outcome::Success) {
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
                attempt = delivered,
                outcome = label,
                "messaging_delivery_failed"
            ),
            _ => tracing::warn!(
                subject = message.subject.as_str(),
                event_type,
                attempt = delivered,
                outcome = label,
                "messaging_delivery_failed"
            ),
        }

        match outcome {
            Outcome::Success => acknowledge(&self.shared.client, message).await,
            // The dead-letter reason is the Go wire vocabulary, which has one
            // word for every delivery that a retry cannot help.
            Outcome::Permanent | Outcome::Unhandled | Outcome::Undecodable => {
                self.dead_letter(message, "permanent", event_type, cancel)
                    .await;
            }
            _ if delivered >= MAX_DELIVERIES => {
                self.dead_letter(message, "exhausted", event_type, cancel)
                    .await;
            }
            _ => {
                let delay = RETRY_DELAYS
                    .get(delivered.saturating_sub(1))
                    .copied()
                    .unwrap_or(SETTLEMENT_RETRY_DELAY);
                redeliver_after(message, delay).await;
            }
        }
    }

    /// Copies the source to the dead-letter stream, then acknowledges it.
    /// A failed transfer keeps the source for a later redelivery.
    async fn dead_letter(
        &self,
        source: &Message,
        reason: &'static str,
        event_type: &'static str,
        cancel: &CancellationToken,
    ) {
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
        });
        let Some(transfer_id) = transfer_id else {
            tracing::error!(reason, "messaging dead-letter identity is unavailable");
            return redeliver_after(source, SETTLEMENT_RETRY_DELAY).await;
        };
        let mut headers = HeaderMap::new();
        for name in [
            wire::name::MESSAGE_ID,
            wire::name::EVENT_TYPE,
            wire::name::EVENT_SCHEMA,
            wire::name::CREATED_AT,
            crate::trace::TRACEPARENT,
            crate::trace::TRACESTATE,
        ] {
            let value = wire::header_value(original, name.clone());
            if !value.is_empty() {
                headers.insert(name, value);
            }
        }
        if wire::header_value(&headers, wire::name::MESSAGE_ID).is_empty() {
            headers.insert(wire::name::MESSAGE_ID, transfer_id.as_str());
        }
        headers.insert(wire::name::NATS_MSG_ID, transfer_id.as_str());
        headers.insert(wire::name::ORIGINAL_SUBJECT, source.subject.as_str());
        headers.insert(wire::name::DEAD_LETTER_REASON, reason);

        let result = publish(
            &self.shared,
            &self.dlq_subject,
            headers,
            source.payload.clone(),
            &self.dlq_stream,
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
                event_type,
                reason,
                outcome,
                "messaging dead-letter transfer failed"
            );
            redeliver_after(source, SETTLEMENT_RETRY_DELAY).await;
        }
    }
}

/// Runs the typed handler of one delivery under the handler time limit.
///
/// The handler's token is cancelled when the invocation ends, by return,
/// time limit or panic, so work the handler started with it stops too.
async fn run_handler(
    registry: &Registry,
    subject: &str,
    envelope: wire::InboundEnvelope,
    cancel: &CancellationToken,
) -> Outcome {
    let handler_cancel = cancel.child_token();
    let _ended = handler_cancel.clone().drop_guard();
    let dispatch = registry.dispatch(subject, envelope, handler_cancel);
    match tokio::time::timeout(HANDLER_TIMEOUT, AssertUnwindSafe(dispatch).catch_unwind()).await {
        Ok(Ok(Ok(()))) => Outcome::Success,
        Ok(Ok(Err(DispatchError::Handler(HandlerError::Permanent)))) => Outcome::Permanent,
        Ok(Ok(Err(DispatchError::Handler(HandlerError::Retryable)))) => Outcome::Retryable,
        Ok(Ok(Err(DispatchError::Unhandled))) => Outcome::Unhandled,
        Ok(Ok(Err(DispatchError::Undecodable))) => Outcome::Undecodable,
        Ok(Err(_)) => Outcome::Panicked,
        Err(_) => Outcome::TimedOut,
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
        tracing::warn!("messaging source acknowledgement is unconfirmed");
        metrics::counter!("messaging_settlement_failures_total", "operation" => "ack").increment(1);
    }
}

/// Asks the broker to redeliver the source after `delay`; if that request is
/// lost, ack wait redelivers it anyway.
async fn redeliver_after(source: &Message, delay: Duration) {
    let requested = tokio::time::timeout(
        BROKER_OPERATION_BUDGET,
        source.ack_with(AckKind::Nak(Some(delay))),
    )
    .await;
    if !matches!(requested, Ok(Ok(()))) {
        tracing::warn!("messaging delayed redelivery request failed");
        metrics::counter!("messaging_settlement_failures_total", "operation" => "nak").increment(1);
    }
}

impl ConsumerHandle {
    /// Stops new pulls while admitted work keeps its handler/settlement budget.
    pub fn drain(&self) {
        self.stop.cancel();
    }

    /// Resolves when a terminal consumer failure occurs.
    pub async fn failed(&self) -> ConsumerError {
        let mut failure = self.failure.clone();
        loop {
            if let Some(error) = failure.borrow().clone() {
                return error;
            }
            if failure.changed().await.is_err() {
                return ConsumerError::Close;
            }
        }
    }

    /// Cancels unfinished work; the supervisor aborts every delivery.
    pub fn abort(&self) {
        self.stop.cancel();
        self.force.cancel();
    }

    /// Cancellation-safe join for a root racing drain against another signal.
    /// The handle remains owned here when this future is dropped or times out,
    /// allowing the root to abort and retry within its shared cleanup deadline.
    ///
    /// # Errors
    /// Returns the terminal consumer fault or a forced-drain outcome.
    pub async fn finish(&mut self, deadline: Instant) -> Result<(), ConsumerError> {
        self.drain();
        if let Some(result) = &self.completed {
            return result.clone();
        }
        let Some(task) = self.task.as_mut() else {
            return Err(ConsumerError::Close);
        };
        let result = match tokio::time::timeout_at(deadline, &mut *task).await {
            Ok(result) => result.unwrap_or(Err(ConsumerError::Close)),
            Err(_) => return Err(ConsumerError::DrainTimedOut),
        };
        self.task.take();
        self.completed = Some(result.clone());
        result
    }
}

impl Drop for ConsumerHandle {
    fn drop(&mut self) {
        self.abort();
        if let Some(task) = &self.task {
            // Exhausted cleanup has no budget left to observe a join. Aborting
            // the supervisor drops its pipeline, which aborts every delivery;
            // runtime shutdown remains the final resource owner.
            task.abort();
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test controls the task and all completion signals"
)]
mod tests {
    use domain_events::EventPayload;

    use super::*;
    use crate::registry::Route;

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
            let mut registry = Registry::new([
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
                .record(Outcome::Retryable as usize, Duration::from_millis(5));
            // A routed type without a handler and a type no route knows.
            for unhandled in ["order.shipped", "sentinel.publisher.chosen"] {
                let metrics = metrics.get(unhandled);
                assert_eq!(metrics.event_type, UNREGISTERED);
                metrics
                    .outcomes
                    .record(Outcome::Permanent as usize, Duration::from_millis(5));
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

    /// A handler hands its token to work it starts. That work must hear when
    /// the delivery is over, or it outlives a handler the adapter gave up on.
    #[tokio::test(start_paused = true)]
    #[allow(
        clippy::excessive_nesting,
        reason = "the two-outcome cancellation oracle keeps token extraction beside the returned async handler"
    )]
    async fn the_handler_token_is_cancelled_when_its_delivery_ends() {
        for (stalls, expected) in [(true, "timeout"), (false, "success")] {
            let (token_tx, token_rx) = tokio::sync::oneshot::channel();
            let token_tx = std::sync::Mutex::new(Some(token_tx));
            let mut registry = Registry::new([Route::new::<Created>("orders.created")]).unwrap();
            registry
                .register::<Created, _, _>(move |_, cancel| {
                    let token_tx = token_tx.lock().unwrap().take();
                    async move {
                        token_tx.unwrap().send(cancel).unwrap();
                        if stalls {
                            std::future::pending::<()>().await;
                        }
                        Ok(())
                    }
                })
                .unwrap();

            let delivery = CancellationToken::new();
            let outcome =
                run_handler(&registry, "orders.created", created_envelope(), &delivery).await;

            assert_eq!(OUTCOME_LABELS[outcome as usize], expected);
            let handler_token = token_rx.await.unwrap();
            assert!(handler_token.is_cancelled(), "{expected}");
            assert!(!delivery.is_cancelled(), "{expected}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn expired_finish_retains_the_task_without_spending_cleanup_time() {
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let (_failure_sender, failure) = watch::channel(None);
        let mut handle = ConsumerHandle {
            stop: CancellationToken::new(),
            force: CancellationToken::new(),
            failure,
            task: Some(tokio::spawn(async {
                let _ = released.await;
                Ok(())
            })),
            completed: None,
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        // The task deliberately cannot finish during drain. An unbounded join
        // in the timeout branch consumes this independent caller deadline.
        let result =
            tokio::time::timeout_at(deadline + Duration::from_millis(1), handle.finish(deadline))
                .await;
        let timed_out_within_budget = matches!(result, Ok(Err(ConsumerError::DrainTimedOut)));
        handle.abort();
        release.send(()).unwrap();
        let cleanup = handle.finish(Instant::now() + Duration::from_secs(1)).await;
        assert!(timed_out_within_budget);
        assert!(
            cleanup.is_ok(),
            "the original task remains joinable after timeout"
        );
    }
}
