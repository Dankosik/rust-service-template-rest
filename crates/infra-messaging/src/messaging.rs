use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::jetstream::context::{
    GetStreamByNameErrorKind, GetStreamError, GetStreamErrorKind,
};
use async_nats::jetstream::stream::{Config as StreamConfig, PersistenceMode, StorageType};
use async_nats::rustls::crypto::{GetRandomFailed, SecureRandom};
use async_nats::{ConnectErrorKind, ToServerAddrs as _};
use futures_util::FutureExt as _;
use health::{Probe, ProbeError};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::consumer::Consumer;
use crate::credentials::{CredentialsFile, CredentialsFileError};
use crate::error::MessagingError;
use crate::producer::Producer;
use crate::registry::Registry;
use crate::wire::{self, HEADER_LIMIT_BYTES};

pub(crate) const BROKER_OPERATION_BUDGET: Duration = Duration::from_secs(5);
const PUBLICATION_WINDOW_BYTES: usize = 64 * 1024 * 1024;

/// A count and a duration per outcome label, registered once with the
/// installed recorder. Emitting through the macros instead looks each metric
/// up and allocates its labels on every publish and delivery.
pub(crate) struct Outcomes<const N: usize>([(metrics::Counter, metrics::Histogram); N]);

impl<const N: usize> Outcomes<N> {
    /// `event_type`, when given, labels every outcome with that event type.
    pub(crate) fn register(
        total: &'static str,
        duration: &'static str,
        label: &'static str,
        values: [&'static str; N],
        event_type: Option<&'static str>,
    ) -> Self {
        Self(values.map(|value| {
            let mut labels = vec![metrics::Label::from_static_parts(label, value)];
            if let Some(event_type) = event_type {
                labels.push(metrics::Label::from_static_parts("event_type", event_type));
            }
            (
                metrics::counter!(total, labels.clone()),
                metrics::histogram!(duration, labels),
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
    /// A credentials file read again for every connection, the alternative
    /// to `credentials` for a platform that rotates it.
    pub credentials_file: Option<PathBuf>,
    pub root_ca_path: Option<PathBuf>,
    pub allow_plaintext: bool,
    /// Authenticate TLS before accepting server INFO and discovered destinations.
    pub tls_first: bool,
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
            .field("credentials_file", &self.credentials_file)
            .field("root_ca_path", &self.root_ca_path)
            .field("allow_plaintext", &self.allow_plaintext)
            .field("tls_first", &self.tls_first)
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

/// Retains a partially admitted native connection until it can be transferred
/// to [`Messaging`] or explicitly closed by the process owner.
#[derive(Debug)]
pub struct MessagingStartup {
    options: Option<MessagingOptions>,
    publication_limit: usize,
    deadline: Instant,
    cancel: CancellationToken,
    connection: Option<(async_nats::Client, watch::Receiver<bool>)>,
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) client: async_nats::Client,
    pub(crate) jetstream: async_nats::jetstream::Context,
    pub(crate) source_stream: String,
    pub(crate) dead_letter: Option<DeadLetterTopology>,
    pub(crate) max_payload_bytes: usize,
    pub(crate) startup_deadline: Instant,
    pub(crate) startup_cancel: CancellationToken,
    pub(crate) draining: AtomicBool,
    pub(crate) failed: AtomicBool,
    /// Indexed like `producer::PUBLISH_RESULTS`.
    pub(crate) publish_metrics: Outcomes<3>,
    pub(crate) publish_admission_refused: metrics::Counter,
    publish_work: PublishWork,
    /// Final event submission is separate from native runner completion.
    closed_event: watch::Receiver<bool>,
}

/// The topology already read at startup; consumer admission adds route lengths.
#[derive(Debug)]
pub(crate) struct DeadLetterTopology {
    pub(crate) stream: String,
    source_limit: usize,
    message_limit: i32,
}

impl DeadLetterTopology {
    pub(crate) fn admit(
        &self,
        registry: &Registry,
        filter: &str,
        server_limit: usize,
    ) -> Result<usize, MessagingError> {
        let subject_bytes = registry
            .routed()
            .filter(|(_, subject, _)| wire::subject_matches(filter, subject))
            .map(|(_, subject, _)| subject.len())
            .max()
            .ok_or_else(|| limit_failure("dead_letter_stream", Refusal::TransferBounds, 0, None))?;
        let bounds = wire::dead_letter_bounds(self.source_limit, subject_bytes, self.stream.len())
            .ok_or_else(|| limit_failure("dead_letter_stream", Refusal::TransferBounds, 0, None))?;
        if bounds.headers > wire::NATIVE_HEADER_LIMIT_BYTES {
            return Err(limit_failure(
                "dead_letter_stream",
                Refusal::TransferHeaderSize,
                bounds.headers,
                i64::try_from(wire::NATIVE_HEADER_LIMIT_BYTES).ok(),
            ));
        }
        if bounds.total > server_limit {
            return Err(limit_failure(
                "server",
                Refusal::ServerMaxPayload,
                bounds.total,
                i64::try_from(server_limit).ok(),
            ));
        }
        if self.message_limit > 0
            && usize::try_from(self.message_limit).is_ok_and(|limit| limit < bounds.total)
        {
            return Err(limit_failure(
                "dead_letter_stream",
                Refusal::StreamMessageSize,
                bounds.total,
                Some(i64::from(self.message_limit)),
            ));
        }
        Ok(subject_bytes)
    }
}

/// Timestamped native permit occupancy, sampled only by the existing probe.
#[derive(Debug)]
struct PublishWork {
    in_flight: metrics::Gauge,
    last_observed: metrics::Gauge,
}

impl PublishWork {
    fn register(capacity: usize) -> Self {
        let in_flight = metrics::gauge!("messaging_publish_work_in_flight");
        let last_observed =
            metrics::gauge!("messaging_publish_work_last_observed_timestamp_seconds");
        in_flight.set(f64::NAN);
        last_observed.set(0.0);
        #[expect(
            clippy::cast_precision_loss,
            reason = "capacity is bounded by the 64 MiB window"
        )]
        metrics::gauge!("messaging_publish_capacity").set(capacity as f64);
        Self {
            in_flight,
            last_observed,
        }
    }

    fn observe(&self, context: &async_nats::jetstream::Context) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "occupancy is bounded by publication capacity"
        )]
        self.in_flight.set(context.in_flight_publishes() as f64);
        self.last_observed.set(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(f64::NAN, |elapsed| elapsed.as_secs_f64()),
        );
    }
}

/// A readiness probe whose I/O runs only in health's background refresher.
///
/// Each check passively records native publication permits, including abandoned
/// ACK cleanup, before any health refusal. `messaging_publish_work_in_flight`
/// is NaN and its observation timestamp is zero until the first check. Treat
/// stale, future or invalid timestamps and failed scrapes as unknown occupancy.
/// This is a dated snapshot, not a continuously live count or delivery proof;
/// `messaging_publish_capacity` is the fixed native admission limit.
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
    /// Publication metrics bind to the metrics recorder installed at this
    /// call, and handler metrics to the one installed at [`Self::consumer`],
    /// so install the recorder first.
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
    /// Returns a bounds refusal for zero consumer concurrency or a payload limit
    /// that cannot fit one publication within the native work window.
    pub fn prepare(
        options: MessagingOptions,
        deadline: Instant,
        cancel: CancellationToken,
    ) -> Result<MessagingStartup, MessagingError> {
        if options
            .consumer
            .as_ref()
            .is_some_and(|consumer| consumer.concurrency == 0)
        {
            return Err(MessagingError::Bounds);
        }
        let envelope_limit = options
            .max_payload_bytes
            .checked_add(HEADER_LIMIT_BYTES)
            .filter(|_| options.max_payload_bytes > 0)
            .ok_or(MessagingError::Bounds)?;
        let publication_limit = PUBLICATION_WINDOW_BYTES / envelope_limit;
        if publication_limit == 0 {
            return Err(MessagingError::Bounds);
        }
        Ok(MessagingStartup {
            options: Some(options),
            publication_limit,
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
            &self.shared.closed_event,
            deadline.min(Instant::now() + BROKER_OPERATION_BUDGET),
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
        let (client, closed_event) = match &mut self.connection {
            Some(connection) => connection.clone(),
            slot @ None => {
                let (closed_event_tx, closed_event) = watch::channel(false);
                let servers: Vec<_> = options
                    .servers
                    .to_server_addrs()
                    .map_err(|_| MessagingError::Configuration("broker server address is invalid"))?
                    .collect();
                let all_tls = servers.iter().all(async_nats::ServerAddr::tls_required);
                if options.tls_first && !all_tls {
                    return Err(MessagingError::Configuration(
                        "TLS-first requires every configured server to use TLS",
                    ));
                }
                let any_tls = servers.iter().any(async_nats::ServerAddr::tls_required);
                let connect_deadline = deadline.min(Instant::now() + BROKER_OPERATION_BUDGET);
                let random =
                    async_nats::rustls::crypto::aws_lc_rs::default_provider().secure_random;
                let mut connect = authenticated(options, deadline, cancel)
                    .await?
                    .name(&options.connection_name)
                    .connection_timeout(BROKER_OPERATION_BUDGET)
                    .initial_connect_deadline(connect_deadline)
                    .request_timeout(Some(BROKER_OPERATION_BUDGET))
                    .reconnect_delay_callback(move |attempts| reconnect_delay(attempts, random))
                    .require_tls(!options.allow_plaintext)
                    .event_callback(move |event| {
                        // Close acknowledgement lets the owner retire telemetry.
                        // Submit this final event before publishing completion.
                        report_connection_event(&event);
                        if matches!(event, async_nats::Event::Closed) {
                            closed_event_tx.send_replace(true);
                        }
                        std::future::ready(())
                    });
                if options.tls_first {
                    connect = connect.tls_first();
                } else if any_tls {
                    // Ordinary TLS sees INFO before the TLS handshake. Mixed
                    // seeds cannot share a single discovery trust boundary.
                    connect = connect.ignore_discovered_servers();
                }
                if let Some(root_ca) = options.root_ca_path.clone() {
                    connect = connect.add_root_certificates(root_ca);
                }
                let client = admission(connect_deadline, cancel, async {
                    connect
                        .connect(servers)
                        .await
                        .map_err(|error| connect_failure(&error))
                })
                .await?;
                slot.insert((client, closed_event)).clone()
            }
        };
        describe_metrics();
        let jetstream = async_nats::jetstream::context::ContextBuilder::new()
            .timeout(BROKER_OPERATION_BUDGET)
            .ack_timeout(BROKER_OPERATION_BUDGET)
            .max_ack_inflight(self.publication_limit)
            .backpressure_on_inflight(false)
            .build(client.clone());
        let dead_letter = admit_topology(options, &client, &jetstream, deadline, cancel).await?;
        let messaging = Messaging {
            shared: Arc::new(Shared {
                client,
                jetstream,
                source_stream: options.source_stream.clone(),
                dead_letter,
                max_payload_bytes: options.max_payload_bytes,
                startup_deadline: deadline,
                startup_cancel: cancel.clone(),
                draining: AtomicBool::new(false),
                failed: AtomicBool::new(false),
                publish_metrics: Outcomes::register(
                    "messaging_publish_total",
                    "messaging_publish_duration_seconds",
                    "result",
                    crate::producer::PUBLISH_RESULTS,
                    None,
                ),
                publish_admission_refused: metrics::counter!(
                    "messaging_publish_admission_refused_total"
                ),
                publish_work: PublishWork::register(self.publication_limit),
                closed_event,
            }),
            consumer: options.consumer.clone(),
        };
        self.connection = None;
        self.options = None;
        Ok(messaging)
    }

    /// Drains any retained native client and observes native and final-event completion.
    pub async fn close(self, deadline: Instant, cancel: &CancellationToken) -> CloseOutcome {
        let Some((client, closed_event)) = self.connection else {
            return CloseOutcome::Complete;
        };
        close_client(
            &client,
            &closed_event,
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
        self.shared.publish_work.observe(&self.shared.jetstream);
        self.check_local_state()?;
        self.shared
            .jetstream
            .get_stream(&self.shared.source_stream)
            .await
            .map_err(|error| {
                let failure = classify_topology(&error);
                tracing::warn!(reason = %failure, "messaging_readiness_probe_failed");
                ProbeError::new("messaging source stream is unavailable")
            })?;
        self.check_local_state()
    }
}

impl MessagingProbe {
    fn check_local_state(&self) -> Result<(), ProbeError> {
        if self.shared.draining.load(Ordering::Acquire) {
            return Err(ProbeError::new("messaging is draining"));
        }
        if self.shared.failed.load(Ordering::Acquire) {
            return Err(ProbeError::new("messaging consumer failed"));
        }
        if self.shared.client.wait_closed().now_or_never().is_some()
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

/// Client options that authenticate with the configured credential source:
/// inline credentials, a credentials file read again for every connection,
/// or neither.
async fn authenticated(
    options: &MessagingOptions,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<async_nats::ConnectOptions, MessagingError> {
    match (&options.credentials, options.credentials_file.clone()) {
        (Some(_), Some(_)) => Err(MessagingError::Configuration(
            "credentials and a credentials file are both set",
        )),
        (Some(credentials), None) => async_nats::ConnectOptions::new()
            .credentials(credentials.expose_secret())
            // The client's parse error is free text about the credentials.
            .map_err(|_| credentials_failure(CredentialsFileError::Malformed)),
        (None, Some(path)) => {
            let file = admission(deadline, cancel, async {
                CredentialsFile::admit(path)
                    .await
                    .map_err(credentials_failure)
            })
            .await?;
            Ok(async_nats::ConnectOptions::with_auth_callback(
                move |nonce| {
                    let file = file.clone();
                    async move {
                        file.answer(&nonce)
                            .await
                            .map_err(async_nats::AuthError::new)
                    }
                },
            ))
        }
        (None, None) => Ok(async_nats::ConnectOptions::new()),
    }
}

/// Preserve the SDK's immediate attempts and capped backoff while spreading
/// each later attempt independently. The SDK still owns all retry and sleep.
fn reconnect_delay(attempts: usize, random: &dyn SecureRandom) -> Duration {
    if attempts <= 1 {
        return Duration::ZERO;
    }
    // Cap before shifting so even usize::MAX cannot wrap into a short retry.
    let base_millis = if attempts >= 13 {
        4_000
    } else {
        1_u64 << (attempts - 1)
    };
    let mut bytes = [0; 2];
    let sample = random.fill(&mut bytes).map(|()| u16::from_le_bytes(bytes));
    spread_reconnect_delay(base_millis, sample)
}

fn spread_reconnect_delay(base_millis: u64, sample: Result<u16, GetRandomFailed>) -> Duration {
    // The capped base is at most 4e9 ns; its 10% window times a u16 fits u64.
    let base_nanos = base_millis * 1_000_000;
    let spread = (base_nanos / 10) * u64::from(sample.unwrap_or_default()) / u64::from(u16::MAX);
    Duration::from_nanos(base_nanos - spread)
}

async fn close_client(
    client: &async_nats::Client,
    closed_event: &watch::Receiver<bool>,
    deadline: Instant,
    cancel: &CancellationToken,
) -> CloseOutcome {
    let mut closed_event = closed_event.clone();
    if client.wait_closed().now_or_never() == Some(true) && *closed_event.borrow() {
        return CloseOutcome::Complete;
    }
    let drain = async {
        if client.wait_closed().now_or_never() != Some(true)
            && client.drain().await.is_err()
            && client.wait_closed().now_or_never() != Some(true)
        {
            return CloseOutcome::UnobservedClose;
        }
        // A submitted Closed event cannot stand in for native resource release.
        if !client.wait_closed().await {
            return CloseOutcome::UnobservedClose;
        }
        if closed_event.wait_for(|submitted| *submitted).await.is_ok() {
            CloseOutcome::Complete
        } else {
            CloseOutcome::UnobservedClose
        }
    };
    let outcome = if cancel.is_cancelled() || Instant::now() >= deadline {
        CloseOutcome::TimedOut
    } else {
        tokio::select! {
            biased;
            () = cancel.cancelled() => CloseOutcome::TimedOut,
            () = tokio::time::sleep_until(deadline) => CloseOutcome::TimedOut,
            result = drain => result,
        }
    };
    if outcome != CloseOutcome::Complete {
        client.force_close();
        if Instant::now() < deadline {
            // Forced closure does not gain a fresh wait budget.
            let _ = tokio::time::timeout_at(deadline, client.wait_closed()).await;
        }
    }
    outcome
}

/// Preserve the SDK's immediate attempts and capped backoff while spreading
/// each later attempt independently. The SDK still owns all retry and sleep.
fn reconnect_delay(attempts: usize, random: &dyn SecureRandom) -> Duration {
    if attempts <= 1 {
        return Duration::ZERO;
    }
    // Cap before shifting so even usize::MAX cannot wrap into a short retry.
    let base_millis = if attempts >= 13 {
        4_000
    } else {
        1_u64 << (attempts - 1)
    };
    let mut bytes = [0; 2];
    let sample = random.fill(&mut bytes).map(|()| u16::from_le_bytes(bytes));
    spread_reconnect_delay(base_millis, sample)
}

fn spread_reconnect_delay(base_millis: u64, sample: Result<u16, GetRandomFailed>) -> Duration {
    // The capped base is at most 4e9 ns; its 10% window times a u16 fits u64.
    let base_nanos = base_millis * 1_000_000;
    let spread = (base_nanos / 10) * u64::from(sample.unwrap_or_default()) / u64::from(u16::MAX);
    Duration::from_nanos(base_nanos - spread)
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
/// Retains the resolved limits until the complete consumer registry is available.
async fn admit_topology(
    options: &MessagingOptions,
    client: &async_nats::Client,
    jetstream: &async_nats::jetstream::Context,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<Option<DeadLetterTopology>, MessagingError> {
    let envelope_limit = options.max_payload_bytes + HEADER_LIMIT_BYTES;
    validate_server(client, envelope_limit).map_err(|refusal| {
        limit_failure(
            "server",
            refusal,
            envelope_limit,
            i64::try_from(client.server_info().max_payload).ok(),
        )
    })?;
    let source = get_stream(jetstream, &options.source_stream, deadline, cancel).await?;
    validate_stream_publication(&source.cached_info().config)
        .map_err(|refusal| limit_failure("source_stream", refusal, envelope_limit, None))?;
    let Some(consumer) = &options.consumer else {
        return Ok(None);
    };
    // The stream's own message limit bounds what one delivery can hold in memory.
    let source_limit = source.cached_info().config.max_message_size;
    let refusal = match usize::try_from(source_limit) {
        Ok(limit) if limit > envelope_limit => Some(Refusal::StreamMessageSize),
        Ok(1..) => None,
        _ => Some(Refusal::StreamMessageSizeUnset),
    };
    if let Some(refusal) = refusal {
        return Err(limit_failure(
            "source_stream",
            refusal,
            envelope_limit,
            Some(source_limit.into()),
        ));
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
        return Err(limit_failure(
            "dead_letter_stream",
            Refusal::DeadLetterIsSource,
            envelope_limit,
            None,
        ));
    }
    let dlq = get_stream(jetstream, &dlq_name, deadline, cancel).await?;
    validate_stream_publication(&dlq.cached_info().config)
        .map_err(|refusal| limit_failure("dead_letter_stream", refusal, envelope_limit, None))?;
    Ok(Some(DeadLetterTopology {
        stream: dlq_name,
        source_limit: usize::try_from(source_limit).map_err(|_| MessagingError::Bounds)?,
        message_limit: dlq.cached_info().config.max_message_size,
    }))
}

/// Requires publication ACKs and durable storage for every publishing role.
/// Replication, fsync and failure-zone placement still belong to the operator.
fn validate_stream_publication(config: &StreamConfig) -> Result<(), Refusal> {
    if config.no_ack {
        return Err(Refusal::StreamNoAck);
    }
    if config.storage != StorageType::File {
        return Err(Refusal::StreamMemoryStorage);
    }
    if matches!(config.persist_mode, Some(PersistenceMode::Async)) {
        return Err(Refusal::StreamAsyncPersistence);
    }
    Ok(())
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

/// Why an answering broker cannot carry this adapter's deliveries.
#[derive(Clone, Copy, Debug)]
enum Refusal {
    ServerVersion,
    JetStreamDisabled,
    HeadersUnsupported,
    ServerMaxPayload,
    StreamMessageSizeUnset,
    StreamMessageSize,
    DeadLetterIsSource,
    StreamMemoryStorage,
    StreamAsyncPersistence,
    StreamNoAck,
    TransferBounds,
    TransferHeaderSize,
}

impl Refusal {
    /// The closed `error.type` of this refusal.
    const fn error_type(self) -> &'static str {
        match self {
            Self::ServerVersion => "server_version",
            Self::JetStreamDisabled => "jetstream_disabled",
            Self::HeadersUnsupported => "headers_unsupported",
            Self::ServerMaxPayload => "server_max_payload",
            Self::StreamMessageSizeUnset => "stream_max_message_size_unset",
            Self::StreamMessageSize => "stream_max_message_size",
            Self::DeadLetterIsSource => "dead_letter_stream_is_source",
            Self::StreamMemoryStorage => "stream_memory_storage",
            Self::StreamAsyncPersistence => "stream_async_persistence",
            Self::StreamNoAck => "stream_no_ack",
            Self::TransferBounds => "dead_letter_bounds_unavailable",
            Self::TransferHeaderSize => "dead_letter_header_bytes",
        }
    }

    const fn failure(self) -> MessagingError {
        match self {
            Self::ServerMaxPayload
            | Self::StreamMessageSizeUnset
            | Self::StreamMessageSize
            | Self::TransferBounds
            | Self::TransferHeaderSize => MessagingError::Bounds,
            Self::ServerVersion
            | Self::JetStreamDisabled
            | Self::HeadersUnsupported
            | Self::DeadLetterIsSource
            | Self::StreamMemoryStorage
            | Self::StreamAsyncPersistence
            | Self::StreamNoAck => MessagingError::Topology,
        }
    }
}

/// Classifies a refusal and logs which property of the broker caused it.
///
/// `required_bytes` is the admitted envelope or transfer bound; `limit_bytes`
/// is the corresponding broker limit. Neither exposes message content.
fn limit_failure(
    operation: &'static str,
    refusal: Refusal,
    required_bytes: usize,
    limit_bytes: Option<i64>,
) -> MessagingError {
    let failure = refusal.failure();
    tracing::warn!(
        operation,
        reason = %failure,
        error.type = refusal.error_type(),
        required_bytes,
        limit_bytes,
        "messaging_admission_failed"
    );
    failure
}

/// Classifies an unusable credentials source and logs why, without the path
/// or any of its content.
fn credentials_failure(error: CredentialsFileError) -> MessagingError {
    let failure = match error {
        CredentialsFileError::Unreadable(_) => {
            MessagingError::Configuration("credentials file is unreadable")
        }
        CredentialsFileError::Malformed => MessagingError::Authentication,
    };
    tracing::warn!(
        operation = "credentials",
        reason = %failure,
        error.type = error.error_type(),
        "messaging_admission_failed"
    );
    failure
}

fn validate_server(client: &async_nats::Client, envelope_limit: usize) -> Result<(), Refusal> {
    let info = client.server_info();
    if !client.is_server_compatible(2, 12, 3) {
        return Err(Refusal::ServerVersion);
    }
    if !info.jetstream {
        return Err(Refusal::JetStreamDisabled);
    }
    if !info.headers {
        return Err(Refusal::HeadersUnsupported);
    }
    // The server bounds payload and headers together.
    if info.max_payload < envelope_limit {
        return Err(Refusal::ServerMaxPayload);
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

/// Classifies a failed first connection and logs which stage refused it.
///
/// `Connection` covers name resolution, TLS and the socket alike; the client's
/// kind is a closed vocabulary that tells them apart. Its source can quote a
/// server URL and stays out of the log.
fn connect_failure(error: &async_nats::ConnectError) -> MessagingError {
    let (failure, error_type) = match error.kind() {
        ConnectErrorKind::Authentication => (MessagingError::Authentication, "authentication"),
        ConnectErrorKind::AuthorizationViolation => {
            (MessagingError::Authentication, "authorization_violation")
        }
        ConnectErrorKind::TimedOut => (
            MessagingError::TimedOut {
                budget: BROKER_OPERATION_BUDGET,
            },
            "timeout",
        ),
        ConnectErrorKind::ServerParse => (
            MessagingError::Configuration("server URL is invalid"),
            "server_parse",
        ),
        ConnectErrorKind::Dns => (MessagingError::Connection, "dns"),
        ConnectErrorKind::Tls => (MessagingError::Connection, "tls"),
        ConnectErrorKind::Io => (MessagingError::Connection, "io"),
        ConnectErrorKind::MaxReconnects => (MessagingError::Connection, "max_reconnects"),
    };
    tracing::warn!(
        operation = "connect",
        reason = %failure,
        error.type = error_type,
        "messaging_admission_failed"
    );
    failure
}

/// Counts every connection event and logs the ones an operator acts on.
///
/// The client reports a slow consumer once per dropped message, so that
/// event is only counted. Server and client error text is arbitrary and stays
/// out of the log; `error.type` names the client's closed variant.
fn report_connection_event(event: &async_nats::Event) {
    use async_nats::{ClientError, Event, ServerError};

    let (result, error_type) = match event {
        Event::Connected => ("connected", None),
        Event::Disconnected => ("disconnected", None),
        Event::Draining => ("draining", None),
        Event::Closed => ("closed", None),
        Event::LameDuckMode => ("lame_duck", None),
        Event::SlowConsumer(_) => ("slow_consumer", None),
        Event::ServerError(error) => (
            "server_error",
            Some(match error {
                ServerError::AuthorizationViolation => "authorization_violation",
                ServerError::SlowConsumer(_) => "slow_consumer",
                ServerError::Other(_) => "other",
            }),
        ),
        Event::ClientError(error) => (
            "client_error",
            Some(match error {
                ClientError::MaxReconnects => "max_reconnects",
                ClientError::ServerNotInPool => "server_not_in_pool",
                ClientError::Other(_) => "other",
            }),
        ),
    };
    metrics::counter!("messaging_connection_events_total", "result" => result).increment(1);
    match event {
        Event::SlowConsumer(_) => {}
        Event::Connected | Event::Draining | Event::Closed => {
            tracing::info!(result, "messaging_connection");
        }
        Event::Disconnected
        | Event::LameDuckMode
        | Event::ServerError(_)
        | Event::ClientError(_) => {
            tracing::warn!(result, error.type = error_type, "messaging_connection");
        }
    }
}

/// Describes the adapter's metrics to the installed recorder. Repeating it is
/// harmless.
fn describe_metrics() {
    use metrics::{Unit, describe_counter, describe_gauge, describe_histogram};

    describe_counter!(
        "messaging_publish_admission_refused_total",
        "Native publication admission refusals because all ACK permits are occupied"
    );
    describe_gauge!(
        "messaging_publish_work_in_flight",
        "Last observed native publication permits, including abandoned ACK cleanup; NaN before observation"
    );
    describe_gauge!(
        "messaging_publish_work_last_observed_timestamp_seconds",
        Unit::Seconds,
        "Unix time of the native publication observation; zero before observation"
    );
    describe_gauge!(
        "messaging_publish_capacity",
        "Fixed native publication permit capacity"
    );
    describe_counter!(
        "messaging_publish_total",
        "Publications by result: acknowledged, rejected or ambiguous"
    );
    describe_histogram!(
        "messaging_publish_duration_seconds",
        Unit::Seconds,
        "Time from publication start to its result"
    );
    describe_counter!(
        "messaging_handler_total",
        "Handled deliveries by event type and outcome"
    );
    describe_histogram!(
        "messaging_handler_duration_seconds",
        Unit::Seconds,
        "Handler run time by event type and outcome"
    );
    describe_counter!(
        "messaging_dead_letter_total",
        "Dead-letter transfers by event type, reason and transfer outcome"
    );
    describe_counter!(
        "messaging_settlement_failures_total",
        "Unconfirmed source acknowledgements and failed redelivery requests"
    );
    describe_counter!(
        "messaging_consumer_stream_errors_total",
        "Recoverable pull-stream errors"
    );
    describe_counter!(
        "messaging_connection_events_total",
        "Broker connection events by result"
    );
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_nats::{ClientError, ConnectError, Event, ServerError};

    use super::*;

    #[test]
    fn reconnect_keeps_immediate_attempts_and_saturates_before_large_counts() {
        let random = async_nats::rustls::crypto::aws_lc_rs::default_provider().secure_random;
        assert_eq!(reconnect_delay(0, random), Duration::ZERO);
        assert_eq!(reconnect_delay(1, random), Duration::ZERO);
        for (attempts, minimum_micros, maximum_micros) in [
            (2, 1_800, 2_000),
            (4, 7_200, 8_000),
            (12, 1_843_200, 2_048_000),
            (13, 3_600_000, 4_000_000),
            (50, 3_600_000, 4_000_000),
            (usize::MAX, 3_600_000, 4_000_000),
        ] {
            let delay = reconnect_delay(attempts, random);
            assert!(delay >= Duration::from_micros(minimum_micros));
            assert!(delay <= Duration::from_micros(maximum_micros));
        }
    }

    #[test]
    fn reconnect_spread_has_exact_endpoints_and_conservative_source_failure() {
        for (base_millis, minimum_nanos) in [(2, 1_800_000), (4_000, 3_600_000_000)] {
            let base = Duration::from_millis(base_millis);
            assert_eq!(spread_reconnect_delay(base_millis, Ok(0)), base);
            assert_eq!(
                spread_reconnect_delay(base_millis, Ok(u16::MAX)),
                Duration::from_nanos(minimum_nanos)
            );
            assert_eq!(
                spread_reconnect_delay(base_millis, Err(GetRandomFailed)),
                base
            );
            for sample in [1, u16::MAX / 2, u16::MAX - 1] {
                let delay = spread_reconnect_delay(base_millis, Ok(sample));
                assert!(delay >= Duration::from_nanos(minimum_nanos));
                assert!(delay <= base);
            }
        }
    }

    #[test]
    fn publication_occupancy_starts_unknown_with_fixed_capacity() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        metrics::with_local_recorder(&recorder, || {
            let _work = PublishWork::register(17);
        });
        let scrape = recorder.handle().render();
        assert!(scrape.contains("messaging_publish_work_in_flight NaN"));
        assert!(scrape.contains("messaging_publish_work_last_observed_timestamp_seconds 0"));
        assert!(scrape.contains("messaging_publish_capacity 17"));
    }

    #[tokio::test]
    async fn invalid_publication_bounds_are_rejected_before_connecting() {
        for max_payload_bytes in [0, 64 * 1024 * 1024 - 8192 + 1, usize::MAX] {
            let result = Messaging::connect(
                MessagingOptions {
                    connection_name: "invalid-bounds".to_owned(),
                    servers: vec!["not a server URL".to_owned()],
                    credentials: None,
                    credentials_file: None,
                    root_ca_path: None,
                    allow_plaintext: true,
                    tls_first: false,
                    source_stream: "SOURCE".to_owned(),
                    dlq_stream: None,
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

    #[test]
    fn reconnect_keeps_immediate_attempts_and_saturates_before_large_counts() {
        let random = async_nats::rustls::crypto::aws_lc_rs::default_provider().secure_random;
        assert_eq!(reconnect_delay(0, random), Duration::ZERO);
        assert_eq!(reconnect_delay(1, random), Duration::ZERO);
        for (attempts, minimum_micros, maximum_micros) in [
            (2, 1_800, 2_000),
            (4, 7_200, 8_000),
            (12, 1_843_200, 2_048_000),
            (13, 3_600_000, 4_000_000),
            (50, 3_600_000, 4_000_000),
            (usize::MAX, 3_600_000, 4_000_000),
        ] {
            let delay = reconnect_delay(attempts, random);
            assert!(delay >= Duration::from_micros(minimum_micros));
            assert!(delay <= Duration::from_micros(maximum_micros));
        }
    }

    #[test]
    fn reconnect_spread_has_exact_endpoints_and_conservative_source_failure() {
        for (base_millis, minimum_nanos) in [(2, 1_800_000), (4_000, 3_600_000_000)] {
            let base = Duration::from_millis(base_millis);
            assert_eq!(spread_reconnect_delay(base_millis, Ok(0)), base);
            assert_eq!(
                spread_reconnect_delay(base_millis, Ok(u16::MAX)),
                Duration::from_nanos(minimum_nanos)
            );
            assert_eq!(
                spread_reconnect_delay(base_millis, Err(GetRandomFailed)),
                base
            );
            for sample in [1, u16::MAX / 2, u16::MAX - 1] {
                let delay = spread_reconnect_delay(base_millis, Ok(sample));
                assert!(delay >= Duration::from_nanos(minimum_nanos));
                assert!(delay <= base);
            }
        }
    }

    /// One logged event's fields, the message under `message`.
    type Logged = Vec<(&'static str, String)>;

    #[derive(Clone, Default)]
    struct Events(Arc<Mutex<Vec<Logged>>>);

    struct Fields(Logged);

    impl tracing::field::Visit for Fields {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.push((field.name(), format!("{value:?}")));
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.push((field.name(), value.to_owned()));
        }
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Events {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut fields = Fields(Vec::new());
            event.record(&mut fields);
            self.0.lock().expect("event lock").push(fields.0);
        }
    }

    fn logged(test: impl FnOnce()) -> Vec<Logged> {
        use tracing_subscriber::layer::SubscriberExt as _;

        let events = Events::default();
        let subscriber = tracing_subscriber::registry().with(events.clone());
        tracing::subscriber::with_default(subscriber, || {
            // Keep callsite interest independent of a sibling test's
            // thread-local subscriber.
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            test();
        });
        events.0.lock().expect("event lock").clone()
    }

    fn field<'a>(event: &'a Logged, name: &str) -> Option<&'a str> {
        event
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn a_failed_first_connection_logs_which_stage_refused_it() {
        let stages = [
            (ConnectErrorKind::Dns, "dns"),
            (ConnectErrorKind::Tls, "tls"),
            (ConnectErrorKind::Io, "io"),
        ];
        let events = logged(|| {
            for (kind, _) in stages {
                let source = std::io::Error::other("nats://user:sentinel@broker.invalid");
                let failure = connect_failure(&ConnectError::with_source(kind, source));
                assert!(matches!(failure, MessagingError::Connection));
            }
        });

        assert_eq!(events.len(), stages.len());
        for (event, (_, error_type)) in events.iter().zip(stages) {
            assert_eq!(field(event, "message"), Some("messaging_admission_failed"));
            assert_eq!(field(event, "operation"), Some("connect"));
            assert_eq!(field(event, "error.type"), Some(error_type));
            assert!(event.iter().all(|(_, value)| !value.contains("sentinel")));
        }
    }

    #[test]
    fn connection_events_are_counted_and_logged_without_broker_text() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let events = metrics::with_local_recorder(&recorder, || {
            logged(|| {
                for event in [
                    Event::Disconnected,
                    Event::SlowConsumer(7),
                    Event::SlowConsumer(7),
                    Event::ServerError(ServerError::Other("sentinel broker text".to_owned())),
                    Event::ClientError(ClientError::MaxReconnects),
                    Event::Connected,
                ] {
                    report_connection_event(&event);
                }
            })
        });

        let scrape = recorder.handle().render();
        for (result, count) in [
            ("disconnected", 1),
            ("slow_consumer", 2),
            ("server_error", 1),
            ("client_error", 1),
            ("connected", 1),
        ] {
            let line = format!("messaging_connection_events_total{{result=\"{result}\"}} {count}");
            assert!(scrape.contains(&line), "{line} is missing from:\n{scrape}");
        }
        // A slow consumer is reported once per dropped message and only counted.
        let logged: Vec<_> = events
            .iter()
            .map(|event| {
                assert_eq!(field(event, "message"), Some("messaging_connection"));
                assert!(event.iter().all(|(_, value)| !value.contains("sentinel")));
                (
                    field(event, "result").expect("result"),
                    field(event, "error.type"),
                )
            })
            .collect();
        assert_eq!(
            logged,
            [
                ("disconnected", None),
                ("server_error", Some("other")),
                ("client_error", Some("max_reconnects")),
                ("connected", None),
            ]
        );
    }
}
