mod delivery;

use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_nats::jetstream::consumer::pull;
use async_nats::jetstream::consumer::{
    AckPolicy, Config, DeliverPolicy, PullConsumer, ReplayPolicy,
};
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
use crate::topology::{ConsumerAdmission, bind_durable, durable_contract_error_type};
use crate::wire;

const ACK_WAIT: Duration = HANDLER_TIMEOUT
    .saturating_add(BROKER_OPERATION_BUDGET)
    .saturating_add(BROKER_OPERATION_BUDGET)
    .saturating_add(BROKER_OPERATION_BUDGET)
    .saturating_add(Duration::from_secs(1));
const STREAM_ERROR_BACKOFF: Duration = Duration::from_secs(1);
const PULL_EXPIRES: Duration = Duration::from_secs(30);
const PULL_EXPIRY_GRACE: Duration = Duration::from_secs(2);
const RELEASE_DELAY: Duration = Duration::from_secs(1);
const STREAM_ERRORS_TOTAL: &str = "messaging_consumer_stream_errors_total";

const ACK_SUBJECT_PREFIX: &str = "$JS.ACK.";
const ACK_ACCOUNT_HASH_BYTES: usize = 8;
const ACK_INTERIOR_SEPARATORS: usize = 3;
const ACK_NUMERIC_FIELDS: usize = 5;
const ACK_MAX_NUMERIC_FIELD_BYTES: usize = 1 + 20;
const ACK_FIXED_BYTES: usize = ACK_SUBJECT_PREFIX.len()
    + ACK_ACCOUNT_HASH_BYTES
    + ACK_INTERIOR_SEPARATORS
    + ACK_NUMERIC_FIELDS * ACK_MAX_NUMERIC_FIELD_BYTES;

fn pull_delivery_bytes(
    payload_bytes: usize,
    subject_bytes: usize,
    stream: &str,
    consumer: &str,
    domain: Option<&str>,
) -> Option<usize> {
    [
        wire::HEADER_LIMIT_BYTES,
        subject_bytes,
        stream.len(),
        consumer.len(),
        domain.map_or(0, str::len).max(1),
        ACK_FIXED_BYTES,
    ]
    .into_iter()
    .try_fold(payload_bytes, usize::checked_add)
}

#[derive(Debug)]
pub struct Consumer {
    shared: Arc<Shared>,
    pull: PullConsumer,
    concurrency: NonZeroUsize,
    pull_delivery_bytes: usize,
    retained_pull_bytes: usize,
    handler: Arc<DeliveryHandler>,
    durable_ready: Arc<AtomicBool>,
}

struct ConsumerProbe(Weak<AtomicBool>);

#[health::async_trait]
impl health::Probe for ConsumerProbe {
    fn name(&self) -> &'static str {
        "messaging_consumer"
    }

    async fn check(&self) -> Result<(), health::ProbeError> {
        let ready = self
            .0
            .upgrade()
            .ok_or_else(|| health::ProbeError::from_static("messaging consumer stopped"))?;
        if ready.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(health::ProbeError::from_static(
                "messaging durable is unavailable",
            ))
        }
    }
}

#[derive(Clone, Copy)]
struct PullLimits {
    messages: usize,
    bytes: usize,
    expires: Duration,
}

impl PullLimits {
    fn for_durable(config: &Config, delivery_bytes: usize) -> Option<Self> {
        fn positive_limit(value: i64) -> usize {
            usize::try_from(value)
                .ok()
                .filter(|limit| *limit > 0)
                .unwrap_or(usize::MAX)
        }

        let bytes = positive_limit(config.max_bytes);
        let messages = positive_limit(config.max_batch).min(bytes / delivery_bytes);
        (messages > 0).then_some(Self {
            messages,
            bytes,
            expires: if config.max_expires.is_zero() {
                PULL_EXPIRES
            } else {
                PULL_EXPIRES.min(config.max_expires)
            },
        })
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum ConsumerError {
    #[error("messaging durable consumer was deleted, replaced, or reconfigured incompatibly")]
    ConsumerLost,
    #[error("messaging source message exceeds the maximum retained-message pull size")]
    RetainedMessageTooLarge,
    #[error("messaging source sequence {sequence} exceeds the dead-letter publication size limit")]
    DeadLetterTooLarge { sequence: u64 },
    #[error(
        "messaging source sequence {sequence} cannot be transferred to a sealed dead-letter stream"
    )]
    DeadLetterStreamSealed { sequence: u64 },
    #[error("messaging consumer drain exceeded its shared deadline")]
    DrainTimedOut,
    #[error("messaging consumer task exited unexpectedly")]
    TaskEnded,
}

