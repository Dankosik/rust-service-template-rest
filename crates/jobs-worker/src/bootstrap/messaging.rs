//! Broker admission and the typed-message consumer.

use std::time::Duration;

use futures_util::FutureExt;
use health::Probe;
use infra_messaging::{
    Consumer, ConsumerError, ConsumerOptions, Credentials, Messaging, MessagingError,
    MessagingOptions, Registry as MessagingRegistry,
};
use service_config::{Config, MessagingCredentials};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{Ended, WorkerError, pending_end, worker_identity};
use crate::Registration;
use crate::background::Background;
use crate::resources::{BrokerResource, Resources};
use crate::signals::Signals;

const MESSAGING_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct RetainedMessages {
    pub(super) registry: MessagingRegistry,
    // template:begin outbox:worker-bootstrap-postgres-messages-field
    pub(super) postgres_messages: Option<crate::PostgresMessages>,
    // template:end outbox:worker-bootstrap-postgres-messages-field
}

impl RetainedMessages {
    pub(super) fn retain(
        config: &Config,
        registration: Registration<'_>,
    ) -> Result<Option<Self>, WorkerError> {
        let consumes_messages = registration.messages.has_handlers();
        // template:begin outbox:worker-bootstrap-postgres-messages-intent
        let consumes_messages = consumes_messages || registration.postgres_messages.is_some();
        // template:end outbox:worker-bootstrap-postgres-messages-intent
        if !consumes_messages {
            return Ok(None);
        }
        config.messaging.validate_consumer(&config.app.env)?;
        Ok(Some(Self {
            registry: registration.messages,
            // template:begin outbox:worker-bootstrap-postgres-messages-value
            postgres_messages: registration.postgres_messages,
            // template:end outbox:worker-bootstrap-postgres-messages-value
        }))
    }
}

pub(super) async fn connect_messaging(
    config: &Config,
    messages: Option<MessagingRegistry>,
    cancel: &CancellationToken,
    resources: &mut Resources,
) -> Result<Option<Consumer>, WorkerError> {
    #[allow(
        unused_variables,
        reason = "retained outbox also requires broker admission"
    )]
    let needs_messaging = messages.is_some();
    // template:begin outbox:worker-bootstrap-outbox-messaging
    let needs_messaging = true;
    // template:end outbox:worker-bootstrap-outbox-messaging
    if !needs_messaging {
        return Ok(None);
    }
    let options = messaging_options(config, messages.is_some())?;
    let deadline = Instant::now() + MESSAGING_STARTUP_TIMEOUT;
    let startup = Messaging::prepare(options, deadline, cancel.child_token())?;
    let BrokerResource::Starting(startup) = resources
        .broker
        .insert(BrokerResource::Starting(Box::new(startup)))
    else {
        return Err(MessagingError::Connection.into());
    };
    let messaging = startup.admit().await?;
    drop(
        resources
            .broker
            .replace(BrokerResource::Admitted(messaging)),
    );
    if let Some(registry) = messages {
        let messaging = resources
            .admitted_messaging()
            .ok_or(MessagingError::Connection)?;
        Ok(Some(messaging.consumer(registry).await?))
    } else {
        Ok(None)
    }
}

fn messaging_options(
    config: &Config,
    needs_subscription: bool,
) -> Result<MessagingOptions, WorkerError> {
    let messaging = &config.messaging;
    let source_stream = messaging.required_source_stream()?.to_owned();
    let consumer = if needs_subscription {
        let keys = messaging.required_consumer_keys()?;
        Some(ConsumerOptions {
            durable_name: keys.durable.to_owned(),
            filter_subject: keys.filter_subject.to_owned(),
            dlq_subject: keys.dlq_subject.to_owned(),
            dlq_stream: None,
            concurrency: std::num::NonZeroUsize::try_from(messaging.consumer_concurrency)
                .map_err(|_| WorkerError::MessagingConcurrency)?,
        })
    } else {
        None
    };
    Ok(MessagingOptions {
        connection_name: worker_identity(&config.observability.otel.service_name),
        servers: messaging.urls.clone(),
        credentials: messaging.credential_source()?.map(|source| match source {
            MessagingCredentials::Inline(secret) => Credentials::Inline(secret.clone()),
            MessagingCredentials::File(path) => Credentials::File(path.to_path_buf()),
        }),
        root_ca_path: messaging.root_ca_path.clone(),
        allow_plaintext: messaging.plaintext_admitted(),
        tls_first: messaging.tls_first,
        source_stream,
        max_payload_bytes: usize::try_from(messaging.max_payload_bytes.as_u64())
            .map_err(|_| WorkerError::MessagingPayloadBound)?,
        consumer,
    })
}

pub(super) fn probe(resources: &Resources) -> Option<Box<dyn Probe>> {
    let messaging = resources.admitted_messaging()?;
    Some(Box::new(messaging.probe()))
}

pub(super) fn start_consumer(
    consumer: Option<Consumer>,
    resources: &mut Resources,
    background: &Background,
    signals: &mut Signals,
) -> Option<Ended> {
    if let Some(consumer) = consumer {
        if let Some(ended) = pending_end(resources, background, signals) {
            return Some(ended);
        }
        resources.consumer = Some(consumer.start(&background.cancel));
        tracing::info!("messaging_consuming_started");
    }
    None
}

pub(super) fn consumer_failure(resources: &Resources) -> Option<WorkerError> {
    let error = resources.consumer.as_ref()?.failed().now_or_never()?;
    Some(WorkerError::ConsumerStopped(error))
}

pub(super) async fn consumer_stopped(resources: &Resources) -> ConsumerError {
    if let Some(consumer) = resources.consumer.as_ref() {
        consumer.failed().await
    } else {
        std::future::pending::<ConsumerError>().await
    }
}

#[cfg(test)]
mod tests {
    use crate::bootstrap::WorkerError;

    #[test]
    fn refusal_texts_name_the_failed_check() {
        assert_eq!(
            WorkerError::ConsumerStopped(infra_messaging::ConsumerError::ConsumerLost).to_string(),
            "the messaging consumer stopped without a stop signal: messaging durable consumer was deleted or replaced"
        );
    }
}
