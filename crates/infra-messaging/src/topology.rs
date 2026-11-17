use std::num::NonZeroUsize;

use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::consumer::{Config as ConsumerConfig, IntoConsumerConfig, PullConsumer};
use async_nats::jetstream::context::{
    GetStreamByNameErrorKind, GetStreamError, GetStreamErrorKind,
};
use async_nats::jetstream::stream::{
    Config as StreamConfig, ConsumerCreateStrictErrorKind, ConsumerErrorKind, PersistenceMode,
    StorageType, Stream,
};
use async_nats::jetstream::{Context, ErrorCode};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::budget::{BROKER_OPERATION_BUDGET, bounded_operation};
use crate::error::MessagingError;
use crate::messaging::{ConsumerOptions, MessagingOptions};
use crate::registry::Registry;
use crate::wire;

const MIN_SERVER_VERSION: (i64, i64, i64) = (2, 12, 3);

const WILDCARD_SUBJECT_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct ConsumerAdmission {
    pub(crate) options: ConsumerOptions,
    pub(crate) dead_letter: DeadLetterTopology,
}

#[derive(Clone, Debug)]
pub(crate) struct DeadLetterTopology {
    pub(crate) stream: String,
    source_max_message_bytes: usize,
    dead_letter_max_message_bytes: Option<NonZeroUsize>,
}

impl DeadLetterTopology {
    pub(crate) fn admit_routes(
        &self,
        registry: &Registry,
        filter: &str,
        server_limit: usize,
    ) -> Result<usize, MessagingError> {
        let bounds_unavailable = || {
            refusal_failure(
                "dead_letter_stream",
                Refusal::DeadLetterBoundsUnavailable,
                0,
                None,
            )
        };
        let subject_bytes = if filter.contains(['*', '>']) {
            registry
                .routes()
                .filter(|route| wire::matches_filter(filter, &route.subject))
                .map(|route| route.subject.len())
                .fold(WILDCARD_SUBJECT_BYTES, usize::max)
        } else {
            filter.len()
        };
        let bounds = wire::dead_letter_bounds(
            self.source_max_message_bytes,
            subject_bytes,
            self.stream.len(),
        )
        .ok_or_else(bounds_unavailable)?;
        if bounds.headers > wire::NATIVE_HEADER_LIMIT_BYTES {
            return Err(refusal_failure(
                "dead_letter_stream",
                Refusal::DeadLetterHeaderSize,
                bounds.headers,
                i64::try_from(wire::NATIVE_HEADER_LIMIT_BYTES).ok(),
            ));
        }
        if bounds.total > server_limit {
            return Err(refusal_failure(
                "server",
                Refusal::ServerMaxPayload,
                bounds.total,
                i64::try_from(server_limit).ok(),
            ));
        }
        if let Some(limit) = self
            .dead_letter_max_message_bytes
            .filter(|limit| limit.get() < bounds.total)
        {
            return Err(refusal_failure(
                "dead_letter_stream",
                Refusal::StreamMessageSize,
                bounds.total,
                i64::try_from(limit.get()).ok(),
            ));
        }
        Ok(subject_bytes)
    }
}