impl ConsumerError {
    const fn requires_delivery_drain(&self) -> bool {
        matches!(
            self,
            Self::DeadLetterTooLarge { .. } | Self::DeadLetterStreamSealed { .. }
        )
    }
}

#[derive(Debug)]
pub struct ConsumerHandle {
    drain: CancellationToken,
    abort: CancellationToken,
    failure: watch::Receiver<Option<ConsumerError>>,
    supervisor: Supervisor,
}

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
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<Self, MessagingError> {
        let ConsumerAdmission {
            options,
            dead_letter,
        } = &admission;
        if registry
            .handled_routes()
            .any(|(_, route)| !wire::matches_filter(&options.filter_subject, &route.subject))
        {
            return Err(MessagingError::Configuration(
                "handled subject is outside the consumer filter",
            ));
        }
        let server = shared.connection.client.server_info();
        let subject_bytes = dead_letter.admit_max_subject_bytes(
            &registry,
            &options.filter_subject,
            server.max_payload,
        )?;
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
        delivery_bytes
            .checked_mul(options.concurrency.get())
            .and_then(|bytes| i64::try_from(bytes).ok())
            .ok_or(MessagingError::Bounds)?;
        let config = async_nats::jetstream::consumer::pull::Config {
            durable_name: Some(options.durable_name.clone()),
            filter_subject: options.filter_subject.clone(),
            metadata: registry.durable_metadata(&dead_letter.stream, &options.dlq_subject),
            deliver_policy: DeliverPolicy::All,
            ack_policy: AckPolicy::Explicit,
            ack_wait: ACK_WAIT,
            max_deliver: -1,
            replay_policy: ReplayPolicy::Instant,
            ..Default::default()
        };
        let pull = bounded_operation(
            deadline,
            cancel,
            bind_durable(
                &shared.jetstream,
                &shared.source_stream,
                &options.durable_name,
                config,
            ),
        )
        .await?;
        if PullLimits::for_durable(&pull.cached_info().config, delivery_bytes).is_none() {
            tracing::warn!(operation = "bind_consumer", error.type = "durable_max_bytes", reason = %MessagingError::Topology, "messaging_admission_failed");
            return Err(MessagingError::Topology);
        }
        Ok(Self {
            concurrency: options.concurrency,
            handler: Arc::new(DeliveryHandler::new(
                Arc::clone(&shared),
                registry,
                admission,
            )),
            shared,
            pull,
            pull_delivery_bytes: delivery_bytes,
            retained_pull_bytes,
            durable_ready: Arc::new(AtomicBool::new(true)),
        })
    }

    #[must_use]
    pub fn probe(&self) -> impl health::Probe + use<> {
        ConsumerProbe(Arc::downgrade(&self.durable_ready))
    }

    #[must_use]
    pub fn start(self, cancel: &CancellationToken) -> ConsumerHandle {
        let drain = cancel.child_token();
        let abort = self.handler.abort.clone();
        let task_drain = drain.clone();
        let (failure_tx, failure) = watch::channel(None);
        let task = tokio::spawn(async move {
            let shared = Arc::clone(&self.shared);
            let result = self.run(task_drain).await;
            if let Err(error) = &result {
                if !matches!(error, ConsumerError::DrainTimedOut) {
                    shared.mark_failed();
                    tracing::error!(reason = %error, "messaging_consumer_stopped");
                }
                let _ = failure_tx.send(Some(error.clone()));
            }
            result
        });
        ConsumerHandle {
            drain,
            abort,
            failure,
            supervisor: Supervisor::Running(AbortOnDropHandle::new(task)),
        }
    }

    async fn run(self, drain: CancellationToken) -> Result<(), ConsumerError> {
        let mut deliveries = JoinSet::new();
        tokio::select! {
            biased;
            () = self.handler.abort.cancelled() => Err(ConsumerError::DrainTimedOut),
            result = async {
                let result = self.consume(&mut deliveries, &drain).await;
                if result.as_ref().is_err_and(ConsumerError::requires_delivery_drain) {
                    while deliveries.join_next().await.is_some() {}
                }
                result
            } => result,
        }
    }

    async fn consume(
        &self,
        deliveries: &mut JoinSet<Result<(), ConsumerError>>,
        drain: &CancellationToken,
    ) -> Result<(), ConsumerError> {
        let concurrency = self.concurrency.get();
        let mut mode = PullMode::Normal;
        'pulls: loop {
            while deliveries.len() == concurrency
                || (mode == PullMode::SingleRetainedMessage && !deliveries.is_empty())
            {
                tokio::select! {
                    biased;
                    () = drain.cancelled() => break 'pulls,
                    Some(joined) = deliveries.join_next() => check_delivery_task_result(joined)?,
                }
            }
            let status = tokio::select! {
                biased;
                () = drain.cancelled() => break 'pulls,
                status = self.refresh_durable_status() => status,
            };
            let limits = match status {
                DurableStatus::Lost => return Err(ConsumerError::ConsumerLost),
                DurableStatus::Ready(_) | DurableStatus::Unavailable(_) if drain.is_cancelled() => {
                    break 'pulls;
                }
                DurableStatus::Ready(limits) => limits,
                DurableStatus::Unavailable(error) => {
                    pull_failed("consumer_info", consumer_info_error_kind(&error.kind()));
                    if wait_for_backoff(drain).await.is_break() {
                        break 'pulls;
                    }
                    continue;
                }
            };
            let budget = self.pull_budget(mode, deliveries.len(), limits);
            let opened = self.open_batch(&budget, deliveries, drain).await?;
            let messages = match opened {
                BatchOpen::Ready(messages) => messages,
                BatchOpen::Retry => continue 'pulls,
                BatchOpen::Stop => break 'pulls,
            };
            match self
                .dispatch_batch(messages, deliveries, drain, &budget)
                .await?
            {
                BatchEnd::Filled => {
                    if mode == PullMode::SingleRetainedMessage {
                        if let Some(joined) = deliveries.join_next().await {
                            check_delivery_task_result(joined)?;
                        }
                        mode = PullMode::Normal;
                    }
                }
                BatchEnd::Stopped => break 'pulls,
                BatchEnd::Incomplete(incomplete) => match self
                    .recover_incomplete_batch(incomplete, mode, drain)
                    .await?
                {
                    ControlFlow::Continue(next_mode) => mode = next_mode,
                    ControlFlow::Break(()) => break 'pulls,
                },
            }
        }
        while let Some(joined) = deliveries.join_next().await {
            check_delivery_task_result(joined)?;
        }
        Ok(())
    }

    async fn recover_incomplete_batch(
        &self,
        incomplete: BatchIncomplete,
        mode: PullMode,
        drain: &CancellationToken,
    ) -> Result<ControlFlow<(), PullMode>, ConsumerError> {
        if self.recheck_durable_consumer(drain).await?.is_break() {
            return Ok(ControlFlow::Break(()));
        }
        match incomplete {
            BatchIncomplete::MaxBytesExceededBeforeFirstDelivery => {
                pull_failed("batch_receive", BatchReceiveError::MaxBytesExceeded.label());
                if mode == PullMode::SingleRetainedMessage {
                    return Err(ConsumerError::RetainedMessageTooLarge);
                }
                Ok(ControlFlow::Continue(PullMode::SingleRetainedMessage))
            }
            BatchIncomplete::Closed => Ok(ControlFlow::Continue(PullMode::Normal)),
            BatchIncomplete::Failed { phase, error_kind } => {
                pull_failed(phase, error_kind);
                if wait_for_backoff(drain).await.is_break() {
                    return Ok(ControlFlow::Break(()));
                }
                Ok(ControlFlow::Continue(PullMode::Normal))
            }
        }
    }

    async fn open_batch(
        &self,
        budget: &PullLimits,
        deliveries: &mut JoinSet<Result<(), ConsumerError>>,
        drain: &CancellationToken,
    ) -> Result<BatchOpen, ConsumerError> {
        let request = tokio::time::timeout(
            BROKER_OPERATION_BUDGET,
            self.pull
                .batch()
                .max_messages(budget.messages)
                .max_bytes(budget.bytes)
                .expires(budget.expires)
                .messages(),
        );
        tokio::pin!(request);
        let result = loop {
            tokio::select! {
                biased;
                () = drain.cancelled() => return Ok(BatchOpen::Stop),
                Some(joined) = deliveries.join_next(), if !deliveries.is_empty() => {
                    check_delivery_task_result(joined)?;
                }
                result = &mut request => break result,
            }
        };
        let error_kind = match result {
            Ok(Ok(messages)) => return Ok(BatchOpen::Ready(messages)),
            Ok(Err(error)) => batch_create_error_kind(error.kind()),
            Err(_) => "local_timeout",
        };
        if drain.is_cancelled() {
            return Ok(BatchOpen::Stop);
        }
        pull_failed("batch_create", error_kind);
        if self.recheck_durable_consumer(drain).await?.is_break()
            || wait_for_backoff(drain).await.is_break()
        {
            return Ok(BatchOpen::Stop);
        }
        Ok(BatchOpen::Retry)
    }

    async fn recheck_durable_consumer(
        &self,
        drain: &CancellationToken,
    ) -> Result<ControlFlow<()>, ConsumerError> {
        tokio::select! {
            biased;
            () = drain.cancelled() => Ok(ControlFlow::Break(())),
            status = self.refresh_durable_status() => match status {
                DurableStatus::Lost => Err(ConsumerError::ConsumerLost),
                DurableStatus::Ready(_) | DurableStatus::Unavailable(_) => {
                    if drain.is_cancelled() {
                        Ok(ControlFlow::Break(()))
                    } else {
                        Ok(ControlFlow::Continue(()))
                    }
                }
            },
        }
    }

    async fn refresh_durable_status(&self) -> DurableStatus {
        let result = tokio::time::timeout(BROKER_OPERATION_BUDGET, self.pull.get_info())
            .await
            .unwrap_or_else(|_| Err(ConsumerInfoError::new(ConsumerInfoErrorKind::TimedOut)));
        let status = match result {
            Ok(info) => {
                let bound = self.pull.cached_info();
                if let Some(error_type) = durable_contract_error_type(&bound.config, &info.config) {
                    tracing::warn!(
                        operation = "consumer_info",
                        error.type = error_type,
                        "messaging_durable_configuration_changed"
                    );
                    DurableStatus::Lost
                } else if info.created == bound.created {
                    PullLimits::for_durable(&info.config, self.pull_delivery_bytes)
                        .map_or(DurableStatus::Lost, DurableStatus::Ready)
                } else {
                    DurableStatus::Lost
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ConsumerInfoErrorKind::NotFound | ConsumerInfoErrorKind::StreamNotFound
                ) =>
            {
                DurableStatus::Lost
            }
            Err(error) => DurableStatus::Unavailable(error),
        };
        self.durable_ready
            .store(matches!(status, DurableStatus::Ready(_)), Ordering::Release);
        status
    }

    fn pull_budget(
        &self,
        mode: PullMode,
        active_deliveries: usize,
        limits: PullLimits,
    ) -> PullLimits {
        let (messages, bytes) = match mode {
            PullMode::SingleRetainedMessage => (1, self.retained_pull_bytes.min(limits.bytes)),
            PullMode::Normal => {
                let messages = (self.concurrency.get() - active_deliveries).min(limits.messages);
                (messages, messages * self.pull_delivery_bytes)
            }
        };
        PullLimits {
            messages,
            bytes,
            ..limits
        }
    }

    async fn dispatch_batch(
        &self,
        mut messages: pull::Batch,
        deliveries: &mut JoinSet<Result<(), ConsumerError>>,
        drain: &CancellationToken,
        budget: &PullLimits,
    ) -> Result<BatchEnd, ConsumerError> {
        let result = self
            .dispatch_batch_messages(&mut messages, deliveries, drain, budget)
            .await;
        match &result {
            Ok(BatchEnd::Stopped | BatchEnd::Incomplete(_)) => {
                release_prefetched_messages(
                    messages,
                    budget.messages,
                    drain,
                    &self.handler.filter_subject,
                )
                .await;
            }
            Err(error) if error.requires_delivery_drain() => {
                let _ = tokio::time::timeout(
                    BROKER_OPERATION_BUDGET,
                    release_prefetched_messages(
                        messages,
                        budget.messages,
                        drain,
                        &self.handler.filter_subject,
                    ),
                )
                .await;
            }
            Ok(BatchEnd::Filled) | Err(_) => {}
        }
        result
    }

    async fn dispatch_batch_messages(
        &self,
        messages: &mut pull::Batch,
        deliveries: &mut JoinSet<Result<(), ConsumerError>>,
        drain: &CancellationToken,
        budget: &PullLimits,
    ) -> Result<BatchEnd, ConsumerError> {
        let expires = Instant::now() + budget.expires + PULL_EXPIRY_GRACE;
        let mut remaining = budget.messages;
        let drained = drain.cancelled();
        tokio::pin!(drained);
        while remaining > 0 {
            let message = tokio::select! {
                biased;
                () = &mut drained => return Ok(BatchEnd::Stopped),
                Some(joined) = deliveries.join_next(), if !deliveries.is_empty() => {
                    check_delivery_task_result(joined)?;
                    continue;
                },
                next = tokio::time::timeout_at(expires, messages.next()) => match next {
                    Ok(Some(Ok(message))) => message,
                    Ok(None) => return Ok(BatchEnd::Incomplete(BatchIncomplete::Closed)),
                    Ok(Some(Err(error))) => {
                        let failure = classify_batch_receive_error(&error);
                        if remaining == budget.messages
                            && matches!(failure, BatchReceiveError::MaxBytesExceeded)
                        {
                            return Ok(BatchEnd::Incomplete(
                                BatchIncomplete::MaxBytesExceededBeforeFirstDelivery,
                            ));
                        }
                        return Ok(BatchEnd::Incomplete(BatchIncomplete::Failed {
                            phase: "batch_receive",
                            error_kind: failure.label(),
                        }));
                    }
                    Err(_) => return Ok(BatchEnd::Incomplete(BatchIncomplete::Failed {
                        phase: "batch_receive",
                        error_kind: "local_timeout",
                    })),
                }
            };
            if remaining == budget.messages {
                let status = tokio::select! {
                    biased;
                    () = &mut drained => {
                        release_message(&message, drain, &self.handler.filter_subject).await;
                        return Ok(BatchEnd::Stopped);
                    }
                    status = self.refresh_durable_status() => status,
                };
                match status {
                    DurableStatus::Ready(_) => {}
                    DurableStatus::Lost => return Err(ConsumerError::ConsumerLost),
                    DurableStatus::Unavailable(error) => {
                        release_message(&message, drain, &self.handler.filter_subject).await;
                        return Ok(BatchEnd::Incomplete(BatchIncomplete::Failed {
                            phase: "consumer_info",
                            error_kind: consumer_info_error_kind(&error.kind()),
                        }));
                    }
                }
            }
            if drain.is_cancelled() {
                release_message(&message, drain, &self.handler.filter_subject).await;
                return Ok(BatchEnd::Stopped);
            }
            remaining -= 1;
            let handler = Arc::clone(&self.handler);
            deliveries.spawn(async move { handler.handle(message).await });
        }
        Ok(BatchEnd::Filled)
    }
}

