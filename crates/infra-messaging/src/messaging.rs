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
// template:begin outbox:messaging-outbox-refusal-import
use crate::topology::refusal_failure;
// template:end outbox:messaging-outbox-refusal-import
use crate::topology::{TopologyAdmission, admit_topology};
use crate::wire::HEADER_LIMIT_BYTES;

pub(crate) const PUBLICATION_WINDOW_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, derive_more::Debug)]
pub struct MessagingOptions {
    pub connection_name: String,
    #[debug("{} servers", servers.len())]
    pub servers: Vec<String>,
    pub credentials: Option<Credentials>,
    pub root_ca_path: Option<PathBuf>,
    pub allow_plaintext: bool,
    pub tls_first: bool,
    pub source_stream: String,
    pub max_payload_bytes: usize,
    pub consumer: Option<ConsumerOptions>,
}

#[derive(Clone, Debug)]
pub enum Credentials {
    Inline(SecretString),
    File(PathBuf),
}

#[derive(Clone, Debug)]
pub struct ConsumerOptions {
    pub durable_name: String,
    pub filter_subject: String,
    pub dlq_subject: String,
    pub dlq_stream: Option<String>,
    pub concurrency: NonZeroUsize,
}

#[derive(Clone, Debug)]
pub struct Messaging {
    pub(crate) shared: Arc<Shared>,
    topology: TopologyAdmission,
}

// template:begin outbox:messaging-outbox-producer
#[derive(Debug)]
pub struct OutboxProducer(pub(crate) Producer);
// template:end outbox:messaging-outbox-producer

#[derive(Debug)]
pub struct MessagingStartup {
    options: MessagingOptions,
    publication_permits: usize,
    envelope_limit: usize,
    deadline: Instant,
    cancel: CancellationToken,
    phase: StartupPhase,
}

#[derive(Debug)]
enum StartupPhase {
    Prepared,
    Connected(NativeConnection),
    Admitted,
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) connection: NativeConnection,
    pub(crate) jetstream: async_nats::jetstream::Context,
    pub(crate) source_stream: String,
    pub(crate) max_payload_bytes: usize,
    pub(crate) envelope_limit: usize,
    draining: AtomicBool,
    failed: AtomicBool,
    pub(crate) publish_metrics: PublishMetrics,
    pub(crate) publish_work: PublishWork,
}

impl Shared {
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
    Complete,
    WaitInterrupted,
    UnobservedClose,
}

impl Messaging {
    pub async fn connect(
        options: MessagingOptions,
        deadline: Instant,
        cancel: CancellationToken,
    ) -> Result<Self, MessagingError> {
        let mut startup = Self::prepare(options, deadline, cancel.clone())?;
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
        if publication_permits == 0
            || options
                .consumer
                .as_ref()
                .is_some_and(|consumer| consumer.concurrency.get() > publication_permits)
        {
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

    // template:begin outbox:messaging-outbox-producer-access
    pub fn outbox_producer(&self) -> Result<OutboxProducer, MessagingError> {
        self.topology.outbox_source.map_err(|refusal| {
            refusal_failure("source_stream", refusal, self.shared.envelope_limit, None)
        })?;
        Ok(OutboxProducer(self.producer()))
    }
    // template:end outbox:messaging-outbox-producer-access

    #[must_use]
    pub fn probe(&self) -> MessagingProbe {
        MessagingProbe {
            shared: Arc::clone(&self.shared),
        }
    }

    pub async fn consumer(
        &self,
        registry: Registry,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<Consumer, MessagingError> {
        if self.shared.refuses_work() {
            return Err(MessagingError::Unavailable);
        }
        let admission = self
            .topology
            .consumer
            .clone()
            .ok_or(MessagingError::Configuration("consumer is not configured"))?;
        registry
            .require_handlers()
            .map_err(|_| MessagingError::Configuration("no typed event handlers are registered"))?;
        Consumer::admit(
            Arc::clone(&self.shared),
            admission,
            registry,
            deadline,
            cancel,
        )
        .await
    }

    pub async fn close(self, deadline: Instant, cancel: &CancellationToken) -> CloseOutcome {
        self.shared.begin_drain();
        self.shared.connection.close(deadline, cancel).await
    }
}

impl MessagingStartup {
    pub async fn admit(&mut self) -> Result<Messaging, MessagingError> {
        let options = &self.options;
        let deadline = self.deadline;
        let cancel = &self.cancel;
        crate::producer::describe_metrics();
        crate::probe::describe_metrics();
        crate::connection::describe_metrics();
        crate::consumer::describe_metrics();
        crate::credentials::describe_metrics();
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
            .backpressure_on_inflight(false)
            .build(client.clone());
        let topology = admit_topology(
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
            topology,
        };
        self.phase = StartupPhase::Admitted;
        Ok(messaging)
    }

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

    #[test]
    fn preparation_bounds_publication_and_resident_deliveries() {
        for (max_payload_bytes, concurrency, accepted) in [
            (0, None, false),
            (64 * 1024 * 1024 - 8192 + 1, None, false),
            (usize::MAX, None, false),
            (32 * 1024 * 1024 - 8192, Some(2), true),
            (32 * 1024 * 1024 - 8192 + 1, Some(2), false),
            (1, Some(usize::MAX), false),
        ] {
            let result = Messaging::prepare(
                MessagingOptions {
                    connection_name: "invalid-bounds".to_owned(),
                    servers: vec!["not a server URL".to_owned()],
                    credentials: None,
                    root_ca_path: None,
                    allow_plaintext: true,
                    tls_first: false,
                    source_stream: "SOURCE".to_owned(),
                    max_payload_bytes,
                    consumer: concurrency.and_then(NonZeroUsize::new).map(|concurrency| {
                        ConsumerOptions {
                            durable_name: "worker".to_owned(),
                            filter_subject: "events".to_owned(),
                            dlq_subject: "failed".to_owned(),
                            dlq_stream: None,
                            concurrency,
                        }
                    }),
                },
                Instant::now() + BROKER_OPERATION_BUDGET,
                CancellationToken::new(),
            );
            if accepted {
                assert!(result.is_ok());
            } else {
                std::assert_matches!(result, Err(MessagingError::Bounds));
            }
        }
    }
}
