use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_nats::HeaderMap;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, FromConsumer, ReplayPolicy};
use async_nats::jetstream::context::ConsumerInfoErrorKind;
use async_nats::jetstream::{AckKind, Message};
use futures_util::StreamExt;
use tokio::sync::watch;
use tokio::task::{JoinError, JoinHandle, JoinSet};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::error::{HandlerError, MessagingError, PublishError};
use crate::messaging::{BROKER_OPERATION_BUDGET, ConsumerOptions, Shared};
use crate::producer::publish_raw;
use crate::registry::Registry;
use crate::wire::{self, HEADER_LIMIT_BYTES};

const HANDLER_TIMEOUT: Duration = Duration::from_secs(30);
const UNCERTAIN_REDELIVERY: Duration = Duration::from_secs(30);
const RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
];

/// A bounded pull consumer admitted against an existing source stream.
#[derive(Debug)]
pub struct Consumer {
    shared: Arc<Shared>,
    options: ConsumerOptions,
    registry: Registry,
    pull: async_nats::jetstream::consumer::PullConsumer,
}

/// Terminal worker failure or incomplete shutdown. Reasons never contain input.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ConsumerError {
    #[error("messaging consumer could not read source delivery")]
    Terminal,
    #[error("messaging source exceeds the admitted delivery bound")]
    SourceOversized,
    #[error("messaging source cannot fit the dead-letter envelope")]
    DeadLetterOversized,
    #[error("messaging dead-letter publication was refused")]
    DeadLetterRejected,
    #[error("messaging delayed source redelivery could not be requested")]
    RedeliveryFailed,
    #[error("messaging handler panicked")]
    HandlerPanicked,
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