enum DurableStatus {
    Ready(PullLimits),
    Lost,
    Unavailable(ConsumerInfoError),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PullMode {
    Normal,
    SingleRetainedMessage,
}

#[expect(
    clippy::large_enum_variant,
    reason = "inline batch handoff avoids a heap allocation per pull"
)]
enum BatchOpen {
    Ready(pull::Batch),
    Retry,
    Stop,
}

#[derive(Clone, Copy, Debug)]
enum BatchEnd {
    Filled,
    Stopped,
    Incomplete(BatchIncomplete),
}

#[derive(Clone, Copy, Debug)]
enum BatchIncomplete {
    Closed,
    MaxBytesExceededBeforeFirstDelivery,
    Failed {
        phase: &'static str,
        error_kind: &'static str,
    },
}

fn check_delivery_task_result(
    joined: Result<Result<(), ConsumerError>, JoinError>,
) -> Result<(), ConsumerError> {
    joined.map_err(|_| ConsumerError::TaskEnded).flatten()
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
    let Some(error) = error.downcast_ref::<pull::BatchStatusError>() else {
        return BatchReceiveError::Other(if error.is::<std::io::Error>() {
            "io_error"
        } else {
            "other"
        });
    };
    let label = match (error.status.as_u16(), error.description.as_deref()) {
        (409, Some("Message Size Exceeds MaxBytes")) => {
            return BatchReceiveError::MaxBytesExceeded;
        }
        (400, _) => "status_400",
        (403, _) => "status_403",
        (404, _) => "status_404",
        (408, _) => "status_408",
        (409, _) => "status_409",
        (503, _) => "status_503",
        _ => "status_other",
    };
    BatchReceiveError::Other(label)
}