pub(crate) async fn admit_topology(
    options: &MessagingOptions,
    envelope_limit: usize,
    client: &async_nats::Client,
    jetstream: &async_nats::jetstream::Context,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<Option<ConsumerAdmission>, MessagingError> {
    validate_server(client, envelope_limit).map_err(|refusal| {
        refusal_failure(
            "server",
            refusal,
            envelope_limit,
            i64::try_from(client.server_info().max_payload).ok(),
        )
    })?;
    let source = get_stream(
        jetstream,
        &options.source_stream,
        "source",
        deadline,
        cancel,
    )
    .await?;
    validate_stream_publication(&source.cached_info().config)
        .map_err(|refusal| refusal_failure("source_stream", refusal, envelope_limit, None))?;
    let Some(consumer) = &options.consumer else {
        return Ok(None);
    };
    let raw_source_limit = source.cached_info().config.max_message_size;
    let source_max_message_bytes =
        source_message_limit(raw_source_limit, envelope_limit).map_err(|refusal| {
            refusal_failure(
                "source_stream",
                refusal,
                envelope_limit,
                Some(raw_source_limit.into()),
            )
        })?;
    let dead_letter_name = match &consumer.dlq_stream {
        Some(name) => name.clone(),
        None => stream_name_for_subject(jetstream, &consumer.dlq_subject, deadline, cancel).await?,
    };
    if dead_letter_name == options.source_stream {
        return Err(refusal_failure(
            "dead_letter_stream",
            Refusal::DeadLetterIsSource,
            envelope_limit,
            None,
        ));
    }
    let dead_letter = get_stream(
        jetstream,
        &dead_letter_name,
        "dead_letter",
        deadline,
        cancel,
    )
    .await?;
    let dead_letter_config = &dead_letter.cached_info().config;
    validate_stream_publication(dead_letter_config)
        .map_err(|refusal| refusal_failure("dead_letter_stream", refusal, envelope_limit, None))?;
    if !wire::valid_subject(&consumer.dlq_subject)
        || !dead_letter_config
            .subjects
            .iter()
            .any(|filter| wire::matches_filter(filter, &consumer.dlq_subject))
    {
        return Err(refusal_failure(
            "dead_letter_stream",
            Refusal::DeadLetterSubjectMismatch,
            envelope_limit,
            None,
        ));
    }
    Ok(Some(ConsumerAdmission {
        options: consumer.clone(),
        dead_letter: DeadLetterTopology {
            stream: dead_letter_name,
            source_max_message_bytes,
            dead_letter_max_message_bytes: usize::try_from(dead_letter_config.max_message_size)
                .ok()
                .and_then(NonZeroUsize::new),
        },
    }))
}

fn source_message_limit(raw: i32, envelope_limit: usize) -> Result<usize, Refusal> {
    match usize::try_from(raw) {
        Ok(limit) if limit > envelope_limit => Err(Refusal::StreamMessageSize),
        Ok(limit @ 1..) => Ok(limit),
        _ => Err(Refusal::StreamMessageSizeUnset),
    }
}

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
    role: &'static str,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<Stream, MessagingError> {
    bounded_operation(deadline, cancel, async {
        jetstream
            .get_stream(name)
            .await
            .map_err(|error| get_stream_failure(&error, role))
    })
    .await
}

fn get_stream_failure(error: &GetStreamError, role: &'static str) -> MessagingError {
    let broker = match error.kind() {
        GetStreamErrorKind::JetStream(broker) => Some(broker),
        _ => None,
    };
    topology_failure("get_stream", Some(role), error, broker.as_ref())
}

