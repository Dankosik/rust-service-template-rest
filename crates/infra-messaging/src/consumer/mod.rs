//! The pull loop of one admitted durable consumer and the handle the
//! composition root owns to drain, abort and join it.

mod delivery;

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::consumer::pull;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, PullConsumer, ReplayPolicy};
use async_nats::jetstream::context::{ConsumerInfoError, ConsumerInfoErrorKind};
use futures_util::{FutureExt as _, StreamExt as _};
use tokio::sync::watch;
use tokio::task::{JoinError, JoinSet};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use self::delivery::{DeliveryHandler, HANDLER_TIMEOUT, redeliver_after};
use crate::budget::{BROKER_OPERATION_BUDGET, bounded_operation};
use crate::error::MessagingError;
use crate::messaging::{PUBLICATION_WINDOW_BYTES, Shared};
use crate::registry::Registry;
use crate::topology::{ConsumerAdmission, create_durable, get_stream_failure};
use crate::wire;

/// The broker redelivers an unsettled message after the batch identity check,
/// handler budget and both settlement round trips.
const ACK_WAIT: Duration = HANDLER_TIMEOUT
    .saturating_add(BROKER_OPERATION_BUDGET)
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
const STREAM_ERRORS_TOTAL: &str = "messaging_consumer_stream_errors_total";

/// NATS 2.15 counts the original subject and ACK reply in a pull's byte limit.
/// V2 ACKs cover V1 too: prefix, domain, eight-byte account hash, stream,
/// consumer, then five 64-bit decimal fields (including a signed timestamp).
fn pull_delivery_bytes(
    payload_bytes: usize,
    subject_bytes: usize,
    stream: &str,
    consumer: &str,
    domain: Option<&str>,
) -> Option<usize> {
    // Three separators between names, then a separator and at most 20 bytes
    // for each numeric field. An absent or empty domain is written as "_".
    let ack_fixed_bytes = "$JS.ACK.".len() + 8 + 3 + 5 * 21;
    [
        wire::HEADER_LIMIT_BYTES,
        subject_bytes,
        stream.len(),
        consumer.len(),
        domain.map_or(0, str::len).max(1),
        ack_fixed_bytes,
    ]
    .into_iter()
    .try_fold(payload_bytes, usize::checked_add)
}

/// A bounded pull consumer admitted against an existing source stream.
#[derive(Debug)]
pub struct Consumer {
    shared: Arc<Shared>,
    pull: PullConsumer,
    concurrency: NonZeroUsize,
    pull_delivery_bytes: usize,
    retained_pull_bytes: usize,
    handler: Arc<DeliveryHandler>,
}