fn pull_failed(phase: &'static str, error_type: &'static str) {
    tracing::warn!(phase, error.type = error_type, "messaging_pull_failed");
    metrics::counter!(STREAM_ERRORS_TOTAL).increment(1);
}

async fn wait_for_backoff(drain: &CancellationToken) -> ControlFlow<()> {
    tokio::select! {
        biased;
        () = drain.cancelled() => ControlFlow::Break(()),
        () = tokio::time::sleep(STREAM_ERROR_BACKOFF) => ControlFlow::Continue(()),
    }
}

async fn release_prefetched_messages(
    mut messages: pull::Batch,
    slots: usize,
    drain: &CancellationToken,
    filter_subject: &str,
) {
    let mut prefetched = Vec::new();
    for _ in 0..slots {
        let Some(Some(item)) = messages.next().now_or_never() else {
            break;
        };
        if let Ok(message) = item {
            prefetched.push(message);
        }
    }
    drop(messages);
    for message in &prefetched {
        release_message(message, drain, filter_subject).await;
    }
}

async fn release_message(
    message: &async_nats::jetstream::Message,
    drain: &CancellationToken,
    filter_subject: &str,
) {
    let request = redeliver_after(message, RELEASE_DELAY, filter_subject);
    tokio::pin!(request);
    tokio::select! {
        biased;
        () = drain.cancelled() => {
            let _ = request.now_or_never();
        }
        () = &mut request => {}
    }
}