async fn stream_name_for_subject(
    jetstream: &async_nats::jetstream::Context,
    subject: &str,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<String, MessagingError> {
    bounded_operation(deadline, cancel, async {
        jetstream
            .stream_by_subject(subject.to_owned())
            .await
            .map_err(|error| {
                let broker = match error.kind() {
                    GetStreamByNameErrorKind::JetStream(broker) => Some(broker),
                    _ => None,
                };
                topology_failure("stream_by_subject", None, &error, broker.as_ref())
            })
    })
    .await
}

async fn find_durable(
    jetstream: &Context,
    stream: &str,
    name: &str,
) -> Result<Option<PullConsumer>, MessagingError> {
    match jetstream.get_consumer_from_stream(name, stream).await {
        Ok(pull) => Ok(Some(pull)),
        Err(error) => match error.kind() {
            ConsumerErrorKind::JetStream(broker)
                if broker.error_code() == ErrorCode::CONSUMER_NOT_FOUND =>
            {
                Ok(None)
            }
            ConsumerErrorKind::JetStream(broker) => Err(topology_failure(
                "get_consumer",
                None,
                &error,
                Some(&broker),
            )),
            ConsumerErrorKind::TimedOut => Err(MessagingError::TimedOut {
                budget: BROKER_OPERATION_BUDGET,
            }),
            _ => Err(topology_failure("get_consumer", None, &error, None)),
        },
    }
}

pub(crate) async fn bind_durable(
    jetstream: &Context,
    stream: &str,
    name: &str,
    config: PullConfig,
) -> Result<PullConsumer, MessagingError> {
    let pull = match find_durable(jetstream, stream, name).await? {
        Some(pull) => pull,
        None => match jetstream
            .create_consumer_strict_on_stream(config.clone(), stream)
            .await
        {
            Ok(pull) => pull,
            Err(error) => match error.kind() {
                ConsumerCreateStrictErrorKind::AlreadyExists => {
                    find_durable(jetstream, stream, name)
                        .await?
                        .ok_or(MessagingError::Topology)?
                }
                ConsumerCreateStrictErrorKind::JetStream(broker) => {
                    return Err(topology_failure(
                        "create_consumer",
                        None,
                        &error,
                        Some(&broker),
                    ));
                }
                ConsumerCreateStrictErrorKind::TimedOut => {
                    return Err(MessagingError::TimedOut {
                        budget: BROKER_OPERATION_BUDGET,
                    });
                }
                _ => return Err(topology_failure("create_consumer", None, &error, None)),
            },
        },
    };
    if !same_durable_contract(&config.into_consumer_config(), &pull.cached_info().config) {
        tracing::warn!(operation = "bind_consumer", error.type = "durable_configuration", reason = %MessagingError::Topology, "messaging_admission_failed");
        return Err(MessagingError::Topology);
    }
    Ok(pull)
}

pub(crate) fn same_durable_contract(expected: &ConsumerConfig, actual: &ConsumerConfig) -> bool {
    fn filters(config: &ConsumerConfig) -> impl Iterator<Item = &str> {
        std::iter::once(config.filter_subject.as_str())
            .filter(|filter| !filter.is_empty())
            .chain(config.filter_subjects.iter().map(String::as_str))
    }
    expected.durable_name == actual.durable_name
        && expected.deliver_subject == actual.deliver_subject
        && filters(expected).eq(filters(actual))
        && expected.deliver_policy == actual.deliver_policy
        && expected.ack_policy == actual.ack_policy
        && expected.ack_wait == actual.ack_wait
        && expected.max_deliver == actual.max_deliver
        && expected.replay_policy == actual.replay_policy
        && expected.backoff == actual.backoff
        && expected.headers_only == actual.headers_only
}

fn topology_failure(
    operation: &'static str,
    stream_role: Option<&'static str>,
    error: &(dyn std::error::Error + 'static),
    broker: Option<&async_nats::jetstream::Error>,
) -> MessagingError {
    let failure = match broker {
        Some(broker) if is_auth_refusal(broker) => MessagingError::Authentication,
        Some(_) => MessagingError::Topology,
        None => classify_topology(error),
    };
    tracing::warn!(
        operation,
        stream_role,
        reason = %failure,
        jetstream.error_code = broker.map(|broker| broker.error_code().0),
        "messaging_admission_failed"
    );
    failure
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Refusal {
    ServerVersion,
    JetStreamDisabled,
    HeadersUnsupported,
    ServerMaxPayload,
    StreamMessageSizeUnset,
    StreamMessageSize,
    DeadLetterIsSource,
    DeadLetterSubjectMismatch,
    StreamMemoryStorage,
    StreamAsyncPersistence,
    StreamNoAck,
    DeadLetterBoundsUnavailable,
    DeadLetterHeaderSize,
}

impl Refusal {
    pub(crate) const fn error_type(self) -> &'static str {
        match self {
            Self::ServerVersion => "server_version",
            Self::JetStreamDisabled => "jetstream_disabled",
            Self::HeadersUnsupported => "headers_unsupported",
            Self::ServerMaxPayload => "server_max_payload",
            Self::StreamMessageSizeUnset => "stream_max_message_size_unset",
            Self::StreamMessageSize => "stream_max_message_size",
            Self::DeadLetterIsSource => "dead_letter_stream_is_source",
            Self::DeadLetterSubjectMismatch => "dead_letter_subject_mismatch",
            Self::StreamMemoryStorage => "stream_memory_storage",
            Self::StreamAsyncPersistence => "stream_async_persistence",
            Self::StreamNoAck => "stream_no_ack",
            Self::DeadLetterBoundsUnavailable => "dead_letter_bounds_unavailable",
            Self::DeadLetterHeaderSize => "dead_letter_header_bytes",
        }
    }

    const fn failure(self) -> MessagingError {
        match self {
            Self::ServerMaxPayload
            | Self::StreamMessageSizeUnset
            | Self::StreamMessageSize
            | Self::DeadLetterBoundsUnavailable
            | Self::DeadLetterHeaderSize => MessagingError::Bounds,
            Self::ServerVersion
            | Self::JetStreamDisabled
            | Self::HeadersUnsupported
            | Self::DeadLetterIsSource
            | Self::DeadLetterSubjectMismatch
            | Self::StreamMemoryStorage
            | Self::StreamAsyncPersistence
            | Self::StreamNoAck => MessagingError::Topology,
        }
    }
}

fn refusal_failure(
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

pub(crate) fn validate_server(
    client: &async_nats::Client,
    envelope_limit: usize,
) -> Result<(), Refusal> {
    let info = client.server_info();
    let (major, minor, patch) = MIN_SERVER_VERSION;
    if !client.is_server_compatible(major, minor, patch) {
        return Err(Refusal::ServerVersion);
    }
    if !info.jetstream {
        return Err(Refusal::JetStreamDisabled);
    }
    if !info.headers {
        return Err(Refusal::HeadersUnsupported);
    }
    if info.max_payload < envelope_limit {
        return Err(Refusal::ServerMaxPayload);
    }
    Ok(())
}

fn is_auth_refusal(error: &async_nats::jetstream::Error) -> bool {
    matches!(error.code(), 401 | 403)
}

pub(crate) fn classify_topology(error: &(dyn std::error::Error + 'static)) -> MessagingError {
    shared_rules::error_chain::causes(error)
        .find_map(classify_cause)
        .unwrap_or(MessagingError::Topology)
}

fn classify_cause(error: &(dyn std::error::Error + 'static)) -> Option<MessagingError> {
    if let Some(error) = error.downcast_ref::<async_nats::jetstream::context::RequestError>() {
        match error.kind() {
            async_nats::jetstream::context::RequestErrorKind::TimedOut => {
                return Some(MessagingError::TimedOut {
                    budget: BROKER_OPERATION_BUDGET,
                });
            }
            async_nats::jetstream::context::RequestErrorKind::InvalidSubject => {
                return Some(MessagingError::Configuration("broker subject is invalid"));
            }
            _ => {}
        }
    }
    error
        .downcast_ref::<async_nats::jetstream::Error>()
        .is_some_and(is_auth_refusal)
        .then_some(MessagingError::Authentication)
}

#[cfg(test)]
mod tests {
    use async_nats::jetstream::context::{RequestError, RequestErrorKind};

    use super::classify_topology;
    use crate::error::MessagingError;

    #[derive(Debug, thiserror::Error)]
    #[error("stream lookup failed")]
    struct Wrapper(#[source] Box<dyn std::error::Error + 'static>);

    #[test]
    fn a_timed_out_request_is_found_at_any_depth() {
        let direct = RequestError::new(RequestErrorKind::TimedOut);
        let wrapped = Wrapper(Box::new(RequestError::new(RequestErrorKind::TimedOut)));
        let in_io = Wrapper(Box::new(std::io::Error::other(RequestError::new(
            RequestErrorKind::TimedOut,
        ))));
        for error in [
            &direct as &(dyn std::error::Error + 'static),
            &wrapped,
            &in_io,
        ] {
            std::assert_matches!(
                classify_topology(error),
                MessagingError::TimedOut { .. },
                "{error}"
            );
        }
    }

    #[test]
    fn an_unrecognized_chain_is_a_topology_failure() {
        let error = Wrapper(Box::new(RequestError::new(RequestErrorKind::Other)));
        std::assert_matches!(classify_topology(&error), MessagingError::Topology);
    }
}
