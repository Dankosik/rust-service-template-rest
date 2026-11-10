use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use async_nats::HeaderMap;
use async_nats::jetstream::consumer::pull;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, PullConsumer, ReplayPolicy};
use async_nats::jetstream::context::{ConsumerInfoError, ConsumerInfoErrorKind};
use async_nats::jetstream::stream::ConsumerErrorKind;
use async_nats::jetstream::{AckKind, Message};
use futures_util::{FutureExt as _, StreamExt as _};
use operation_context::{Deadline, OperationContext, Stopped};
use tokio::sync::watch;
use tokio::task::{JoinError, JoinHandle, JoinSet};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use crate::error::{HandlerError, MessagingError};
use crate::messaging::{
    BROKER_OPERATION_BUDGET, ConsumerAdmission, OutcomeLabel, Outcomes, Shared, bounded_operation,
    get_stream_failure, outcome_label, topology_failure,
};
use crate::producer::send_publication;
use crate::registry::{DispatchError, Registry};
use crate::wire;

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
    concurrency: NonZeroUsize,
    pull_delivery_bytes: usize,
    delivery: Arc<Delivery>,
}

/// Terminal worker failure or incomplete shutdown. Reasons never contain input.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ConsumerError {
    #[error("messaging durable consumer was deleted or replaced")]
    ConsumerLost,
    #[error("messaging consumer drain exceeded its shared deadline")]
    DrainTimedOut,
    /// The consumer task or one of its deliveries panicked or was cancelled,
    /// or, as seen by [`ConsumerHandle::failed`], the task stopped without
    /// reporting a failure.
    #[error("messaging consumer task exited unexpectedly")]
    TaskEnded,
}

/// A running consumer owned and joined by the composition root.
#[derive(Debug)]
pub struct ConsumerHandle {
    stop: CancellationToken,
    force: CancellationToken,
    failure: watch::Receiver<Option<ConsumerError>>,
    supervisor: Supervisor,
}

/// The consumer task, or its result once it was joined.
#[derive(Debug)]
enum Supervisor {
    Running(JoinHandle<Result<(), ConsumerError>>),
    Finished(Result<(), ConsumerError>),
}

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