pub(crate) fn describe_metrics() {
    use metrics::describe_counter;

    describe_counter!(STREAM_ERRORS_TOTAL, "Recoverable pull-stream errors");
    delivery::describe_metrics();
}

impl ConsumerHandle {
    pub fn drain(&self) {
        self.drain.cancel();
    }

    pub async fn failed(&self) -> ConsumerError {
        let mut failure = self.failure.clone();
        match failure.wait_for(Option::is_some).await {
            Ok(error) => error.clone().unwrap_or(ConsumerError::TaskEnded),
            Err(_) => ConsumerError::TaskEnded,
        }
    }

    pub fn abort(&self) {
        self.drain.cancel();
        self.abort.cancel();
    }

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
        for (code, description, expected) in [
            (409, "Message Size Exceeds MaxBytes", "max_bytes_exceeded"),
            (409, "Message Size Exceeds MaxBytes extra", "status_409"),
            (503, "sentinel.provider.chosen", "status_503"),
            (500, "sentinel.provider.chosen", "status_other"),
        ] {
            let error: async_nats::Error = Box::new(pull::BatchStatusError {
                status: async_nats::StatusCode::from_u16(code).unwrap(),
                description: Some(description.to_owned()),
            });
            assert_eq!(classify_batch_receive_error(&error).label(), expected);
        }
        let error: async_nats::Error =
            Box::new(std::io::Error::other("Message Size Exceeds MaxBytes"));
        assert_eq!(classify_batch_receive_error(&error).label(), "io_error");
        let error: async_nats::Error = Box::new(std::fmt::Error);
        assert_eq!(classify_batch_receive_error(&error).label(), "other");
    }

    #[tokio::test(start_paused = true)]
    async fn expired_finish_retains_the_task_without_spending_cleanup_time() {
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let (_failure_sender, failure) = watch::channel(None);
        let mut handle = ConsumerHandle {
            drain: CancellationToken::new(),
            abort: CancellationToken::new(),
            failure,
            supervisor: Supervisor::Running(AbortOnDropHandle::new(tokio::spawn(async {
                let _ = released.await;
                Ok(())
            }))),
        };
        let deadline = Instant::now() + Duration::from_secs(1);
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
