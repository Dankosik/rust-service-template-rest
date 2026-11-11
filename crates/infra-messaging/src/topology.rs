//! Broker and stream admission: what an answering broker must offer before
//! this adapter publishes or consumes, and how a refusal is classified.

use std::num::NonZeroUsize;

use async_nats::jetstream::consumer::PullConsumer;
use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::context::{
    GetStreamByNameErrorKind, GetStreamError, GetStreamErrorKind,
};
use async_nats::jetstream::stream::{
    Config as StreamConfig, ConsumerErrorKind, PersistenceMode, StorageType, Stream,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::budget::{BROKER_OPERATION_BUDGET, bounded_operation};
use crate::error::MessagingError;
use crate::messaging::{ConsumerOptions, MessagingOptions};
use crate::registry::Registry;
use crate::wire;

/// Oldest broker version this adapter admits; see the durable-messaging
/// guide for the server requirements.
const MIN_SERVER_VERSION: (i64, i64, i64) = (2, 12, 3);

/// A configured consumer, the dead-letter topology admitted for it, and the
/// startup budget its durable creation still runs under.
#[derive(Clone, Debug)]
pub(crate) struct ConsumerAdmission {
    pub(crate) options: ConsumerOptions,
    pub(crate) dead_letter: DeadLetterTopology,
    pub(crate) deadline: Instant,
    pub(crate) cancel: CancellationToken,
}

/// The topology already read at startup; consumer admission adds route lengths.
#[derive(Clone, Debug)]
pub(crate) struct DeadLetterTopology {
    pub(crate) stream: String,
    source_max_message_bytes: usize,
    /// The dead-letter stream's own limit; `None` means unlimited.
    dead_letter_max_message_bytes: Option<NonZeroUsize>,
}

impl DeadLetterTopology {
    /// Checks that a dead-letter transfer of any delivery on a route `filter`
    /// selects fits the header, server and dead-letter stream limits.
    ///
    /// Returns the byte length of the longest selected route subject.
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
        let subject_bytes = registry
            .routes()
            .filter(|route| wire::matches_filter(filter, &route.subject))
            .map(|route| route.subject.len())
            .max()
            .ok_or_else(bounds_unavailable)?;
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

/// Admits the source stream and, for a consumer, its dead-letter stream.
/// Retains the resolved limits until the complete consumer registry is available.
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
    // The stream's own message limit bounds what one delivery can hold in memory.
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
    validate_stream_publication(&dead_letter.cached_info().config)
        .map_err(|refusal| refusal_failure("dead_letter_stream", refusal, envelope_limit, None))?;
    Ok(Some(ConsumerAdmission {
        options: consumer.clone(),
        dead_letter: DeadLetterTopology {
            stream: dead_letter_name,
            source_max_message_bytes,
            dead_letter_max_message_bytes: usize::try_from(
                dead_letter.cached_info().config.max_message_size,
            )
            .ok()
            .and_then(NonZeroUsize::new),
        },
        deadline,
        cancel: cancel.clone(),
    }))
}

/// The source stream's message limit, which must be set and at most one
/// admitted envelope so a delivery cannot exceed what the adapter bounds.
fn source_message_limit(raw: i32, envelope_limit: usize) -> Result<usize, Refusal> {
    match usize::try_from(raw) {
        Ok(limit) if limit > envelope_limit => Err(Refusal::StreamMessageSize),
        Ok(limit @ 1..) => Ok(limit),
        _ => Err(Refusal::StreamMessageSizeUnset),
    }
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

/// Reads one stream; `role` names it in a failure log without the
/// configured stream name.
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

pub(crate) fn get_stream_failure(error: &GetStreamError, role: &'static str) -> MessagingError {
    let broker = match error.kind() {
        GetStreamErrorKind::JetStream(broker) => Some(broker),
        _ => None,
    };
    topology_failure("get_stream", Some(role), error, broker.as_ref())
}

/// Asks the broker which stream stores `subject`.
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

/// Creates the durable consumer, or updates its editable fields.
pub(crate) async fn create_durable(
    stream: &Stream,
    config: PullConfig,
) -> Result<PullConsumer, MessagingError> {
    stream.create_consumer(config).await.map_err(|error| {
        let broker = match error.kind() {
            ConsumerErrorKind::JetStream(broker) => Some(broker),
            _ => None,
        };
        topology_failure("create_consumer", None, &error, broker.as_ref())
    })
}

/// Classifies a failed topology request and logs which request failed.
///
/// `stream_role` says which stream a stream request read. `broker` is the
/// `JetStream` API error when the broker answered with one. Its numeric code
/// is a closed vocabulary; its description can quote configuration and stays
/// out of the log.
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

/// Why an answering broker cannot carry this adapter's deliveries.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Refusal {
    ServerVersion,
    JetStreamDisabled,
    HeadersUnsupported,
    ServerMaxPayload,
    StreamMessageSizeUnset,
    /// A stream's message limit is on the wrong side of the adapter's bound:
    /// the source stream's must be at most one envelope, so a delivery fits
    /// the pull budget; the dead-letter stream's must be at least one transfer.
    StreamMessageSize,
    DeadLetterIsSource,
    StreamMemoryStorage,
    StreamAsyncPersistence,
    StreamNoAck,
    DeadLetterBoundsUnavailable,
    DeadLetterHeaderSize,
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
            | Self::StreamMemoryStorage
            | Self::StreamAsyncPersistence
            | Self::StreamNoAck => MessagingError::Topology,
        }
    }
}

/// Classifies a refusal and logs which property of the broker caused it.
///
/// `required_bytes` is the admitted envelope or transfer bound; `limit_bytes`
/// is the corresponding broker limit, when the refusal concerns one. Neither
/// exposes message content.
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
    // The server bounds payload and headers together.
    if info.max_payload < envelope_limit {
        return Err(Refusal::ServerMaxPayload);
    }
    Ok(())
}

/// Whether the broker answered 401 or 403, which both mean the credentials
/// were refused.
fn is_auth_refusal(error: &async_nats::jetstream::Error) -> bool {
    matches!(error.code(), 401 | 403)
}

pub(crate) fn classify_topology(error: &(dyn std::error::Error + 'static)) -> MessagingError {
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
        && is_auth_refusal(error)
    {
        return MessagingError::Authentication;
    }
    error
        .source()
        .map_or(MessagingError::Topology, classify_topology)
}
