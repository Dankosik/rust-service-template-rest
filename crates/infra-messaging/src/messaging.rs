use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use operation_context::OperationContext;
use secrecy::SecretString;

use crate::budget::BROKER_OPERATION_BUDGET;
use crate::connection::NativeConnection;
use crate::consumer::Consumer;
use crate::error::MessagingError;
use crate::probe::{MessagingProbe, PublishWorkMetrics};
use crate::producer::{Producer, PublishMetrics};
use crate::registry::Registry;
use crate::topology::{TopologyAdmission, admit_topology};
use crate::wire::HEADER_LIMIT_BYTES;

pub(crate) const PUBLICATION_WINDOW_BYTES: usize =
    shared_rules::messaging::PUBLICATION_WINDOW_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportSecurity {
    Tls,
    TlsFirst,
    AllowPlaintext,
}

#[derive(Clone, derive_more::Debug)]
pub struct MessagingOptions {
    pub connection_name: String,
    #[debug("{} servers", servers.len())]
    pub servers: Vec<String>,
    pub credentials: Option<Credentials>,
    pub root_ca_path: Option<PathBuf>,
    pub security: TransportSecurity,
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
}

// template:begin outbox:messaging-outbox-producer
#[derive(Clone, Debug)]
pub struct OutboxProducer(pub(crate) Producer);
// template:end outbox:messaging-outbox-producer

#[must_use = "admit or close MessagingStartup explicitly"]
#[derive(Debug)]
pub struct MessagingStartup {
    phase: StartupPhase,
}

#[derive(Debug)]
struct StartupConfig {
    options: MessagingOptions,
    publication_permits: usize,
    envelope_limit_bytes: usize,
    context: OperationContext,
}

#[derive(Debug)]
enum StartupPhase {
    Starting {
        config: Box<StartupConfig>,
        connection: Option<NativeConnection>,
    },
    Admitted(Messaging),
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) connection: NativeConnection,
    pub(crate) jetstream: async_nats::jetstream::Context,
    pub(crate) source_stream: async_nats::HeaderValue,
    pub(crate) source_stream_info_subject: async_nats::Subject,
    pub(crate) max_payload_bytes: usize,
    pub(crate) envelope_limit_bytes: usize,
    topology: TopologyAdmission,
    draining: AtomicBool,
    failed: AtomicBool,
    pub(crate) publish_metrics: PublishMetrics,
    pub(crate) publish_work_metrics: PublishWorkMetrics,
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

#[must_use = "inspect close outcome to log or handle incomplete drain"]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    Complete,
    WaitInterrupted,
    UnobservedClose,
}

impl Messaging {
    pub async fn connect(
        options: MessagingOptions,
        context: &OperationContext,
    ) -> Result<Self, MessagingError> {
        let mut startup = Self::prepare(options, context)?;
        let admitted = startup.admit().await;
        if admitted.is_err() {
            let outcome = startup.close(context).await;
            if outcome != CloseOutcome::Complete {
                tracing::warn!(?outcome, "messaging_admission_close_incomplete");
            }
        }
        admitted
    }

