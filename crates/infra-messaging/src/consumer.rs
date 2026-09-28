use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_nats::jetstream::consumer::pull::MessagesErrorKind;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, PullConsumer, ReplayPolicy};
use async_nats::jetstream::{AckKind, Message};
use async_nats::{HeaderMap, HeaderName};
use futures_util::{FutureExt as _, StreamExt as _, TryStreamExt as _};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use crate::error::{HandlerError, MessagingError};
use crate::messaging::{BROKER_OPERATION_BUDGET, ConsumerOptions, Shared};
use crate::producer::publish;
use crate::registry::Registry;
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
    dlq_subject: String,
    dlq_stream: String,
}

/// How one handler invocation ended. The discriminant indexes [`OUTCOME_LABELS`].
#[derive(Clone, Copy)]
enum Outcome {
    Success = 0,
    Permanent = 1,
    Retryable = 2,
    TimedOut = 3,
    Panicked = 4,
}

/// Metric label of each [`Outcome`], indexed by its discriminant.
pub(crate) const OUTCOME_LABELS: [&str; 5] =
    ["success", "permanent", "retryable", "timeout", "panic"];

impl Consumer {
    pub(crate) async fn admit(
        shared: Arc<Shared>,
        options: ConsumerOptions,
        registry: Registry,
    ) -> Result<Self, MessagingError> {
        if registry
            .subjects()
            .any(|subject| !wire::subject_matches(&options.filter_subject, subject))
        {
            return Err(MessagingError::Configuration(
                "registered subject is outside the consumer filter",
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
                .map_err(|_| MessagingError::Topology)?;
            stream
                .create_consumer(config)
                .await
                .map_err(|_| MessagingError::Topology)
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
                registry,
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

    /// Pulls until `stop`, then lets admitted deliveries settle. `force`
    /// drops the pipeline, which aborts every in-flight delivery task.
    async fn run(
        self,
        stop: CancellationToken,
        force: CancellationToken,
    ) -> Result<(), ConsumerError> {
        let envelope_bytes = self.delivery.shared.max_payload_bytes + HEADER_LIMIT_BYTES;
        let messages = self
            .pull
            .stream()
            .max_messages_per_batch(self.concurrency)
            .max_bytes_per_batch(self.concurrency * envelope_bytes)
            .messages()
            .await
            .map_err(|_| ConsumerError::Start)?;
        let consume = messages
            .take_until(stop.cancelled_owned())
            .map(Ok)
            .try_for_each_concurrent(self.concurrency, |next| {
                let delivery = Arc::clone(&self.delivery);
                let cancel = force.child_token();
                async move {
                    match next {
                        Ok(message) => {
                            AbortOnDropHandle::new(tokio::spawn(async move {
                                delivery.handle(message, cancel).await;
                            }))
                            .await
                            .map_err(|_| ConsumerError::Close)
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                MessagesErrorKind::ConsumerDeleted
                                    | MessagesErrorKind::PushBasedConsumer
                            ) =>
                        {
                            Err(ConsumerError::ConsumerLost)
                        }
                        Err(error) => {
                            tracing::warn!(error.kind = %error.kind(), "messaging pull stream error");
                            metrics::counter!("messaging_consumer_stream_errors_total")
                                .increment(1);
                            tokio::time::sleep(STREAM_ERROR_BACKOFF).await;
                            Ok(())
                        }
                    }
                }
            });
        tokio::select! {
            biased;
            () = force.cancelled() => Err(ConsumerError::DrainTimedOut),
            result = consume => result,
        }
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
            return self.dead_letter(&message, "malformed", &cancel).await;
        };
        if delivered > MAX_DELIVERIES {
            return self.dead_letter(&message, "exhausted", &cancel).await;
        }

        let started = Instant::now();
        let dispatch =
            self.registry
                .dispatch(message.subject.as_ref(), envelope, cancel.child_token());
        let outcome =
            match tokio::time::timeout(HANDLER_TIMEOUT, AssertUnwindSafe(dispatch).catch_unwind())
                .await
            {
                Ok(Ok(Ok(()))) => Outcome::Success,
                Ok(Ok(Err(HandlerError::Permanent))) => Outcome::Permanent,
                Ok(Ok(Err(HandlerError::Retryable))) => Outcome::Retryable,
                Ok(Err(_)) => Outcome::Panicked,
                Err(_) => Outcome::TimedOut,
            };
        self.shared
            .handler_metrics
            .record(outcome as usize, started.elapsed());
        if matches!(outcome, Outcome::Panicked) {
            tracing::error!("messaging handler panicked");
        }

        match outcome {
            Outcome::Success => acknowledge(&message).await,
            Outcome::Permanent => self.dead_letter(&message, "permanent", &cancel).await,
            _ if delivered >= MAX_DELIVERIES => {
                self.dead_letter(&message, "exhausted", &cancel).await;
            }
            _ => {
                let delay = RETRY_DELAYS
                    .get(delivered.saturating_sub(1))
                    .copied()
                    .unwrap_or(SETTLEMENT_RETRY_DELAY);
                redeliver_after(&message, delay).await;
            }
        }
    }

    /// Copies the source to the dead-letter stream, then acknowledges it.
    /// A failed transfer keeps the source for a later redelivery.
    async fn dead_letter(
        &self,
        source: &Message,
        reason: &'static str,
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
            HeaderName::from_static("traceparent"),
            HeaderName::from_static("tracestate"),
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
        metrics::counter!("messaging_dead_letter_total", "reason" => reason, "outcome" => outcome)
            .increment(1);
        if result.is_ok() {
            acknowledge(source).await;
        } else {
            tracing::error!(reason, outcome, "messaging dead-letter transfer failed");
            redeliver_after(source, SETTLEMENT_RETRY_DELAY).await;
        }
    }
}

/// Confirms the source. A lost confirmation is redelivered after ack wait,
/// which idempotent handlers tolerate.
async fn acknowledge(source: &Message) {
    let confirmed = tokio::time::timeout(BROKER_OPERATION_BUDGET, source.double_ack()).await;
    if !matches!(confirmed, Ok(Ok(()))) {
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
    use super::*;

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
