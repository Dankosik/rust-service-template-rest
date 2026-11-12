use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use secrecy::SecretString;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::budget::BROKER_OPERATION_BUDGET;
use crate::connection::NativeConnection;
use crate::consumer::Consumer;
use crate::error::MessagingError;
use crate::probe::{MessagingProbe, PublishWork};
use crate::producer::{Producer, PublishMetrics};
use crate::registry::Registry;
use crate::topology::{ConsumerAdmission, admit_topology};
use crate::wire::HEADER_LIMIT_BYTES;

/// One native publication window shared by source, outbox and dead-letter
/// publication; its permits are `floor(window / envelope_limit)`.
const PUBLICATION_WINDOW_BYTES: usize = 64 * 1024 * 1024;

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
#[derive(Clone, Debug)]
pub enum Credentials {
    /// The content of a credentials file. `Debug` redacts it.
    Inline(SecretString),
    /// A credentials file read again for every connection, for a platform
    /// that rotates it.
    File(PathBuf),
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
    options: MessagingOptions,
    publication_permits: usize,
    envelope_limit: usize,
    deadline: Instant,
    cancel: CancellationToken,
    phase: StartupPhase,
}

/// How far a [`MessagingStartup`] got.
#[derive(Debug)]
enum StartupPhase {
    /// No connection was opened yet.
    Prepared,
    /// The connection is open and owned here until admission succeeds or
    /// [`MessagingStartup::close`] closes it.
    Connected(NativeConnection),
    /// Ownership moved to the admitted [`Messaging`].
    Admitted,
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) connection: NativeConnection,
    pub(crate) jetstream: async_nats::jetstream::Context,
    pub(crate) source_stream: String,
    pub(crate) max_payload_bytes: usize,
    /// The largest encoded envelope the configured payload limit admits.
    pub(crate) envelope_limit: usize,
    draining: AtomicBool,
    failed: AtomicBool,
    pub(crate) publish_metrics: PublishMetrics,
    pub(crate) publish_work: PublishWork,
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
            options,
            publication_permits,
            envelope_limit,
            deadline,
            cancel,
            phase: StartupPhase::Prepared,
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
            .require_handlers()
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
        self.shared.connection.close(deadline, cancel).await
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
        let options = &self.options;
        let deadline = self.deadline;
        let cancel = &self.cancel;
        crate::producer::describe_metrics();
        crate::probe::describe_metrics();
        crate::connection::describe_metrics();
        crate::consumer::describe_metrics();
        crate::credentials::describe_metrics();
        // The connection is retained before topology admission, so `close`
        // owns it when admission fails.
        let connection = match &mut self.phase {
            StartupPhase::Admitted => {
                return Err(MessagingError::Configuration(
                    "messaging is already admitted",
                ));
            }
            StartupPhase::Connected(connection) => connection.clone(),
            phase @ StartupPhase::Prepared => {
                let connection = NativeConnection::open(options, deadline, cancel).await?;
                *phase = StartupPhase::Connected(connection.clone());
                connection
            }
        };
        let client = &connection.client;
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
            client,
            &jetstream,
            deadline,
            cancel,
        )
        .await?;
        let messaging = Messaging {
            shared: Arc::new(Shared {
                jetstream,
                source_stream: options.source_stream.clone(),
                max_payload_bytes: options.max_payload_bytes,
                envelope_limit: self.envelope_limit,
                draining: AtomicBool::new(false),
                failed: AtomicBool::new(false),
                publish_metrics: PublishMetrics::register(),
                publish_work: PublishWork::register(
                    self.publication_permits,
                    &options.connection_name,
                ),
                connection,
            }),
            consumer,
        };
        self.phase = StartupPhase::Admitted;
        Ok(messaging)
    }

    /// Drains any retained native client and observes native and final-event completion.
    ///
    /// A client already closed, with its final event observed, returns
    /// [`CloseOutcome::Complete`]. Otherwise an already-cancelled `cancel`, or
    /// a deadline that has passed, skips the drain, force-closes the native
    /// client, and returns [`CloseOutcome::TimedOut`].
    pub async fn close(self, deadline: Instant, cancel: &CancellationToken) -> CloseOutcome {
        match self.phase {
            StartupPhase::Connected(connection) => connection.close(deadline, cancel).await,
            StartupPhase::Prepared | StartupPhase::Admitted => CloseOutcome::Complete,
        }
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
            std::assert_matches!(result, Err(MessagingError::Bounds));
        }
    }
}
