use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_nats::ConnectErrorKind;
use health::{Probe, ProbeError};
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::consumer::Consumer;
use crate::error::MessagingError;
use crate::producer::Producer;
use crate::registry::Registry;
use crate::wire::{
    HEADER_LIMIT_BYTES, filter_covers, subject_matches, valid_filter, valid_subject,
};

pub(crate) const BROKER_OPERATION_BUDGET: Duration = Duration::from_secs(5);
const RESIDENT_DELIVERY_LIMIT: usize = 64 * 1024 * 1024;

/// Configuration already validated by the composition root.
#[derive(Clone)]
pub struct MessagingOptions {
    pub servers: Vec<String>,
    pub credentials: Option<String>,
    pub root_ca_path: Option<PathBuf>,
    pub allow_plaintext: bool,
    pub allow_unauthenticated: bool,
    pub source_stream: String,
    pub dlq_stream: Option<String>,
    pub max_payload_bytes: usize,
    pub consumer: Option<ConsumerOptions>,
}

impl std::fmt::Debug for MessagingOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessagingOptions")
            .field("server_count", &self.servers.len())
            .field(
                "credentials",
                &self.credentials.as_ref().map(|_| "redacted"),
            )
            .field("root_ca_path", &self.root_ca_path)
            .field("allow_plaintext", &self.allow_plaintext)
            .field("allow_unauthenticated", &self.allow_unauthenticated)
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
    pub(crate) source_subjects: Vec<String>,
    pub(crate) dlq_stream: Option<String>,
    pub(crate) source_max_message_size: usize,
    pub(crate) dlq_max_message_size: usize,
    pub(crate) max_payload_bytes: usize,
    pub(crate) startup_deadline: Instant,
    pub(crate) startup_cancel: CancellationToken,
    pub(crate) draining: AtomicBool,
    pub(crate) failed: AtomicBool,
    admitted_consumer: RwLock<Option<String>>,
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
    /// # Errors
    ///
    /// Returns a sanitized connection, topology, bounds, cancellation, or timeout
    /// failure when the configured dependency cannot be admitted.
    pub async fn connect(
        options: MessagingOptions,
        deadline: Instant,
        cancel: CancellationToken,
    ) -> Result<Self, MessagingError> {
        validate_options(&options)?;
        let (closed_tx, closed) = watch::channel(false);
        let capacity = options
            .consumer
            .as_ref()
            .map_or(1, |consumer| consumer.concurrency);
        let mut connect = async_nats::ConnectOptions::new()
            .connection_timeout(BROKER_OPERATION_BUDGET)
            .request_timeout(Some(BROKER_OPERATION_BUDGET))
            .subscription_capacity(capacity)
            .client_capacity(capacity)
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
        if let Some(credentials) = options.credentials.as_deref() {
            connect = connect
                .credentials(credentials)
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
            .max_ack_inflight(capacity)
            .build(client.clone());
        let topology = admit_topology(&options, &client, &jetstream, deadline, &cancel).await;
        let (source_subjects, source_max_message_size, dlq_stream, dlq_max_message_size) =
            match topology {
                Ok(topology) => topology,
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
                source_subjects,
                dlq_stream,
                source_max_message_size,
                dlq_max_message_size,
                max_payload_bytes: options.max_payload_bytes,
                startup_deadline: deadline,
                startup_cancel: cancel,
                draining: AtomicBool::new(false),
                failed: AtomicBool::new(false),
                admitted_consumer: RwLock::new(None),
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
        if registry.subjects().any(|subject| {
            !self
                .shared
                .source_subjects
                .iter()
                .any(|filter| subject_matches(filter, subject))
        }) {
            return Err(MessagingError::Topology);
        }
        let name = options.durable_name.clone();
        let consumer = Consumer::admit(Arc::clone(&self.shared), options, registry).await?;
        *self
            .shared
            .admitted_consumer
            .write()
            .map_err(|_| MessagingError::Closed)? = Some(name);
        Ok(consumer)
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
        .map_err(|_| ProbeError::new("messaging server admission is unavailable"))?;
        let consumer_name = self
            .shared
            .admitted_consumer
            .read()
            .map_err(|_| ProbeError::new("messaging consumer admission is unavailable"))?
            .clone();
        let check = async {
            let stream = self
                .shared
                .jetstream
                .get_stream(&self.shared.source_stream)
                .await
                .map_err(|_| ProbeError::new("messaging source topology is unavailable"))?;
            if let Some(name) = consumer_name {
                stream
                    .consumer_info(name)
                    .await
                    .map_err(|_| ProbeError::new("messaging consumer topology is unavailable"))?;
            }
            Ok(())
        };
        tokio::time::timeout(BROKER_OPERATION_BUDGET, check)
            .await
            .map_err(|_| ProbeError::new("messaging topology check timed out"))?
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

async fn admit_topology(
    options: &MessagingOptions,
    client: &async_nats::Client,
    jetstream: &async_nats::jetstream::Context,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<(Vec<String>, usize, Option<String>, usize), MessagingError> {
    let envelope_limit = options.max_payload_bytes + HEADER_LIMIT_BYTES;
    validate_server(client, envelope_limit)?;
    let source = admission(deadline, cancel, async {
        jetstream
            .get_stream(&options.source_stream)
            .await
            .map_err(|error| classify_topology(&error))
    })
    .await?;
    let source_config = &source.cached_info().config;
    let source_max = admit_stream(source_config, client.max_payload(), envelope_limit)?;
    let source_subjects = source_config.subjects.clone();
    let Some(consumer) = &options.consumer else {
        return Ok((source_subjects, source_max, None, 0));
    };
    if source_config.max_message_size <= 0
        || usize::try_from(source_config.max_message_size).map_err(|_| MessagingError::Bounds)?
            > envelope_limit
    {
        return Err(MessagingError::Bounds);
    }
    if !source_subjects
        .iter()
        .any(|subject| filter_covers(subject, &consumer.filter_subject))
    {
        return Err(MessagingError::Topology);
    }
    let dlq_name = match &options.dlq_stream {
        Some(name) => name.clone(),
        None => {
            admission(deadline, cancel, async {
                jetstream
                    .stream_by_subject(consumer.dlq_subject.clone())
                    .await
                    .map_err(|error| classify_topology(&error))
            })
            .await?
        }
    };
    if dlq_name == options.source_stream {
        return Err(MessagingError::Topology);
    }
    let dlq = admission(deadline, cancel, async {
        jetstream
            .get_stream(&dlq_name)
            .await
            .map_err(|error| classify_topology(&error))
    })
    .await?;
    let dlq_config = &dlq.cached_info().config;
    let dlq_max = admit_stream(dlq_config, client.max_payload(), envelope_limit)?;
    if !dlq_config
        .subjects
        .iter()
        .any(|filter| subject_matches(filter, &consumer.dlq_subject))
    {
        return Err(MessagingError::Topology);
    }
    Ok((source_subjects, source_max, Some(dlq_name), dlq_max))
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

fn admit_stream(
    config: &async_nats::jetstream::stream::Config,
    server_max: usize,
    envelope_limit: usize,
) -> Result<usize, MessagingError> {
    if config.no_ack || config.sealed || config.subjects.is_empty() {
        return Err(MessagingError::Topology);
    }
    let maximum = if config.max_message_size > 0 {
        usize::try_from(config.max_message_size).map_err(|_| MessagingError::Bounds)?
    } else {
        server_max
    };
    if maximum < envelope_limit {
        return Err(MessagingError::Bounds);
    }
    Ok(maximum.min(server_max))
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

fn validate_options(options: &MessagingOptions) -> Result<(), MessagingError> {
    if options.servers.is_empty()
        || options.source_stream.is_empty()
        || options.max_payload_bytes == 0
    {
        return Err(MessagingError::Configuration(
            "servers, source stream and payload bound are required",
        ));
    }
    if options
        .max_payload_bytes
        .checked_add(HEADER_LIMIT_BYTES)
        .is_none()
    {
        return Err(MessagingError::Bounds);
    }
    if options.credentials.is_none() && !options.allow_unauthenticated {
        return Err(MessagingError::Configuration("credentials are required"));
    }
    for server in &options.servers {
        let address = server
            .parse::<async_nats::ServerAddr>()
            .map_err(|_| MessagingError::Configuration("server URL is invalid"))?;
        if address.host().is_empty()
            || address.username().is_some()
            || address.password().is_some()
            || !matches!(address.scheme(), "nats" | "tls")
            || (address.scheme() == "nats" && !options.allow_plaintext)
        {
            return Err(MessagingError::Configuration(
                "server URL violates transport policy",
            ));
        }
    }
    if let Some(consumer) = &options.consumer
        && (consumer.concurrency == 0
            || consumer.durable_name.is_empty()
            || !valid_filter(&consumer.filter_subject)
            || !valid_subject(&consumer.dlq_subject)
            || subject_matches(&consumer.filter_subject, &consumer.dlq_subject)
            || consumer
                .concurrency
                .saturating_mul(options.max_payload_bytes.saturating_add(8 * 1024))
                > RESIDENT_DELIVERY_LIMIT)
    {
        return Err(MessagingError::Bounds);
    }
    Ok(())
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
