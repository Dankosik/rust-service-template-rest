use std::time::Duration;

use infra_messaging::{
    Consumer, ConsumerOptions, Credentials, Messaging, MessagingOptions,
    Registry as MessagingRegistry, TransportSecurity,
};
use service_config::{
    AdmittedMessaging, MessagingCredentials, MessagingTransportSecurity, WorkerConfig,
};
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
            .check_publication_limit(config.messaging().max_payload_bytes)?;
        let consumes_messages = messages.registry.has_handlers();
        // template:begin outbox:worker-bootstrap-postgres-messages-intent
        let consumes_messages = consumes_messages || messages.postgres_messages.is_some();
        // template:end outbox:worker-bootstrap-postgres-messages-intent
        if !consumes_messages {
            return Ok(None);
        }
        let admitted = config.messaging().admit_consumer()?;
        let consumer = ConsumerOptions {
            durable_name: admitted.durable.to_owned(),
            filter_subject: admitted.filter_subject.to_owned(),
            dlq_subject: admitted.dlq_subject.to_owned(),
            dlq_stream: None,
            concurrency: admitted.concurrency,
        };
        Ok(Some(Self {
            options: messaging_options(config, admitted.messaging, Some(consumer)),
            retained_messages: Some(messages),
        }))
    }

    // template:begin outbox:worker-bootstrap-outbox-messaging
    pub(super) fn for_producer(config: &WorkerConfig) -> Result<Self, WorkerError> {
        let admitted = config.messaging().admit_producer()?;
        Ok(Self {
            options: messaging_options(config, admitted, None),
            retained_messages: None,
        })
    }
    // template:end outbox:worker-bootstrap-outbox-messaging
}

pub(super) async fn connect_messaging(
    plan: MessagingPlan,
    // template:begin outbox:worker-bootstrap-connect-messaging-pool
    pool: Option<&infra_postgres::PgPool>,
    // template:end outbox:worker-bootstrap-connect-messaging-pool
    cancel: &CancellationToken,
    retained: &mut Option<BrokerResource>,
) -> Result<(Messaging, Option<Consumer>), WorkerError> {
    let MessagingPlan {
        options,
        retained_messages,
    } = plan;
    // template:begin outbox:worker-bootstrap-activate-postgres-messages
    let mut retained_messages = retained_messages;
    if let Some(messages) = &mut retained_messages {
        messages.register_postgres_messages(pool)?;
        messages
            .registry
            .check_publication_limit(options.max_payload_bytes)?;
    }
    // template:end outbox:worker-bootstrap-activate-postgres-messages
    let deadline = Instant::now() + MESSAGING_STARTUP_TIMEOUT;
    let startup = Messaging::prepare(options, deadline, cancel.child_token())?;
    let mut startup = std::mem::DropGuard::new((Box::new(startup), retained), |(startup, slot)| {
        *slot = Some(BrokerResource::Starting(startup));
    });
    let messaging = startup.0.admit().await?;
    let (_, slot) = std::mem::DropGuard::dismiss(startup);
    *slot = Some(BrokerResource::Admitted(messaging.clone()));
    // template:begin outbox:worker-bootstrap-admit-outbox-source
    messaging.outbox_producer()?;
    // template:end outbox:worker-bootstrap-admit-outbox-source
    let consumer = if let Some(messages) = retained_messages {
        Some(
            messaging
                .consumer(messages.registry, deadline, cancel)
                .await?,
        )
    } else {
        None
    };
    Ok((messaging, consumer))
}

fn messaging_options(
    config: &WorkerConfig,
    admitted: AdmittedMessaging<'_>,
    consumer: Option<ConsumerOptions>,
) -> MessagingOptions {
    let security = match admitted.security {
        MessagingTransportSecurity::Tls => TransportSecurity::Tls,
        MessagingTransportSecurity::TlsFirst => TransportSecurity::TlsFirst,
        MessagingTransportSecurity::AllowPlaintext => TransportSecurity::AllowPlaintext,
    };
    let credentials = match admitted.credentials {
        MessagingCredentials::Inline(secret) => Some(Credentials::Inline(secret.clone())),
        MessagingCredentials::File(path) => Some(Credentials::File(path.clone())),
        MessagingCredentials::Unauthenticated => None,
    };
    MessagingOptions {
        connection_name: worker_identity(&config.observability().otel.service_name),
        servers: admitted.urls.to_vec(),
        credentials,
        root_ca_path: admitted.root_ca_path.map(std::path::Path::to_path_buf),
        security,
        source_stream: admitted.source_stream.to_owned(),
        max_payload_bytes: admitted.max_payload_bytes,
        consumer,
    }
}

pub(super) fn start_consumer(
    consumer: Consumer,
    resources: &mut Resources,
    background: &Background,
) {
    resources.work.consumer = Some(consumer.start(background.cancel()));
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
