use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_nats::ConnectErrorKind;
use async_nats::jetstream::context::{
    GetStreamByNameErrorKind, GetStreamError, GetStreamErrorKind,
};
use health::{Probe, ProbeError};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::consumer::Consumer;
use crate::error::MessagingError;
use crate::producer::Producer;
use crate::registry::Registry;
use crate::wire::HEADER_LIMIT_BYTES;

pub(crate) const BROKER_OPERATION_BUDGET: Duration = Duration::from_secs(5);

/// A count and a duration per outcome label, registered once with the
/// recorder installed at connect. Emitting through the macros instead looks
/// each metric up and allocates its labels on every publish and delivery.
pub(crate) struct Outcomes<const N: usize>([(metrics::Counter, metrics::Histogram); N]);

impl<const N: usize> Outcomes<N> {
    fn register(
        total: &'static str,
        duration: &'static str,
        label: &'static str,
        values: [&'static str; N],
    ) -> Self {
        Self(values.map(|value| {
            (
                metrics::counter!(total, label => value),
                metrics::histogram!(duration, label => value),
            )
        }))
    }

    /// Records one outcome by its index in the registered label values.
    pub(crate) fn record(&self, outcome: usize, elapsed: Duration) {
        let (total, duration) = &self.0[outcome];
        total.increment(1);
        duration.record(elapsed.as_secs_f64());
    }
}

impl<const N: usize> std::fmt::Debug for Outcomes<N> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Outcomes")
    }
}

/// Configuration already validated by the composition root, which owns the
/// transport, credential and resident-memory policy.
#[derive(Clone)]
pub struct MessagingOptions {
    /// Shown by the broker's connection reports, so an operator can tell
    /// this process's connection from its neighbors'.
    pub connection_name: String,
    pub servers: Vec<String>,
    pub credentials: Option<SecretString>,
    pub root_ca_path: Option<PathBuf>,
    pub allow_plaintext: bool,
    pub source_stream: String,
    pub dlq_stream: Option<String>,
    pub max_payload_bytes: usize,
    pub consumer: Option<ConsumerOptions>,
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
            .field("source_stream", &self.source_stream)
            .field("dlq_stream", &self.dlq_stream)
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
    pub concurrency: usize,
}

/// One connected messaging dependency and its lifecycle state.
#[derive(Clone, Debug)]
pub struct Messaging {
    pub(crate) shared: Arc<Shared>,
    consumer: Option<ConsumerOptions>,
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) client: async_nats::Client,
    pub(crate) jetstream: async_nats::jetstream::Context,
    pub(crate) source_stream: String,
    pub(crate) dlq_stream: Option<String>,
    pub(crate) max_payload_bytes: usize,
    pub(crate) startup_deadline: Instant,
    pub(crate) startup_cancel: CancellationToken,
    pub(crate) draining: AtomicBool,
    pub(crate) failed: AtomicBool,
    /// Indexed like `producer::PUBLISH_RESULTS`.
    pub(crate) publish_metrics: Outcomes<3>,
    /// Indexed by `consumer::Outcome`.
    pub(crate) handler_metrics: Outcomes<5>,
    closed: watch::Receiver<bool>,
}

/// A readiness probe whose I/O runs only in health's background refresher.
#[derive(Clone, Debug)]
pub struct MessagingProbe {
    shared: Arc<Shared>,
}

/// Dependency-close result for the process shutdown owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    Complete,
    TimedOut,
    UnobservedClose,
}