/// Terminal worker failure or incomplete shutdown. Reasons never contain input.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ConsumerError {
    #[error("messaging durable consumer was deleted or replaced")]
    ConsumerLost,
    #[error("messaging source message exceeds the maximum retained-message pull size")]
    RetainedMessageTooLarge,
    #[error("messaging source sequence {sequence} exceeds the dead-letter publication size limit")]
    DeadLetterTooLarge { sequence: u64 },
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
    Running(AbortOnDropHandle<Result<(), ConsumerError>>),
    Finished(Result<(), ConsumerError>),
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
            deadline,
            cancel,
        } = admission;
        // A route without a handler is publish-only and need not be consumed.
        if registry
            .handled_routes()
            .any(|route| !wire::matches_filter(&options.filter_subject, &route.subject))
        {
            return Err(MessagingError::Configuration(
                "handled subject is outside the consumer filter",
            ));
        }
        let server = shared.connection.client.server_info();
        let subject_bytes =
            dead_letter.admit_routes(&registry, &options.filter_subject, server.max_payload)?;
        let delivery_bytes = pull_delivery_bytes(
            shared.max_payload_bytes,
            subject_bytes,
            &shared.source_stream,
            &options.durable_name,
            server.domain.as_deref(),
        )
        .ok_or(MessagingError::Bounds)?;
        let retained_pull_bytes = pull_delivery_bytes(
            PUBLICATION_WINDOW_BYTES - wire::HEADER_LIMIT_BYTES,
            subject_bytes,
            &shared.source_stream,
            &options.durable_name,
            server.domain.as_deref(),
        )
        .filter(|bytes| i64::try_from(*bytes).is_ok())
        .ok_or(MessagingError::Bounds)?;
        // `consume` multiplies a full pull's budget unchecked, and the broker
        // reads its byte limit as an i64.
        delivery_bytes
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
        let pull = bounded_operation(deadline, &cancel, async {
            let stream = shared
                .jetstream
                .get_stream(&shared.source_stream)
                .await
                .map_err(|error| get_stream_failure(&error, "source"))?;
            create_durable(&stream, config).await
        })
        .await?;
        Ok(Self {
            handler: Arc::new(DeliveryHandler::new(
                Arc::clone(&shared),
                registry,
                options.filter_subject,
                options.dlq_subject,
                dead_letter.stream,
            )),
            shared,
            pull,
            concurrency: options.concurrency,
            pull_delivery_bytes: delivery_bytes,
            retained_pull_bytes,
        })
    }

    /// Starts the pull loop. Cancelling `cancel` stops admitting new deliveries.
    /// Admitted deliveries keep their handler and settlement allowances during drain.
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
                // The owner's forced cut ends this consumer only; the shared
                // connection still serves producers, other consumers and
                // readiness.
                if !matches!(error, ConsumerError::DrainTimedOut) {
                    shared.mark_failed();
                    tracing::error!(reason = %error, "messaging_consumer_stopped");
                }
                let _ = failure_tx.send(Some(error.clone()));
            }
            result
        });
        ConsumerHandle {
            stop,
            force,
            failure,
            supervisor: Supervisor::Running(AbortOnDropHandle::new(task)),
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
            result = async {
                let result = self.consume(&mut deliveries, &stop, &force).await;
                if matches!(result, Err(ConsumerError::DeadLetterTooLarge { .. })) {
                    // Keep the first failure while admitted siblings settle.
                    while deliveries.join_next().await.is_some() {}
                }
                result
            } => result,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one admission loop preserves stop, terminal failure, and admitted-work join ordering"
    )]
    async fn consume(
        &self,
        deliveries: &mut JoinSet<Result<(), ConsumerError>>,
        stop: &CancellationToken,
        force: &CancellationToken,
    ) -> Result<(), ConsumerError> {
        let concurrency = self.concurrency.get();
        let mut retry_large_retained_record = false;
        'pulls: loop {
            while deliveries.len() == concurrency
                || (retry_large_retained_record && !deliveries.is_empty())
            {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => break 'pulls,
                    joined = deliveries.join_next() => check_delivery_task_result(joined)?,
                }
            }
            if stop.is_cancelled() {
                break;
            }
            // Pull requests address a durable by name, not creation identity.
            // A replacement between successful full batches has no waiting
            // request to terminate, so verify identity before every new pull.
            let loss_check = tokio::select! {
                biased;
                () = stop.cancelled() => break 'pulls,
                loss_check = check_durable_loss(&self.pull) => loss_check,
            };
            match loss_check {
                Ok(false) => {}
                Ok(true) => return Err(ConsumerError::ConsumerLost),
                Err(error) => {
                    if stop.is_cancelled() {
                        break 'pulls;
                    }
                    // Do not admit from a same-name replacement while the
                    // broker cannot confirm the durable's creation identity.
                    pull_failed("consumer_info", consumer_info_error_kind(&error.kind()));
                    tokio::select! {
                        biased;
                        () = stop.cancelled() => break 'pulls,
                        () = tokio::time::sleep(STREAM_ERROR_BACKOFF) => {}
                    }
                    continue;
                }
            }
            if stop.is_cancelled() {
                break 'pulls;
            }
            let (slots, max_bytes) =
                self.pull_budget(deliveries.len(), retry_large_retained_record);
            let request = tokio::time::timeout(
                BROKER_OPERATION_BUDGET,
                self.pull
                    .batch()
                    .max_messages(slots)
                    .max_bytes(max_bytes)
                    .expires(PULL_EXPIRES)
                    .messages(),
            );
            tokio::pin!(request);
            let mut messages = loop {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => break 'pulls,
                    joined = deliveries.join_next(), if !deliveries.is_empty() => check_delivery_task_result(joined)?,
                    result = &mut request => {
                        let error_kind = match result {
                            Ok(Ok(messages)) => break messages,
                            Ok(Err(error)) => batch_create_error_kind(error.kind()),
                            Err(_) => "local_timeout",
                        };
                        if stop.is_cancelled() {
                            break 'pulls;
                        }
                        pull_failed("batch_create", error_kind);
                        let lost = tokio::select! {
                            biased;
                            () = stop.cancelled() => break 'pulls,
                            lost = durable_confirmed_lost(&self.pull) => lost,
                        };
                        if lost {
                            return Err(ConsumerError::ConsumerLost);
                        }
                        tokio::select! {
                            biased;
                            () = stop.cancelled() => break 'pulls,
                            () = tokio::time::sleep(STREAM_ERROR_BACKOFF) => {}
                        }
                        continue 'pulls;
                    }
                }
            };
            match self
                .fill(&mut messages, deliveries, stop, force, slots)
                .await
            {
                Ok(BatchEnd::Filled) => {
                    if retry_large_retained_record {
                        check_delivery_task_result(deliveries.join_next().await)?;
                        retry_large_retained_record = false;
                    }
                }
                Ok(
                    end @ (BatchEnd::Stopped
                    | BatchEnd::Closed
                    | BatchEnd::MaxBytesExceededBeforeFirstDelivery
                    | BatchEnd::Failed { .. }),
                ) => {
                    release(messages, slots, stop).await;
                    if matches!(end, BatchEnd::Stopped) || stop.is_cancelled() {
                        break 'pulls;
                    }
                    let lost = tokio::select! {
                        biased;
                        () = stop.cancelled() => break 'pulls,
                        lost = durable_confirmed_lost(&self.pull) => lost,
                    };
                    if lost {
                        return Err(ConsumerError::ConsumerLost);
                    }
                    if stop.is_cancelled() {
                        break 'pulls;
                    }
                    let was_retrying_large_retained_record = retry_large_retained_record;
                    retry_large_retained_record =
                        matches!(end, BatchEnd::MaxBytesExceededBeforeFirstDelivery);
                    if retry_large_retained_record {
                        pull_failed("batch_receive", BatchReceiveError::MaxBytesExceeded.label());
                        // A multi-message pull may report 409 before already
                        // allocated records reach this replica's subscription.
                        if was_retrying_large_retained_record {
                            return Err(ConsumerError::RetainedMessageTooLarge);
                        }
                    } else if let BatchEnd::Failed { phase, error_kind } = end {
                        pull_failed(phase, error_kind);
                        tokio::select! {
                            biased;
                            () = stop.cancelled() => break 'pulls,
                            () = tokio::time::sleep(STREAM_ERROR_BACKOFF) => {}
                        }
                    }
                }
                Err(error @ ConsumerError::DeadLetterTooLarge { .. }) => {
                    let _ = tokio::time::timeout(
                        BROKER_OPERATION_BUDGET,
                        release(messages, slots, stop),
                    )
                    .await;
                    return Err(error);
                }
                Err(error) => return Err(error),
            }
        }
        while let Some(joined) = deliveries.join_next().await {
            check_delivery_task_result(Some(joined))?;
        }
        Ok(())
    }

    fn pull_budget(
        &self,
        active_deliveries: usize,
        retry_large_retained_record: bool,
    ) -> (usize, usize) {
        if retry_large_retained_record {
            // The caller drains every other slot before reserving a full-size
            // delivery for a record that may predate the current source limit.
            (1, self.retained_pull_bytes)
        } else {
            let slots = self.concurrency.get() - active_deliveries;
            (slots, slots * self.pull_delivery_bytes)
        }
    }

    /// Admits up to `slots` deliveries of one pull batch, reaping finished
    /// deliveries meanwhile.
    async fn fill(
        &self,
        messages: &mut pull::Batch,
        deliveries: &mut JoinSet<Result<(), ConsumerError>>,
        stop: &CancellationToken,
        force: &CancellationToken,
        slots: usize,
    ) -> Result<BatchEnd, ConsumerError> {
        // Batch has no missing-heartbeat watchdog. Bound unanswered pulls
        // here, after the broker's expiry answer is due and before the
        // SDK's own timer would end the batch as if it had answered.
        let expires = Instant::now() + PULL_EXPIRES + PULL_EXPIRY_GRACE;
        let mut remaining = slots;
        while remaining > 0 {
            tokio::select! {
                biased;
                () = stop.cancelled() => return Ok(BatchEnd::Stopped),
                joined = deliveries.join_next(), if !deliveries.is_empty() => check_delivery_task_result(joined)?,
                next = tokio::time::timeout_at(expires, messages.next()) => match next {
                    Ok(Some(Ok(message))) => {
                        if remaining == slots {
                            // The preflight INFO and the named pull are separate requests.
                            // Verify creation identity after this batch has delivered data.
                            let loss_check = tokio::select! {
                                biased;
                                () = stop.cancelled() => {
                                    release_message(&message, stop).await;
                                    return Ok(BatchEnd::Stopped);
                                }
                                loss_check = check_durable_loss(&self.pull) => loss_check,
                            };
                            match loss_check {
                                Ok(false) => {}
                                Ok(true) => return Err(ConsumerError::ConsumerLost),
                                Err(error) => {
                                    release_message(&message, stop).await;
                                    return Ok(BatchEnd::Failed {
                                        phase: "consumer_info",
                                        error_kind: consumer_info_error_kind(&error.kind()),
                                    });
                                }
                            }
                        }
                        if stop.is_cancelled() {
                            release_message(&message, stop).await;
                            return Ok(BatchEnd::Stopped);
                        }
                        remaining -= 1;
                        let handler = Arc::clone(&self.handler);
                        let cancel = force.clone();
                        deliveries.spawn(async move { handler.handle(message, cancel).await });
                    }
                    Ok(None) => return Ok(BatchEnd::Closed),
                    Ok(Some(Err(error))) => {
                        let failure = classify_batch_receive_error(&error);
                        if remaining == slots && matches!(failure, BatchReceiveError::MaxBytesExceeded) {
                            return Ok(BatchEnd::MaxBytesExceededBeforeFirstDelivery);
                        }
                        return Ok(BatchEnd::Failed {
                            phase: "batch_receive",
                            error_kind: failure.label(),
                        });
                    }
                    Err(_) => return Ok(BatchEnd::Failed {
                        phase: "batch_receive",
                        error_kind: "local_timeout",
                    }),
                }
            }
        }
        Ok(BatchEnd::Filled)
    }
}