type Deliveries = JoinSet<Result<(), ConsumerError>>;

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
        let max_ack_pending =
            i64::try_from(options.concurrency).map_err(|_| MessagingError::Bounds)?;
        let deadline = shared
            .startup_deadline
            .min(Instant::now() + BROKER_OPERATION_BUDGET);
        let admission = async {
            let stream = shared
                .jetstream
                .get_stream(&shared.source_stream)
                .await
                .map_err(|_| MessagingError::Topology)?;
            let mut config = match stream.consumer_info(&options.durable_name).await {
                Ok(info) => {
                    async_nats::jetstream::consumer::pull::Config::try_from_consumer_config(
                        info.config,
                    )
                    .map_err(|_| MessagingError::Topology)?
                }
                Err(error) if error.kind() == ConsumerInfoErrorKind::NotFound => {
                    async_nats::jetstream::consumer::pull::Config::default()
                }
                Err(_) => return Err(MessagingError::Topology),
            };
            if config.headers_only || !config.backoff.is_empty() {
                return Err(MessagingError::Topology);
            }
            config.name = Some(options.durable_name.clone());
            config.durable_name = Some(options.durable_name.clone());
            config.deliver_policy = DeliverPolicy::All;
            config.ack_policy = AckPolicy::Explicit;
            config.ack_wait = Duration::from_secs(41);
            config.max_deliver = -1;
            config.replay_policy = ReplayPolicy::Instant;
            config.filter_subject.clone_from(&options.filter_subject);
            config.max_ack_pending = max_ack_pending;
            // Create-or-update only this named consumer. An incompatible cursor
            // is refused by the broker; it is never deleted and recreated.
            stream
                .create_consumer(config)
                .await
                .map_err(|_| MessagingError::Topology)?;
            let pull: async_nats::jetstream::consumer::PullConsumer = stream
                .get_consumer(&options.durable_name)
                .await
                .map_err(|_| MessagingError::Topology)?;
            let actual = &pull.cached_info().config;
            if actual.durable_name.as_deref() != Some(options.durable_name.as_str())
                || actual.name.as_deref() != Some(options.durable_name.as_str())
                || actual.deliver_policy != DeliverPolicy::All
                || actual.ack_policy != AckPolicy::Explicit
                || actual.ack_wait != Duration::from_secs(41)
                || actual.max_deliver != -1
                || actual.replay_policy != ReplayPolicy::Instant
                || actual.filter_subject != options.filter_subject
                || actual.max_ack_pending != max_ack_pending
                || !actual.backoff.is_empty()
                || actual.headers_only
            {
                return Err(MessagingError::Topology);
            }
            Ok(pull)
        };
        let pull = tokio::select! {
            biased;
            () = shared.startup_cancel.cancelled() => return Err(MessagingError::Cancelled),
            result = tokio::time::timeout_at(deadline, admission) => result
                .map_err(|_| MessagingError::TimedOut { budget: BROKER_OPERATION_BUDGET })??,
        };
        Ok(Self {
            shared,
            options,
            registry,
            pull,
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
            let shared = Arc::clone(&self.shared);
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

    async fn run(
        self,
        stop: CancellationToken,
        force: CancellationToken,
    ) -> Result<(), ConsumerError> {
        let mut tasks = JoinSet::new();
        let mut result = self.pull_until_stopped(&stop, &force, &mut tasks).await;
        if result.is_ok() {
            while !tasks.is_empty() {
                let next = tokio::select! {
                    biased;
                    () = force.cancelled() => Err(ConsumerError::DrainTimedOut),
                    next = tasks.join_next() => joined(next),
                };
                if let Err(error) = next {
                    result = Err(error);
                    break;
                }
            }
        }
        if result.is_err() {
            // Cancellation propagates before abort; joining observes every
            // task's destruction and retains no detached handler/settlement.
            self.shared.failed.store(true, Ordering::Release);
            force.cancel();
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
        result
    }

    async fn pull_until_stopped(
        &self,
        stop: &CancellationToken,
        force: &CancellationToken,
        tasks: &mut Deliveries,
    ) -> Result<(), ConsumerError> {
        loop {
            if stop.is_cancelled() || self.shared.draining.load(Ordering::Acquire) {
                return Ok(());
            }
            while let Some(result) = tasks.try_join_next() {
                joined(Some(result))?;
            }
            let free = self.options.concurrency - tasks.len();
            if free == 0 {
                tokio::select! {
                    biased;
                    () = force.cancelled() => return Err(ConsumerError::DrainTimedOut),
                    () = stop.cancelled() => return Ok(()),
                    next = tasks.join_next() => joined(next)?,
                }
                continue;
            }
            // Reserving `free` slots also reserves every raw message that the
            // finite batch can buffer. No replenishing stream or extra queue.
            let fetch = self
                .pull
                .batch()
                .max_messages(free)
                .max_bytes(free * (self.shared.max_payload_bytes + HEADER_LIMIT_BYTES))
                .expires(Duration::from_secs(1))
                .messages();
            let fetch = tokio::time::timeout(BROKER_OPERATION_BUDGET, fetch);
            tokio::pin!(fetch);
            let mut batch = loop {
                tokio::select! {
                    biased;
                    () = force.cancelled() => return Err(ConsumerError::DrainTimedOut),
                    () = stop.cancelled() => return Ok(()),
                    next = tasks.join_next(), if !tasks.is_empty() => joined(next)?,
                    result = &mut fetch => break result.map_err(|_| ConsumerError::Terminal)?
                        .map_err(|_| ConsumerError::Terminal)?,
                }
            };
            loop {
                tokio::select! {
                    biased;
                    () = force.cancelled() => return Err(ConsumerError::DrainTimedOut),
                    () = stop.cancelled() => return Ok(()),
                    next = tasks.join_next(), if !tasks.is_empty() => joined(next)?,
                    next = batch.next() => {
                        let Some(next) = next else { break; };
                        // Includes broker refusal when a retained record cannot
                        // fit the bounded pull: source remains unacknowledged.
                        let message = next.map_err(|_| ConsumerError::Terminal)?;
                        let shared = Arc::clone(&self.shared);
                        let registry = self.registry.clone();
                        let options = self.options.clone();
                        let cancel = force.child_token();
                        tasks.spawn(async move {
                            handle_delivery(&shared, &options, &registry, message, &cancel).await
                        });
                    }
                }
            }
        }
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

    /// Drains and joins until the supplied deadline.
    /// On timeout, dropping this owner requests abort without claiming a join.
    ///
    /// # Errors
    /// Returns a terminal consumer fault or a forced-drain outcome.
    pub async fn join(mut self, deadline: Instant) -> Result<(), ConsumerError> {
        self.finish(deadline).await
    }

    /// Cancels unfinished work; the supervisor aborts and joins every delivery.
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
            // the supervisor drops its JoinSet, which aborts every delivery;
            // runtime shutdown remains the final resource owner.
            task.abort();
        }
    }
}

fn joined(
    result: Option<Result<Result<(), ConsumerError>, JoinError>>,
) -> Result<(), ConsumerError> {
    match result {
        Some(Ok(result)) => result,
        Some(Err(error)) if error.is_panic() => Err(ConsumerError::HandlerPanicked),
        Some(Err(_)) | None => Err(ConsumerError::Close),
    }
}

async fn handle_delivery(
    shared: &Shared,
    options: &ConsumerOptions,
    registry: &Registry,
    source: Message,
    cancel: &CancellationToken,
) -> Result<(), ConsumerError> {
    let info = source.info().map_err(|_| ConsumerError::Terminal)?;
    if info.delivered < 1 {
        return Err(ConsumerError::Terminal);
    }
    let empty = HeaderMap::new();
    let headers = source.headers.as_ref().unwrap_or(&empty);
    let wire_size = source
        .subject
        .len()
        .saturating_add(wire::encoded_header_bytes(headers))
        .saturating_add(source.payload.len());
    if source.payload.len() > shared.max_payload_bytes
        || wire_size > shared.max_payload_bytes + HEADER_LIMIT_BYTES
    {
        return Err(ConsumerError::SourceOversized);
    }
    let Ok(envelope) =
        wire::decode_envelope(source.subject.as_ref(), headers, source.payload.clone())
    else {
        return dead_letter(shared, options, &source, "malformed", cancel).await;
    };
    if info.delivered > 5 {
        return dead_letter(shared, options, &source, "exhausted", cancel).await;
    }
    let handler_cancel = cancel.child_token();
    let mut observation = HandlerObservation {
        started: Instant::now(),
        outcome: "terminal",
        cancel: handler_cancel.clone(),
    };
    let result = tokio::select! {
        biased;
        () = cancel.cancelled() => return Ok(()),
        result = tokio::time::timeout(HANDLER_TIMEOUT,
            registry.dispatch(source.subject.as_ref(), envelope, handler_cancel.clone())) => result,
    };
    observation.outcome = match &result {
        Ok(Ok(())) => "success",
        Ok(Err(HandlerError::Permanent)) => "permanent",
        Ok(Err(HandlerError::Retryable)) => "retryable",
        Err(_) => "timeout",
    };
    drop(observation);
    if cancel.is_cancelled() {
        return Ok(());
    }
    match result {
        Ok(Ok(())) => acknowledge(&source, cancel).await,
        Ok(Err(HandlerError::Permanent)) => {
            dead_letter(shared, options, &source, "permanent", cancel).await
        }
        Ok(Err(HandlerError::Retryable)) | Err(_) if info.delivered >= 5 => {
            dead_letter(shared, options, &source, "exhausted", cancel).await
        }
        Ok(Err(HandlerError::Retryable)) | Err(_) => {
            let index = usize::try_from(info.delivered - 1).map_err(|_| ConsumerError::Terminal)?;
            request_redelivery(&source, RETRY_DELAYS[index], cancel).await
        }
    }
}

struct HandlerObservation {
    started: Instant,
    outcome: &'static str,
    cancel: CancellationToken,
}

impl Drop for HandlerObservation {
    fn drop(&mut self) {
        let outcome = if self.cancel.is_cancelled() {
            "canceled"
        } else {
            self.outcome
        };
        self.cancel.cancel();
        metrics::counter!("messaging_handler_total", "outcome" => outcome).increment(1);
        metrics::histogram!("messaging_handler_duration_seconds", "outcome" => outcome)
            .record(self.started.elapsed().as_secs_f64());
    }
}

async fn dead_letter(
    shared: &Shared,
    options: &ConsumerOptions,
    source: &Message,
    reason: &'static str,
    cancel: &CancellationToken,
) -> Result<(), ConsumerError> {
    let info = source.info().map_err(|_| ConsumerError::Terminal)?;
    let empty = HeaderMap::new();
    let original = source.headers.as_ref().unwrap_or(&empty);
    let transfer_id = wire::dead_letter_id(
        info.stream,
        info.stream_sequence,
        info.published,
        wire::header_value(original, wire::NATS_MSG_ID),
    )
    .map_err(|_| ConsumerError::Terminal)?;
    let mut headers = HeaderMap::new();
    for name in [
        wire::MESSAGE_ID,
        wire::EVENT_TYPE,
        wire::EVENT_SCHEMA,
        wire::CREATED_AT,
        "traceparent",
        "tracestate",
    ] {
        let value = wire::header_value(original, name);
        if !value.is_empty() {
            headers.insert(name, value);
        }
    }
    if wire::header_value(&headers, wire::MESSAGE_ID).is_empty() {
        headers.insert(wire::MESSAGE_ID, transfer_id.as_str());
    }
    headers.insert(wire::NATS_MSG_ID, transfer_id.as_str());
    headers.insert(wire::ORIGINAL_SUBJECT, source.subject.as_str());
    headers.insert(wire::DEAD_LETTER_REASON, reason);
    let dlq_stream = shared
        .dlq_stream
        .as_deref()
        .ok_or(ConsumerError::DeadLetterRejected)?;
    headers.insert(async_nats::header::NATS_EXPECTED_STREAM, dlq_stream);
    wire::validate_encoded_message(
        &options.dlq_subject,
        &headers,
        source.payload.len(),
        shared.max_payload_bytes,
    )
    .map_err(|_| ConsumerError::DeadLetterOversized)?;
    let result = publish_raw(
        shared,
        &options.dlq_subject,
        headers,
        source.payload.clone(),
        dlq_stream,
        Instant::now() + BROKER_OPERATION_BUDGET,
        cancel,
    )
    .await;
    let outcome = match &result {
        Ok(_) => "accepted",
        Err(PublishError::Ambiguous) => "ambiguous",
        Err(PublishError::Rejected) => "rejected",
    };
    metrics::counter!("messaging_dead_letter_total", "reason" => reason, "outcome" => outcome)
        .increment(1);
    match result {
        Ok(_) => acknowledge(source, cancel).await,
        Err(PublishError::Ambiguous) => {
            request_redelivery(source, UNCERTAIN_REDELIVERY, cancel).await
        }
        Err(PublishError::Rejected) if cancel.is_cancelled() => Ok(()),
        Err(PublishError::Rejected) => Err(ConsumerError::DeadLetterRejected),
    }
}

async fn acknowledge(source: &Message, cancel: &CancellationToken) -> Result<(), ConsumerError> {
    let result = tokio::select! {
        biased;
        () = cancel.cancelled() => return Ok(()),
        result = tokio::time::timeout(BROKER_OPERATION_BUDGET, source.double_ack()) => result,
    };
    if matches!(result, Ok(Ok(()))) {
        Ok(())
    } else {
        // The domain effect is already successful. Retry delivery only, never
        // invoke the handler again in this process after ACK uncertainty.
        request_redelivery(source, UNCERTAIN_REDELIVERY, cancel).await
    }
}

async fn request_redelivery(
    source: &Message,
    delay: Duration,
    cancel: &CancellationToken,
) -> Result<(), ConsumerError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Ok(()),
        result = tokio::time::timeout(BROKER_OPERATION_BUDGET, source.ack_with(AckKind::Nak(Some(delay)))) => {
            result.map_err(|_| ConsumerError::RedeliveryFailed)?
                .map_err(|_| ConsumerError::RedeliveryFailed)
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