impl Messaging {
    /// Connects and admits the source stream before the process accepts work.
    ///
    /// Publication and handler metrics bind to the metrics recorder installed
    /// at this call, so install the recorder first.
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
        if options
            .consumer
            .as_ref()
            .is_some_and(|consumer| consumer.concurrency == 0)
        {
            return Err(MessagingError::Bounds);
        }
        let (closed_tx, closed) = watch::channel(false);
        let mut connect = async_nats::ConnectOptions::new()
            .name(&options.connection_name)
            .connection_timeout(BROKER_OPERATION_BUDGET)
            .request_timeout(Some(BROKER_OPERATION_BUDGET))
            .require_tls(!options.allow_plaintext)
            .event_callback(move |event| {
                let outcome = match event {
                    async_nats::Event::Connected => "connected",
                    async_nats::Event::Disconnected => "disconnected",
                    async_nats::Event::Draining => "draining",
                    async_nats::Event::Closed => {
                        closed_tx.send_replace(true);
                        "closed"
                    }
                    async_nats::Event::LameDuckMode => "lame_duck",
                    async_nats::Event::SlowConsumer(_) => "slow_consumer",
                    async_nats::Event::ServerError(_) => "server_error",
                    async_nats::Event::ClientError(_) => "client_error",
                };
                metrics::counter!("messaging_connection_events_total", "result" => outcome)
                    .increment(1);
                std::future::ready(())
            });
        if let Some(credentials) = &options.credentials {
            connect = connect
                .credentials(credentials.expose_secret())
                .map_err(|_| MessagingError::Authentication)?;
        }
        if let Some(root_ca) = options.root_ca_path.clone() {
            connect = connect.add_root_certificates(root_ca);
        }
        let client = admission(deadline, &cancel, async {
            connect
                .connect(options.servers.clone())
                .await
                .map_err(|error| classify_connect(&error))
        })
        .await?;
        let jetstream = async_nats::jetstream::context::ContextBuilder::new()
            .timeout(BROKER_OPERATION_BUDGET)
            .ack_timeout(BROKER_OPERATION_BUDGET)
            .build(client.clone());
        let topology = admit_topology(&options, &client, &jetstream, deadline, &cancel).await;
        let dlq_stream = match topology {
            Ok(dlq_stream) => dlq_stream,
            Err(error) => {
                let _ = close_client(
                    &client,
                    &closed,
                    deadline.min(Instant::now() + BROKER_OPERATION_BUDGET),
                    &cancel,
                )
                .await;
                return Err(error);
            }
        };
        Ok(Self {
            shared: Arc::new(Shared {
                client,
                jetstream,
                source_stream: options.source_stream,
                dlq_stream,
                max_payload_bytes: options.max_payload_bytes,
                startup_deadline: deadline,
                startup_cancel: cancel,
                draining: AtomicBool::new(false),
                failed: AtomicBool::new(false),
                publish_metrics: Outcomes::register(
                    "messaging_publish_total",
                    "messaging_publish_duration_seconds",
                    "result",
                    crate::producer::PUBLISH_RESULTS,
                ),
                handler_metrics: Outcomes::register(
                    "messaging_handler_total",
                    "messaging_handler_duration_seconds",
                    "outcome",
                    crate::consumer::OUTCOME_LABELS,
                ),
                closed,
            }),
            consumer: options.consumer,
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
        if self.shared.draining.load(Ordering::Acquire)
            || self.shared.failed.load(Ordering::Acquire)
        {
            return Err(MessagingError::Draining);
        }
        let options = self
            .consumer
            .clone()
            .ok_or(MessagingError::Configuration("consumer is not configured"))?;
        registry
            .validate_consumer()
            .map_err(|_| MessagingError::Configuration("no typed event handlers are registered"))?;
        Consumer::admit(Arc::clone(&self.shared), options, registry).await
    }

    pub async fn close(self, deadline: Instant, cancel: &CancellationToken) -> CloseOutcome {
        self.shared.draining.store(true, Ordering::Release);
        close_client(
            &self.shared.client,
            &self.shared.closed,
            deadline.min(Instant::now() + BROKER_OPERATION_BUDGET),
            cancel,
        )
        .await
    }
}

#[async_trait::async_trait]
impl Probe for MessagingProbe {
    fn name(&self) -> &'static str {
        "messaging"
    }

    async fn check(&self) -> Result<(), ProbeError> {
        if self.shared.draining.load(Ordering::Acquire) {
            return Err(ProbeError::new("messaging is draining"));
        }
        if self.shared.failed.load(Ordering::Acquire) {
            return Err(ProbeError::new("messaging consumer failed"));
        }
        if *self.shared.closed.borrow()
            || !matches!(
                self.shared.client.connection_state(),
                async_nats::connection::State::Connected
            )
        {
            return Err(ProbeError::new("messaging connection is unavailable"));
        }
        validate_server(
            &self.shared.client,
            self.shared.max_payload_bytes + HEADER_LIMIT_BYTES,
        )
        .map_err(|_| ProbeError::new("messaging server admission is unavailable"))
    }
}

async fn close_client(
    client: &async_nats::Client,
    closed: &watch::Receiver<bool>,
    deadline: Instant,
    cancel: &CancellationToken,
) -> CloseOutcome {
    let mut closed = closed.clone();
    if *closed.borrow_and_update() {
        return CloseOutcome::Complete;
    }
    if cancel.is_cancelled() || Instant::now() >= deadline {
        return CloseOutcome::TimedOut;
    }
    let drain = async {
        if client.drain().await.is_err() {
            return if *closed.borrow() {
                CloseOutcome::Complete
            } else {
                CloseOutcome::UnobservedClose
            };
        }
        loop {
            if *closed.borrow_and_update() {
                return CloseOutcome::Complete;
            }
            if closed.changed().await.is_err() {
                return CloseOutcome::UnobservedClose;
            }
        }
    };
    tokio::select! {
        biased;
        () = cancel.cancelled() => CloseOutcome::TimedOut,
        () = tokio::time::sleep_until(deadline) => CloseOutcome::TimedOut,
        result = drain => result,
    }
}

async fn admission<T>(
    deadline: Instant,
    cancel: &CancellationToken,
    operation: impl Future<Output = Result<T, MessagingError>>,
) -> Result<T, MessagingError> {
    let deadline = deadline.min(Instant::now() + BROKER_OPERATION_BUDGET);
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(MessagingError::Cancelled),
        () = tokio::time::sleep_until(deadline) => Err(MessagingError::TimedOut { budget: BROKER_OPERATION_BUDGET }),
        result = operation => result,
    }
}