/// How admitting one pull batch's deliveries ended.
#[derive(Clone, Copy, Debug)]
enum BatchEnd {
    /// Every reserved slot received a delivery.
    Filled,
    Stopped,
    /// The batch ended before filling its slots.
    Closed,
    MaxBytesExceededBeforeFirstDelivery,
    /// The batch could not receive or verify a delivery.
    Failed {
        phase: &'static str,
        error_kind: &'static str,
    },
}

/// A delivery task that panicked or was cancelled ends the consumer.
fn check_delivery_task_result(
    joined: Option<Result<Result<(), ConsumerError>, JoinError>>,
) -> Result<(), ConsumerError> {
    match joined {
        Some(result) => result.map_err(|_| ConsumerError::TaskEnded).flatten(),
        None => Ok(()),
    }
}

/// An unanswered lookup does not confirm loss of the admitted durable.
async fn durable_confirmed_lost(pull: &PullConsumer) -> bool {
    matches!(check_durable_loss(pull).await, Ok(true))
}

/// Whether the broker answers that the durable, or its stream, no longer
/// exists, or was replaced with another creation or a push consumer.
/// A failed or unanswered lookup is not that answer.
async fn check_durable_loss(pull: &PullConsumer) -> Result<bool, ConsumerInfoError> {
    // The native response timeout starts after the client's command queue admits the request.
    let result = tokio::time::timeout(BROKER_OPERATION_BUDGET, pull.get_info())
        .await
        .map_err(|_| ConsumerInfoError::new(ConsumerInfoErrorKind::TimedOut))?;
    match result {
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

enum BatchReceiveError {
    MaxBytesExceeded,
    Other(&'static str),
}

impl BatchReceiveError {
    const fn label(&self) -> &'static str {
        match self {
            Self::MaxBytesExceeded => "max_bytes_exceeded",
            Self::Other(label) => label,
        }
    }
}

fn classify_batch_receive_error(error: &async_nats::Error) -> BatchReceiveError {
    let Some(error) = error.downcast_ref::<std::io::Error>() else {
        return BatchReceiveError::Other("other");
    };
    if error.kind() != std::io::ErrorKind::Other {
        return BatchReceiveError::Other("io_error");
    }
    // async-nats 0.50 Batch erases status/description into io::Error(Other).
    // Recognize only the pinned SDK format. The exact byte-limit status also
    // selects the bounded retained-record pull.
    let description = error.to_string();
    let Some(status) =
        description.strip_prefix("error while processing messages from the stream: ")
    else {
        return BatchReceiveError::Other("io_error");
    };
    let label = match status {
        "409, Some(\"Batch Completed\")" => "batch_completed",
        "409, Some(\"Message Size Exceeds MaxBytes\")" => {
            return BatchReceiveError::MaxBytesExceeded;
        }
        _ => match status.split_once(", ").map(|(code, _)| code) {
            Some("400") => "status_400",
            Some("403") => "status_403",
            Some("404") => "status_404",
            Some("408") => "status_408",
            Some("409") => "status_409",
            Some("503") => "status_503",
            _ => "status_other",
        },
    };
    BatchReceiveError::Other(label)
}

fn pull_failed(phase: &'static str, error_type: &'static str) {
    tracing::warn!(phase, error.type = error_type, "messaging_pull_failed");
    metrics::counter!(STREAM_ERRORS_TOTAL).increment(1);
}

// Missing or unsubmitted deliveries remain eligible for broker ack-wait
// redelivery. Stop must not spend admitted work's drain window on release I/O.
async fn release(mut messages: pull::Batch, slots: usize, stop: &CancellationToken) {
    let mut prefetched = Vec::new();
    for _ in 0..slots {
        let Some(Some(item)) = messages.next().now_or_never() else {
            break;
        };
        if let Ok(message) = item {
            prefetched.push(message);
        }
    }
    // Ends this replica's pulls before the redelivery requests go out.
    drop(messages);
    for message in &prefetched {
        release_message(message, stop).await;
    }
}

async fn release_message(message: &async_nats::jetstream::Message, stop: &CancellationToken) {
    let request = redeliver_after(message, RELEASE_DELAY);
    tokio::pin!(request);
    tokio::select! {
        biased;
        () = stop.cancelled() => {
            let _ = request.now_or_never();
        }
        () = &mut request => {}
    }
}

/// Describes the delivery metrics to the installed recorder. Repeating it is
/// harmless.
pub(crate) fn describe_metrics() {
    use metrics::describe_counter;

    describe_counter!(STREAM_ERRORS_TOTAL, "Recoverable pull-stream errors");
    delivery::describe_metrics();
}

impl ConsumerHandle {
    /// Requests an end to admission, interrupting admission-only waits.
    /// Admitted work keeps its handler and settlement allowances. Locally held
    /// unadmitted deliveries get a best-effort delayed NAK without waiting for
    /// broker capacity after stop; ack-wait retains redelivery ownership.
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
    /// Success confirms all admitted work joined after admission stopped.
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
        // Exhausted cleanup has no budget left to observe a join. Dropping the
        // supervisor handle aborts the task, which drops its pipeline and
        // aborts every delivery; runtime shutdown remains the final resource
        // owner.
        self.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_budget_covers_native_ack_metadata_and_checks_overflow() {
        let subject = "x".repeat(57_000);
        let source = "SOURCE";
        let consumer = "handler";
        let maximum = u64::MAX;
        let timestamp = i64::MIN;
        let v1 = format!(
            "$JS.ACK.{source}.{consumer}.{maximum}.{maximum}.{maximum}.{timestamp}.{maximum}"
        );
        let long_domain = "domain".repeat(1000);
        for domain in [None, Some(""), Some(long_domain.as_str())] {
            let ack_domain = domain.filter(|domain| !domain.is_empty()).unwrap_or("_");
            let v2 = format!(
                "$JS.ACK.{ack_domain}.01234567.{source}.{consumer}.{maximum}.{maximum}.{maximum}.{timestamp}.{maximum}"
            );
            let bound = pull_delivery_bytes(1024, subject.len(), source, consumer, domain).unwrap();
            assert_eq!(
                bound,
                1024 + wire::HEADER_LIMIT_BYTES + subject.len() + v2.len()
            );
            assert!(1024 + wire::HEADER_LIMIT_BYTES + subject.len() + v1.len() <= bound);
        }
        for [payload, subject] in [[usize::MAX, 1], [1, usize::MAX]] {
            assert!(pull_delivery_bytes(payload, subject, "s", "c", Some("d")).is_none());
        }
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
            assert_eq!(classify_batch_receive_error(&error).label(), expected);
        }
        let error: async_nats::Error = Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "error while processing messages from the stream: 409, Some(\"Batch Completed\")",
        ));
        assert_eq!(classify_batch_receive_error(&error).label(), "io_error");
        let error: async_nats::Error = Box::new(std::fmt::Error);
        assert_eq!(classify_batch_receive_error(&error).label(), "other");
    }

    #[tokio::test(start_paused = true)]
    async fn expired_finish_retains_the_task_without_spending_cleanup_time() {
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let (_failure_sender, failure) = watch::channel(None);
        let mut handle = ConsumerHandle {
            stop: CancellationToken::new(),
            force: CancellationToken::new(),
            failure,
            supervisor: Supervisor::Running(AbortOnDropHandle::new(tokio::spawn(async {
                let _ = released.await;
                Ok(())
            }))),
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
