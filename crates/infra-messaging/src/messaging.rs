use std::future::Future;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use secrecy::SecretString;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::connection::{close_client, connect_client};
use crate::consumer::Consumer;
use crate::error::MessagingError;
use crate::observe::Outcomes;
use crate::probe::{MessagingProbe, PublishWork};
use crate::producer::{Producer, PublishOutcome};
use crate::registry::Registry;
use crate::topology::{ConsumerAdmission, admit_topology};
use crate::wire::HEADER_LIMIT_BYTES;

/// Bounds every broker round trip, inside any longer caller deadline.
pub const BROKER_OPERATION_BUDGET: Duration = Duration::from_secs(5);
/// One native publication window shared by source, outbox and dead-letter
/// publication; its permits are `floor(window / envelope_limit)`.
const PUBLICATION_WINDOW_BYTES: usize = 64 * 1024 * 1024;

/// The earlier of `deadline` and one [`BROKER_OPERATION_BUDGET`] from now.
pub(crate) fn within_operation_budget(deadline: Instant) -> Instant {
    deadline.min(Instant::now() + BROKER_OPERATION_BUDGET)
}

/// Configuration already validated by the composition root, which owns the
/// transport, credential and resident-memory policy.
#[derive(Clone)]
pub struct MessagingOptions {
    /// Shown by the broker's connection reports, so an operator can tell
    /// this process's connection from its neighbors'.
    pub connection_name: String,
    /// A URL can carry credentials, so `Debug` shows only the count.
    pub servers: Vec<String>,
    /// `None` connects without credentials.
    pub credentials: Option<Credentials>,
    pub root_ca_path: Option<PathBuf>,
    pub allow_plaintext: bool,
    /// Authenticate TLS before accepting server INFO and discovered destinations.
    pub tls_first: bool,
    pub source_stream: String,
    pub max_payload_bytes: usize,
    /// Durable consumer settings; `None` means this process only publishes.
    pub consumer: Option<ConsumerOptions>,
}

/// Where the broker credentials come from.
#[derive(Clone)]
pub enum Credentials {
    /// The content of a credentials file. `Debug` redacts it.
    Inline(SecretString),
    /// A credentials file read again for every connection, for a platform
    /// that rotates it.
    File(PathBuf),
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inline(secret) => formatter.debug_tuple("Inline").field(secret).finish(),
            Self::File(path) => formatter.debug_tuple("File").field(path).finish(),
        }
    }
}

impl std::fmt::Debug for MessagingOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessagingOptions")
            .field("connection_name", &self.connection_name)
            // A server URL can carry a user and password.
            .field("server_count", &self.servers.len())
            .field("credentials", &self.credentials)
            .field("root_ca_path", &self.root_ca_path)
            .field("allow_plaintext", &self.allow_plaintext)
            .field("tls_first", &self.tls_first)
            .field("source_stream", &self.source_stream)
            .field("max_payload_bytes", &self.max_payload_bytes)
            .field("consumer", &self.consumer)
            .finish()
    }
}

/// Consumer fields coupled to adapter settlement and resource admission.
#[derive(Clone, Debug)]
pub struct ConsumerOptions {
    pub durable_name: String,
    pub filter_subject: String,
    pub dlq_subject: String,
    /// The stream that stores `dlq_subject`; `None` asks the broker which
    /// stream does.
    pub dlq_stream: Option<String>,
    pub concurrency: NonZeroUsize,
}

/// One connected messaging dependency and its lifecycle state.
///
/// Cloning yields another handle to the same connection and shared state;
/// [`Messaging::close`] drains and closes that connection for every handle.
#[derive(Clone, Debug)]
pub struct Messaging {
    pub(crate) shared: Arc<Shared>,
    consumer: Option<ConsumerAdmission>,
}

/// Retains a partially admitted native connection until it can be transferred
/// to [`Messaging`] or explicitly closed by the process owner.
#[derive(Debug)]
pub struct MessagingStartup {
    options: Option<MessagingOptions>,
    publication_permits: usize,
    envelope_limit: usize,
    deadline: Instant,
    cancel: CancellationToken,
    connection: Option<(async_nats::Client, watch::Receiver<bool>)>,
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) client: async_nats::Client,
    pub(crate) jetstream: async_nats::jetstream::Context,
    pub(crate) source_stream: String,
    pub(crate) max_payload_bytes: usize,
    /// The largest encoded envelope the configured payload limit admits.
    pub(crate) envelope_limit: usize,
    pub(crate) startup_deadline: Instant,
    pub(crate) startup_cancel: CancellationToken,
    draining: AtomicBool,
    failed: AtomicBool,
    pub(crate) publish_metrics: Outcomes<PublishOutcome>,
    pub(crate) publish_admission_refused: metrics::Counter,
    pub(crate) publish_work: PublishWork,
    /// Final event submission is separate from native runner completion.
    closed_event: watch::Receiver<bool>,
}