/// Admits the source stream and, for a consumer, its dead-letter stream.
/// Returns the dead-letter stream name.
async fn admit_topology(
    options: &MessagingOptions,
    client: &async_nats::Client,
    jetstream: &async_nats::jetstream::Context,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<Option<String>, MessagingError> {
    let envelope_limit = options.max_payload_bytes + HEADER_LIMIT_BYTES;
    validate_server(client, envelope_limit)?;
    let source = get_stream(jetstream, &options.source_stream, deadline, cancel).await?;
    let Some(consumer) = &options.consumer else {
        return Ok(None);
    };
    // The stream's own message limit bounds what one delivery can hold in memory.
    let source_limit = source.cached_info().config.max_message_size;
    if source_limit <= 0
        || usize::try_from(source_limit).map_err(|_| MessagingError::Bounds)? > envelope_limit
    {
        return Err(MessagingError::Bounds);
    }
    let dlq_name = match &options.dlq_stream {
        Some(name) => name.clone(),
        None => {
            admission(deadline, cancel, async {
                jetstream
                    .stream_by_subject(consumer.dlq_subject.clone())
                    .await
                    .map_err(|error| {
                        let broker = match error.kind() {
                            GetStreamByNameErrorKind::JetStream(broker) => Some(broker),
                            _ => None,
                        };
                        topology_failure("stream_by_subject", &error, broker.as_ref())
                    })
            })
            .await?
        }
    };
    if dlq_name == options.source_stream {
        return Err(MessagingError::Topology);
    }
    get_stream(jetstream, &dlq_name, deadline, cancel).await?;
    Ok(Some(dlq_name))
}

async fn get_stream(
    jetstream: &async_nats::jetstream::Context,
    name: &str,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<async_nats::jetstream::stream::Stream, MessagingError> {
    admission(deadline, cancel, async {
        jetstream
            .get_stream(name)
            .await
            .map_err(|error| get_stream_failure(&error))
    })
    .await
}

pub(crate) fn get_stream_failure(error: &GetStreamError) -> MessagingError {
    let broker = match error.kind() {
        GetStreamErrorKind::JetStream(broker) => Some(broker),
        _ => None,
    };
    topology_failure("get_stream", error, broker.as_ref())
}

/// Classifies a failed topology request and logs which request failed.
///
/// `broker` is the `JetStream` API error when the broker answered with one.
/// Its numeric code is a closed vocabulary; its description can quote
/// configuration and stays out of the log.
pub(crate) fn topology_failure(
    operation: &'static str,
    error: &(dyn std::error::Error + 'static),
    broker: Option<&async_nats::jetstream::Error>,
) -> MessagingError {
    let failure = match broker {
        Some(broker) if matches!(broker.code(), 401 | 403) => MessagingError::Authentication,
        Some(_) => MessagingError::Topology,
        None => classify_topology(error),
    };
    tracing::warn!(
        operation,
        reason = %failure,
        jetstream.error_code = broker.map(|broker| broker.error_code().0),
        "messaging_admission_failed"
    );
    failure
}

fn validate_server(
    client: &async_nats::Client,
    envelope_limit: usize,
) -> Result<(), MessagingError> {
    let info = client.server_info();
    if !client.is_server_compatible(2, 12, 3) || !info.jetstream || !info.headers {
        return Err(MessagingError::Topology);
    }
    if info.max_payload < envelope_limit {
        return Err(MessagingError::Bounds);
    }
    Ok(())
}

fn classify_topology(error: &(dyn std::error::Error + 'static)) -> MessagingError {
    if let Some(error) = error.downcast_ref::<async_nats::jetstream::context::RequestError>() {
        match error.kind() {
            async_nats::jetstream::context::RequestErrorKind::TimedOut => {
                return MessagingError::TimedOut {
                    budget: BROKER_OPERATION_BUDGET,
                };
            }
            async_nats::jetstream::context::RequestErrorKind::InvalidSubject => {
                return MessagingError::Configuration("broker subject is invalid");
            }
            _ => {}
        }
    }
    if let Some(error) = error.downcast_ref::<async_nats::jetstream::Error>()
        && matches!(error.code(), 401 | 403)
    {
        return MessagingError::Authentication;
    }
    error
        .source()
        .map_or(MessagingError::Topology, classify_topology)
}

fn classify_connect(error: &async_nats::ConnectError) -> MessagingError {
    match error.kind() {
        ConnectErrorKind::Authentication | ConnectErrorKind::AuthorizationViolation => {
            MessagingError::Authentication
        }
        ConnectErrorKind::TimedOut => MessagingError::TimedOut {
            budget: BROKER_OPERATION_BUDGET,
        },
        ConnectErrorKind::ServerParse => MessagingError::Configuration("server URL is invalid"),
        ConnectErrorKind::Dns
        | ConnectErrorKind::Tls
        | ConnectErrorKind::Io
        | ConnectErrorKind::MaxReconnects => MessagingError::Connection,
    }
}