impl Consumer {
    pub(crate) async fn admit(
        shared: Arc<Shared>,
        admission: ConsumerAdmission,
        registry: Registry,
    ) -> Result<Self, MessagingError> {
        let ConsumerAdmission {
            options,
            dead_letter,
        } = admission;
        // A route without a handler is publish-only and need not be consumed.
        if registry
            .handled()
            .any(|route| !wire::matches_filter(&options.filter_subject, &route.subject))
        {
            return Err(MessagingError::Configuration(
                "handled subject is outside the consumer filter",
            ));
        }
        let server = shared.client.server_info();
        let subject_bytes =
            dead_letter.admit_routes(&registry, &options.filter_subject, server.max_payload)?;
        let pull_delivery_bytes = wire::pull_delivery_bytes(
            shared.max_payload_bytes,
            subject_bytes,
            shared.source_stream.len(),
            options.durable_name.len(),
            server.domain.as_ref().map_or(0, String::len),
        )
        .ok_or(MessagingError::Bounds)?;
        // `consume` multiplies a full pull's budget unchecked, and the broker
        // reads its byte limit as an i64.
        pull_delivery_bytes
            .checked_mul(options.concurrency.get())
            .and_then(|bytes| i64::try_from(bytes).ok())
            .ok_or(MessagingError::Bounds)?;
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
            // The adapter, not the broker, ends redelivery: it dead-letters
            // an exhausted delivery, where a broker limit would drop it.
            max_deliver: -1,
            replay_policy: ReplayPolicy::Instant,
            ..Default::default()
        };
        let pull = bounded_operation(shared.startup_deadline, &shared.startup_cancel, async {
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
        })
        .await?;
        Ok(Self {
            pull,
            concurrency: options.concurrency,
            pull_delivery_bytes,
            delivery: Arc::new(Delivery {
                shared,
                metrics: HandlerMetrics::register(&registry),
                registry,
                span_name: format!("process {}", options.filter_subject),
                filter_subject: options.filter_subject,
                dlq_subject: options.dlq_subject,
                dlq_stream: dead_letter.stream,
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
                // The owner's forced cut ends this consumer only; the shared
                // connection still serves producers, other consumers and
                // readiness.
                if !matches!(error, ConsumerError::DrainTimedOut) {
                    shared.mark_failed();
                    tracing::error!(reason = %error, "messaging consumer stopped");
                }
                let _ = failure_tx.send(Some(error.clone()));
            }
            result
        });
        ConsumerHandle {
            stop,
            force,
            failure,
            supervisor: Supervisor::Running(task),
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
        let concurrency = self.concurrency.get();
        'pulls: loop {
            while deliveries.len() == concurrency {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => break 'pulls,
                    joined = deliveries.join_next() => reaped(joined)?,
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
                Err(error) => {
                    // Do not admit from a same-name replacement while the
                    // broker cannot confirm the durable's creation identity.
                    pull_failed("consumer_info", consumer_info_error_kind(&error.kind()));
                    tokio::time::sleep(STREAM_ERROR_BACKOFF).await;
                    continue;
                }
            }
            // Each unconsumed batch entry already owns a free slot. Completed
            // tasks can only increase capacity while this request is alive.
            let slots = concurrency - deliveries.len();
            let request = tokio::time::timeout(
                BROKER_OPERATION_BUDGET,
                self.pull
                    .batch()
                    .max_messages(slots)
                    .max_bytes(slots * self.pull_delivery_bytes)
                    .expires(PULL_EXPIRES)
                    .messages(),
            );
            tokio::pin!(request);
            let mut messages = loop {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => break 'pulls,
                    joined = deliveries.join_next(), if !deliveries.is_empty() => reaped(joined)?,
                    result = &mut request => {
                        let error_kind = match result {
                            Ok(Ok(messages)) => break messages,
                            Ok(Err(error)) => batch_create_error_kind(error.kind()),
                            Err(_) => "local_timeout",
                        };
                        pull_failed("batch_create", error_kind);
                        if durable_lost(&self.pull).await {
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
            let failure = loop {
                if remaining == 0 {
                    break None;
                }
                tokio::select! {
                    biased;
                    () = stop.cancelled() => {
                        release(messages).await;
                        break 'pulls;
                    }
                    joined = deliveries.join_next(), if !deliveries.is_empty() => reaped(joined)?,
                    next = tokio::time::timeout_at(expires, messages.next()) => match next {
                        Ok(Some(Ok(message))) => {
                            remaining -= 1;
                            let delivery = Arc::clone(&self.delivery);
                            let cancel = force.child_token();
                            deliveries.spawn(async move { delivery.handle(message, cancel).await; });
                        }
                        Ok(None) => break None,
                        Ok(Some(Err(error))) => break Some(batch_receive_error_kind(&error)),
                        Err(_) => break Some("local_timeout"),
                    }
                }
            };
            if remaining > 0 {
                release(messages).await;
                if durable_lost(&self.pull).await {
                    return Err(ConsumerError::ConsumerLost);
                }
                if let Some(error_kind) = failure {
                    pull_failed("batch_receive", error_kind);
                    tokio::time::sleep(STREAM_ERROR_BACKOFF).await;
                }
            }
        }
        while let Some(joined) = deliveries.join_next().await {
            reaped(Some(joined))?;
        }
        Ok(())
    }
}

/// A delivery task that panicked or was cancelled ends the consumer.
fn reaped(joined: Option<Result<(), JoinError>>) -> Result<(), ConsumerError> {
    joined
        .transpose()
        .map(|_| ())
        .map_err(|_| ConsumerError::TaskEnded)
}

/// Whether the broker confirmed that the durable is gone; an unanswered
/// lookup is not that confirmation.
async fn durable_lost(pull: &PullConsumer) -> bool {
    matches!(durable_is_gone(pull).await, Ok(true))
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

fn consumer_info_error_kind(kind: &ConsumerInfoErrorKind) -> &'static str {
    match kind {
        ConsumerInfoErrorKind::InvalidName => "invalid_name",
        ConsumerInfoErrorKind::Offline => "offline",
        ConsumerInfoErrorKind::NotFound => "not_found",
        ConsumerInfoErrorKind::StreamNotFound => "stream_not_found",
        ConsumerInfoErrorKind::Request => "request",
        ConsumerInfoErrorKind::JetStream(_) => "jetstream",
        ConsumerInfoErrorKind::TimedOut => "timeout",
        ConsumerInfoErrorKind::NoResponders => "no_responders",
    }
}

fn batch_create_error_kind(kind: pull::BatchErrorKind) -> &'static str {
    match kind {
        pull::BatchErrorKind::Subscribe => "subscribe",
        pull::BatchErrorKind::Pull => "pull",
        pull::BatchErrorKind::Flush => "flush",
        pull::BatchErrorKind::Serialize => "serialize",
    }
}

fn batch_receive_error_kind(error: &async_nats::Error) -> &'static str {
    let Some(error) = error.downcast_ref::<std::io::Error>() else {
        return "other";
    };
    if error.kind() != std::io::ErrorKind::Other {
        return "io_error";
    }
    // async-nats 0.50 Batch erases status/description into io::Error(Other).
    // Recognize only its pinned format for diagnostics; provider text never
    // becomes a field or changes pull/retry behavior.
    let description = error.to_string();
    let Some(status) =
        description.strip_prefix("error while processing messages from the stream: ")
    else {
        return "io_error";
    };
    match status {
        "409, Some(\"Batch Completed\")" => "batch_completed",
        "409, Some(\"Message Size Exceeds MaxBytes\")" => "max_bytes_exceeded",
        _ => match status.split_once(", ").map(|(code, _)| code) {
            Some("400") => "status_400",
            Some("403") => "status_403",
            Some("404") => "status_404",
            Some("408") => "status_408",
            Some("409") => "status_409",
            Some("503") => "status_503",
            _ => "status_other",
        },
    }
}

fn pull_failed(phase: &'static str, error_kind: &'static str) {
    tracing::warn!(phase, error_kind, "messaging_pull_failed");
    metrics::counter!("messaging_consumer_stream_errors_total").increment(1);
}

/// Returns the deliveries the client buffered but no handler admitted.
/// An item the client cannot produce is left unacknowledged for ack-wait
/// redelivery; the drain must not wait for the broker.
async fn release(mut messages: pull::Batch) {
    let mut prefetched = Vec::new();
    while let Some(Some(item)) = messages.next().now_or_never() {
        if let Ok(message) = item {
            prefetched.push(message);
        }
    }
    // Ends this replica's pulls before the redelivery requests go out.
    drop(messages);
    for message in &prefetched {
        redeliver_after(message, RELEASE_DELAY).await;
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

impl Delivery {
    /// Runs the handler for one delivery and settles it when the delivery
    /// carries `JetStream` metadata. A handler failure stays with the
    /// delivery: it is logged, counted and retried by the broker. Without
    /// metadata the attempt count and deterministic DLQ identity are
    /// unavailable, so the delivery is logged and left unacknowledged for
    /// ack-wait redelivery.
    async fn handle(&self, message: Message, cancel: CancellationToken) {
        let Some(delivery_attempt) = message
            .info()
            .ok()
            .and_then(|info| usize::try_from(info.delivered).ok())
        else {
            // The attempt count and deterministic DLQ identity both need
            // delivery metadata; leave the delivery for ack-wait redelivery.
            tracing::warn!("messaging delivery has no JetStream metadata");
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
                reason = reason.as_str(),
                "messaging dead-letter identity is unavailable"
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

    /// Waits for the consumer's terminal failure. Also returns
    /// [`ConsumerError::TaskEnded`] once the task stopped without one, such
    /// as after a clean drain.
    pub async fn failed(&self) -> ConsumerError {
        let mut failure = self.failure.clone();
        match failure.wait_for(Option::is_some).await {
            Ok(error) => error.clone().unwrap_or(ConsumerError::TaskEnded),
            Err(_) => ConsumerError::TaskEnded,
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
        let task = match &mut self.supervisor {
            Supervisor::Finished(result) => return result.clone(),
            Supervisor::Running(task) => task,
        };
        let result = match tokio::time::timeout_at(deadline, &mut *task).await {
            Ok(result) => result.unwrap_or(Err(ConsumerError::TaskEnded)),
            Err(_) => return Err(ConsumerError::DrainTimedOut),
        };
        self.supervisor = Supervisor::Finished(result.clone());
        result
    }
}

impl Drop for ConsumerHandle {
    fn drop(&mut self) {
        self.abort();
        if let Supervisor::Running(task) = &self.supervisor {
            // Exhausted cleanup has no budget left to observe a join. Aborting
            // the supervisor drops its pipeline, which aborts every delivery;
            // runtime shutdown remains the final resource owner.
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use domain_events::EventPayload;

    use super::*;
    use crate::registry::Route;

    #[test]
    fn handler_outcomes_are_listed_in_index_order() {
        crate::messaging::tests::assert_all_in_index_order::<Outcome>();
    }

    #[test]
    fn batch_receipt_diagnostics_classify_pinned_statuses_without_provider_text() {
        for (description, expected) in [
            (
                "error while processing messages from the stream: 409, Some(\"Batch Completed\")",
                "batch_completed",
            ),
            (
                "error while processing messages from the stream: 409, Some(\"Message Size Exceeds MaxBytes\")",
                "max_bytes_exceeded",
            ),
            (
                "error while processing messages from the stream: 409, Some(\"Batch Completed sentinel.provider.chosen\")",
                "status_409",
            ),
            (
                "error while processing messages from the stream: 503, Some(\"sentinel.provider.chosen\")",
                "status_503",
            ),
            (
                "error while processing messages from the stream: 500, Some(\"sentinel.provider.chosen\")",
                "status_other",
            ),
            ("sentinel.provider.chosen", "io_error"),
        ] {
            let error: async_nats::Error = Box::new(std::io::Error::other(description));
            assert_eq!(batch_receive_error_kind(&error), expected);
        }
        let error: async_nats::Error = Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "error while processing messages from the stream: 409, Some(\"Batch Completed\")",
        ));
        assert_eq!(batch_receive_error_kind(&error), "io_error");
        let error: async_nats::Error = Box::new(std::fmt::Error);
        assert_eq!(batch_receive_error_kind(&error), "other");
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

    #[tokio::test(start_paused = true)]
    async fn expired_finish_retains_the_task_without_spending_cleanup_time() {
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let (_failure_sender, failure) = watch::channel(None);
        let mut handle = ConsumerHandle {
            stop: CancellationToken::new(),
            force: CancellationToken::new(),
            failure,
            supervisor: Supervisor::Running(tokio::spawn(async {
                let _ = released.await;
                Ok(())
            })),
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