impl Shared {
    /// Whether this resource is draining or its consumer failed.
    pub(crate) fn refuses_work(&self) -> bool {
        self.is_draining() || self.has_failed()
    }

    pub(crate) fn is_draining(&self) -> bool {
        self.draining.load(Ordering::Acquire)
    }

    pub(crate) fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    fn begin_drain(&self) {
        self.draining.store(true, Ordering::Release);
    }

    pub(crate) fn mark_failed(&self) {
        self.failed.store(true, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    /// The native client and the final connection event were observed.
    Complete,
    /// The drain did not finish before `deadline`, or `cancel` was already
    /// cancelled, so the client was force-closed.
    TimedOut,
    /// The client closing could not be confirmed.
    UnobservedClose,
}

impl Messaging {
    /// Connects and admits the source stream before the process accepts work.
    ///
    /// Publication metrics bind to the metrics recorder installed at this
    /// call, and handler metrics to the one installed at [`Self::consumer`],
    /// so install the recorder first. Each broker round trip is bounded by
    /// [`BROKER_OPERATION_BUDGET`] inside `deadline`.
    ///
    /// # Errors
    ///
    /// Returns a sanitized connection, topology, bounds, cancellation, or timeout
    /// failure when the configured dependency cannot be admitted.
    pub async fn connect(
        options: MessagingOptions,
        deadline: Instant,
        cancel: CancellationToken,
    ) -> Result<Self, MessagingError> {
        let mut startup = Self::prepare(options, deadline, cancel.clone())?;
        // Keep native connection state off every caller's future stack.
        match Box::pin(startup.admit()).await {
            Ok(messaging) => Ok(messaging),
            Err(error) => {
                let outcome = startup.close(deadline, &cancel).await;
                if outcome != CloseOutcome::Complete {
                    tracing::warn!(?outcome, "messaging_admission_close_incomplete");
                }
                Err(error)
            }
        }
    }

    /// Prepares a retained startup owner without network I/O.
    ///
    /// # Errors
    ///
    /// Returns a bounds refusal for a payload limit that is zero or cannot fit
    /// one publication within the native work window.
    pub fn prepare(
        options: MessagingOptions,
        deadline: Instant,
        cancel: CancellationToken,
    ) -> Result<MessagingStartup, MessagingError> {
        if options.max_payload_bytes == 0 {
            return Err(MessagingError::Bounds);
        }
        let envelope_limit = options
            .max_payload_bytes
            .checked_add(HEADER_LIMIT_BYTES)
            .ok_or(MessagingError::Bounds)?;
        let publication_permits = PUBLICATION_WINDOW_BYTES / envelope_limit;
        if publication_permits == 0 {
            return Err(MessagingError::Bounds);
        }
        Ok(MessagingStartup {
            options: Some(options),
            publication_permits,
            envelope_limit,
            deadline,
            cancel,
            connection: None,
        })
    }

    #[must_use]
    pub fn producer(&self) -> Producer {
        Producer {
            shared: Arc::clone(&self.shared),
        }
    }

    #[must_use]
    pub fn probe(&self) -> MessagingProbe {
        MessagingProbe {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Admits a typed registry against the configured durable consumer.
    ///
    /// # Errors
    ///
    /// Returns a sanitized error when routes, the durable configuration, or
    /// the remaining startup budget cannot admit consumption.
    pub async fn consumer(&self, registry: Registry) -> Result<Consumer, MessagingError> {
        if self.shared.refuses_work() {
            return Err(MessagingError::Unavailable);
        }
        let admission = self
            .consumer
            .clone()
            .ok_or(MessagingError::Configuration("consumer is not configured"))?;
        registry
            .validate_consumer()
            .map_err(|_| MessagingError::Configuration("no typed event handlers are registered"))?;
        Consumer::admit(Arc::clone(&self.shared), admission, registry).await
    }

    /// Drains and closes the connection, bounded by `deadline`, `cancel` and
    /// one [`BROKER_OPERATION_BUDGET`].
    ///
    /// A client already closed, with its final event observed, returns
    /// [`CloseOutcome::Complete`]. Otherwise an already-cancelled `cancel`, or
    /// a deadline that has passed, skips the drain, force-closes the native
    /// client, and returns [`CloseOutcome::TimedOut`].
    pub async fn close(self, deadline: Instant, cancel: &CancellationToken) -> CloseOutcome {
        self.shared.begin_drain();
        close_client(
            &self.shared.client,
            &self.shared.closed_event,
            within_operation_budget(deadline),
            cancel,
        )
        .await
    }
}

impl MessagingStartup {
    /// Connects and verifies topology while retaining native cleanup ownership.
    /// On success ownership transfers to the returned admitted dependency.
    ///
    /// # Errors
    ///
    /// Returns a sanitized connection, topology, cancellation or timeout failure,
    /// or a configuration refusal if this owner has already been admitted.
    pub async fn admit(&mut self) -> Result<Messaging, MessagingError> {
        let options = self.options.as_ref().ok_or(MessagingError::Configuration(
            "messaging is already admitted",
        ))?;
        let deadline = self.deadline;
        let cancel = &self.cancel;
        crate::producer::describe_metrics();
        crate::probe::describe_metrics();
        crate::connection::describe_metrics();
        crate::consumer::describe_metrics();
        crate::credentials::describe_metrics();
        // The connection is retained before topology admission, so `close`
        // owns it when admission fails.
        let (client, closed_event) = match &mut self.connection {
            Some(connection) => connection.clone(),
            slot @ None => slot
                .insert(connect_client(options, deadline, cancel).await?)
                .clone(),
        };
        let jetstream = async_nats::jetstream::context::ContextBuilder::new()
            .timeout(BROKER_OPERATION_BUDGET)
            .ack_timeout(BROKER_OPERATION_BUDGET)
            .max_ack_inflight(self.publication_permits)
            // An exhausted window refuses at once as `Rejected` instead of
            // queueing inside the adapter.
            .backpressure_on_inflight(false)
            .build(client.clone());
        let consumer = admit_topology(
            options,
            self.envelope_limit,
            &client,
            &jetstream,
            deadline,
            cancel,
        )
        .await?;
        let messaging = Messaging {
            shared: Arc::new(Shared {
                client,
                jetstream,
                source_stream: options.source_stream.clone(),
                max_payload_bytes: options.max_payload_bytes,
                envelope_limit: self.envelope_limit,
                startup_deadline: deadline,
                startup_cancel: cancel.clone(),
                draining: AtomicBool::new(false),
                failed: AtomicBool::new(false),
                publish_metrics: Outcomes::register(
                    "messaging_publish_total",
                    "messaging_publish_duration_seconds",
                    "result",
                    None,
                ),
                publish_admission_refused: metrics::counter!(
                    "messaging_publish_admission_refused_total"
                ),
                publish_work: PublishWork::register(
                    self.publication_permits,
                    &options.connection_name,
                ),
                closed_event,
            }),
            consumer,
        };
        self.connection = None;
        self.options = None;
        Ok(messaging)
    }

    /// Drains any retained native client and observes native and final-event completion.
    ///
    /// A client already closed, with its final event observed, returns
    /// [`CloseOutcome::Complete`]. Otherwise an already-cancelled `cancel`, or
    /// a deadline that has passed, skips the drain, force-closes the native
    /// client, and returns [`CloseOutcome::TimedOut`].
    pub async fn close(self, deadline: Instant, cancel: &CancellationToken) -> CloseOutcome {
        let Some((client, closed_event)) = self.connection else {
            return CloseOutcome::Complete;
        };
        close_client(
            &client,
            &closed_event,
            within_operation_budget(deadline),
            cancel,
        )
        .await
    }
}

/// Runs one broker operation within [`within_operation_budget`] of `deadline`,
/// unless `cancel` fires first.
pub(crate) async fn bounded_operation<T>(
    deadline: Instant,
    cancel: &CancellationToken,
    operation: impl Future<Output = Result<T, MessagingError>>,
) -> Result<T, MessagingError> {
    let deadline = within_operation_budget(deadline);
    let budget = deadline.saturating_duration_since(Instant::now());
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(MessagingError::Cancelled),
        () = tokio::time::sleep_until(deadline) => Err(MessagingError::TimedOut { budget }),
        result = operation => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalid_publication_bounds_are_rejected_before_connecting() {
        for max_payload_bytes in [0, 64 * 1024 * 1024 - 8192 + 1, usize::MAX] {
            let result = Messaging::connect(
                MessagingOptions {
                    connection_name: "invalid-bounds".to_owned(),
                    servers: vec!["not a server URL".to_owned()],
                    credentials: None,
                    root_ca_path: None,
                    allow_plaintext: true,
                    tls_first: false,
                    source_stream: "SOURCE".to_owned(),
                    max_payload_bytes,
                    consumer: None,
                },
                Instant::now() + BROKER_OPERATION_BUDGET,
                CancellationToken::new(),
            )
            .await;
            assert!(matches!(result, Err(MessagingError::Bounds)));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_operation_reports_the_shorter_caller_deadline() {
        let caller = Duration::from_millis(100);
        let result = bounded_operation(
            Instant::now() + caller,
            &CancellationToken::new(),
            std::future::pending::<Result<(), MessagingError>>(),
        )
        .await;
        assert!(
            matches!(result, Err(MessagingError::TimedOut { budget }) if budget == caller),
            "{result:?}"
        );
    }
}
