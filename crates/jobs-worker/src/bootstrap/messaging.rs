//! Broker admission and the typed-message consumer.

use std::time::Duration;

use futures_util::FutureExt;
use health::Probe;
use infra_messaging::{
    Consumer, ConsumerError, ConsumerOptions, Credentials, Messaging, MessagingOptions,
    Registry as MessagingRegistry,
};
use service_config::Config;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{Ended, WorkerError, pending_end, worker_identity};
use crate::Registration;
use crate::shutdown::{Background, Resources, Signals};

const MESSAGING_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether the registration consumes messages; when it does, the consumer
/// configuration must be complete before any dependency opens.
pub(super) fn consumes_messages(
    config: &Config,
    registration: &Registration<'_>,
) -> Result<bool, WorkerError> {
    let consumes_messages = registration.messages.has_handlers();
    // template:begin outbox:worker-bootstrap-postgres-messages-intent
    let consumes_messages = consumes_messages || registration.postgres_messages.is_some();
    // template:end outbox:worker-bootstrap-postgres-messages-intent
    if consumes_messages {
        config.messaging.validate_consumer(&config.app.env)?;
    }
    Ok(consumes_messages)
}

pub(super) async fn connect_messaging(
    config: &Config,
    messages: &mut Option<MessagingRegistry>,
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
    let startup = resources.messaging_startup.insert(Messaging::prepare(
        options,
        deadline,
        cancel.child_token(),
    )?);
    let messaging = startup.admit().await?;
    let messaging = resources.messaging.insert(messaging);
    resources.messaging_startup = None;
    if let Some(registry) = messages.take() {
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
        let keys = messaging.required_consumer()?;
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
        // Configuration validation refuses both sources at once.
        credentials: messaging
            .credentials
            .clone()
            .map(Credentials::Inline)
            .or_else(|| messaging.credentials_file.clone().map(Credentials::File)),
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
    let messaging = resources.messaging.as_ref()?;
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