    pub fn prepare(
        options: MessagingOptions,
        context: &OperationContext,
    ) -> Result<MessagingStartup, MessagingError> {
        if options.max_payload_bytes == 0 {
            return Err(MessagingError::Bounds);
        }
        let envelope_limit_bytes = options
            .max_payload_bytes
            .checked_add(HEADER_LIMIT_BYTES)
            .ok_or(MessagingError::Bounds)?;
        let publication_permits = PUBLICATION_WINDOW_BYTES / envelope_limit_bytes;
        if publication_permits == 0
            || options
                .consumer
                .as_ref()
                .is_some_and(|consumer| consumer.concurrency.get() > publication_permits)
        {
            return Err(MessagingError::Bounds);
        }
        Ok(MessagingStartup {
            phase: StartupPhase::Starting {
                config: Box::new(StartupConfig {
                    options,
                    publication_permits,
                    envelope_limit_bytes,
                    context: context.clone(),
                }),
                connection: None,
            },
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
        self.shared
            .topology
            .outbox_source
            .map_err(|refusal| refusal.report(self.shared.envelope_limit_bytes))?;
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
        context: &OperationContext,
    ) -> Result<Consumer, MessagingError> {
        if self.shared.refuses_work() {
            return Err(MessagingError::Unavailable);
        }
        let admission = self
            .shared
            .topology
            .consumer
            .clone()
            .ok_or(MessagingError::Configuration("consumer is not configured"))?;
        registry
            .require_handlers()
            .map_err(|_| MessagingError::Configuration("no typed event handlers are registered"))?;
        Consumer::admit(Arc::clone(&self.shared), admission, registry, context).await
    }

    pub async fn close(self, context: &OperationContext) -> CloseOutcome {
        self.shared.begin_drain();
        self.shared.connection.close(context).await
    }
}

impl MessagingStartup {
    pub async fn admit(&mut self) -> Result<Messaging, MessagingError> {
        Box::pin(self.admit_inner()).await
    }

    async fn admit_inner(&mut self) -> Result<Messaging, MessagingError> {
        crate::observe::describe_metrics();
        let StartupPhase::Starting {
            config,
            connection: cached_connection,
        } = &mut self.phase
        else {
            return Err(MessagingError::Configuration(
                "messaging is already admitted",
            ));
        };
        let connection = match cached_connection {
            Some(existing) => existing.clone(),
            None => cached_connection
                .insert(NativeConnection::open(&config.options, &config.context).await?)
                .clone(),
        };
        let client = &connection.client;
        let jetstream = async_nats::jetstream::context::ContextBuilder::new()
            .timeout(BROKER_OPERATION_BUDGET)
            .ack_timeout(BROKER_OPERATION_BUDGET)
            .max_ack_inflight(config.publication_permits)
            .backpressure_on_inflight(false)
            .build(client.clone());
        let topology = admit_topology(
            &config.options,
            config.envelope_limit_bytes,
            client,
            &jetstream,
            &config.context,
        )
        .await?;
        let source_stream = config
            .options
            .source_stream
            .parse::<async_nats::HeaderValue>()
            .map_err(|_| MessagingError::Topology)?;
        let source_stream_info_subject =
            async_nats::Subject::from(format!("STREAM.INFO.{}", source_stream.as_str()));
        let messaging = Messaging {
            shared: Arc::new(Shared {
                jetstream,
                source_stream,
                source_stream_info_subject,
                max_payload_bytes: config.options.max_payload_bytes,
                envelope_limit_bytes: config.envelope_limit_bytes,
                topology,
                draining: AtomicBool::new(false),
                failed: AtomicBool::new(false),
                publish_metrics: PublishMetrics::register(),
                publish_work_metrics: PublishWorkMetrics::register(
                    config.publication_permits,
                    &config.options.connection_name,
                ),
                connection,
            }),
        };
        self.phase = StartupPhase::Admitted(messaging.clone());
        Ok(messaging)
    }

    #[must_use]
    pub const fn admitted(&self) -> Option<&Messaging> {
        match &self.phase {
            StartupPhase::Admitted(messaging) => Some(messaging),
            StartupPhase::Starting { .. } => None,
        }
    }

    pub async fn close(self, context: &OperationContext) -> CloseOutcome {
        match self.phase {
            StartupPhase::Starting {
                connection: Some(connection),
                ..
            } => connection.close(context).await,
            StartupPhase::Starting {
                connection: None, ..
            } => CloseOutcome::Complete,
            StartupPhase::Admitted(messaging) => messaging.close(context).await,
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
                    security: TransportSecurity::AllowPlaintext,
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
                &OperationContext::with_timeout(BROKER_OPERATION_BUDGET),
            );
            if accepted {
                let startup = result.expect("valid bounds are prepared");
                assert!(startup.admitted().is_none());
            } else {
                std::assert_matches!(result, Err(MessagingError::Bounds));
            }
        }
    }
}
