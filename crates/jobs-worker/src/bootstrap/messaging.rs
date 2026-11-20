use std::time::Duration;

use health::BoxedProbe;
use infra_messaging::{
    Consumer, ConsumerOptions, Credentials, Messaging, MessagingError, MessagingOptions,
    Registry as MessagingRegistry, TransportSecurity,
};
use service_config::{MessagingCredentials, WorkerConfig};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{WorkerError, worker_identity};
use crate::background::Background;
use crate::resources::{BrokerResource, Resources};

const MESSAGING_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct RetainedMessages {
    pub(super) registry: MessagingRegistry,
    // template:begin outbox:worker-bootstrap-postgres-messages-field
    pub(super) postgres_messages: Option<crate::PostgresMessageFactory>,
    // template:end outbox:worker-bootstrap-postgres-messages-field
}

pub(super) struct MessagingPlan {
    options: MessagingOptions,
    retained_messages: Option<RetainedMessages>,
}

impl MessagingPlan {
    pub(super) fn if_consuming(
        config: &WorkerConfig,
        messages: RetainedMessages,
    ) -> Result<Option<Self>, WorkerError> {
        messages
            .registry
            .check_publication_limit(max_payload_bytes(config)?)?;
        let consumes_messages = messages.registry.has_handlers();
        // template:begin outbox:worker-bootstrap-postgres-messages-intent
        let consumes_messages = consumes_messages || messages.postgres_messages.is_some();
        // template:end outbox:worker-bootstrap-postgres-messages-intent
        if !consumes_messages {
            return Ok(None);
        }
        let keys = config.messaging().consumer_keys()?;
        let consumer = ConsumerOptions {
            durable_name: keys.durable.to_owned(),
            filter_subject: keys.filter_subject.to_owned(),
            dlq_subject: keys.dlq_subject.to_owned(),
            dlq_stream: None,
            concurrency: std::num::NonZeroUsize::try_from(config.messaging().consumer_concurrency)
                .map_err(|_| WorkerError::MessagingConcurrency)?,
        };
        Ok(Some(Self {
            options: messaging_options(config, keys.source_stream, Some(consumer))?,
            retained_messages: Some(messages),
        }))
    }

    // template:begin outbox:worker-bootstrap-outbox-messaging
    pub(super) fn for_producer(config: &WorkerConfig) -> Result<Self, WorkerError> {
        let source_stream = config.messaging().producer_source_stream()?;
        Ok(Self {
            options: messaging_options(config, source_stream, None)?,
            retained_messages: None,
        })
    }
    // template:end outbox:worker-bootstrap-outbox-messaging
}

pub(super) async fn connect_messaging(
    plan: MessagingPlan,
    cancel: &CancellationToken,
    resources: &mut Resources,
) -> Result<Option<Consumer>, WorkerError> {
    let MessagingPlan {
        options,
        retained_messages,
    } = plan;
    // template:begin outbox:worker-bootstrap-activate-postgres-messages
    let mut retained_messages = retained_messages;
    if let Some(messages) = &mut retained_messages {
        messages.register_postgres_messages(resources)?;
        messages
            .registry
            .check_publication_limit(options.max_payload_bytes)?;
    }
    // template:end outbox:worker-bootstrap-activate-postgres-messages
    let deadline = Instant::now() + MESSAGING_STARTUP_TIMEOUT;
    let startup = Messaging::prepare(options, deadline, cancel.child_token())?;
    let BrokerResource::Starting(startup) = resources
        .dependencies
        .broker
        .insert(BrokerResource::Starting(Box::new(startup)))
    else {
        return Err(MessagingError::Connection.into());
    };
    let messaging = startup.admit().await?;
    resources.dependencies.broker = Some(BrokerResource::Admitted(messaging.clone()));
    // template:begin outbox:worker-bootstrap-admit-outbox-source
    messaging.outbox_producer()?;
    // template:end outbox:worker-bootstrap-admit-outbox-source
    if let Some(messages) = retained_messages {
        Ok(Some(
            messaging
                .consumer(messages.registry, deadline, cancel)
                .await?,
        ))
    } else {
        Ok(None)
    }
}

fn max_payload_bytes(config: &WorkerConfig) -> Result<usize, WorkerError> {
    usize::try_from(config.messaging().max_payload_bytes.as_u64())
        .map_err(|_| WorkerError::MessagingPayloadBound)
}

fn messaging_options(
    config: &WorkerConfig,
    source_stream: &str,
    consumer: Option<ConsumerOptions>,
) -> Result<MessagingOptions, WorkerError> {
    let messaging = config.messaging();
    let security = if messaging.tls_first {
        TransportSecurity::TlsFirst
    } else if messaging.plaintext_admitted {
        TransportSecurity::AllowPlaintext
    } else {
        TransportSecurity::Tls
    };
    Ok(MessagingOptions {
        connection_name: worker_identity(&config.observability().otel.service_name),
        servers: messaging.urls.clone(),
        credentials: messaging
            .credentials
            .as_ref()
            .and_then(|source| match source {
                MessagingCredentials::Inline(secret) => Some(Credentials::Inline(secret.clone())),
                MessagingCredentials::File(path) => Some(Credentials::File(path.clone())),
                MessagingCredentials::Unauthenticated => None,
            }),
        root_ca_path: messaging.root_ca_path.clone(),
        security,
        source_stream: source_stream.to_owned(),
        max_payload_bytes: max_payload_bytes(config)?,
        consumer,
    })
}

pub(super) fn probe(resources: &Resources) -> Option<BoxedProbe> {
    let messaging = resources.admitted_messaging()?;
    Some(BoxedProbe::new(messaging.probe()))
}

pub(super) fn start_consumer(
    consumer: Consumer,
    resources: &mut Resources,
    background: &Background,
) {
    resources.work.consumer = Some(consumer.start(&background.cancel));
    tracing::info!("messaging_consuming_started");
}

pub(super) async fn consumer_failure(resources: &Resources) -> WorkerError {
    match &resources.work.consumer {
        Some(consumer) => WorkerError::ConsumerStopped(consumer.failed().await),
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use crate::bootstrap::WorkerError;

    #[test]
    fn refusal_texts_name_the_failed_check() {
        assert_eq!(
            WorkerError::ConsumerStopped(infra_messaging::ConsumerError::ConsumerLost).to_string(),
            "the messaging consumer stopped without a stop signal: messaging durable consumer was deleted, replaced, or reconfigured incompatibly"
        );
    }
}
