use std::collections::HashMap;
use std::num::NonZeroUsize;

use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::consumer::{Config as ConsumerConfig, IntoConsumerConfig, PullConsumer};
use async_nats::jetstream::context::{
    GetStreamByNameErrorKind, GetStreamError, GetStreamErrorKind,
};
use async_nats::jetstream::response::Response;
use async_nats::jetstream::stream::{
    Config as StreamConfig, ConsumerCreateStrictErrorKind, ConsumerErrorKind, Info as StreamInfo,
    PersistenceMode, RetentionPolicy, StorageType, Stream,
};
// template:begin outbox:messaging-outbox-topology-imports
use async_nats::jetstream::stream::{DiscardPolicy, SubjectTransform};

use crate::budget::{BrokerBudget, bounded_with};
// template:end outbox:messaging-outbox-topology-imports
use async_nats::jetstream::{Context, ErrorCode};
use operation_context::OperationContext;
use sha2::{Digest as _, Sha256};

use crate::budget::{BROKER_OPERATION_BUDGET, bounded_operation};
use crate::error::MessagingError;
use crate::messaging::{ConsumerOptions, MessagingOptions};
use crate::registry::Registry;
use crate::wire;

const MIN_SERVER_VERSION: (i64, i64, i64) = (2, 12, 3);

const WILDCARD_SUBJECT_BYTES: usize = 4 * 1024;
const DURABLE_CONTRACT: &str = "service.delivery-contract.v1";
const BROKER_REQUEST_TIMED_OUT: MessagingError = MessagingError::TimedOut {
    budget: BROKER_OPERATION_BUDGET,
};

impl Registry {
    #[must_use]
    pub fn durable_metadata(&self, dlq_stream: &str, dlq_subject: &str) -> HashMap<String, String> {
        fn hash_text(hash: &mut Sha256, text: &str) {
            hash.update((text.len() as u64).to_be_bytes());
            hash.update(text.as_bytes());
        }

        let mut routes: Vec<_> = self
            .handled_routes()
            .map(|(key, route)| (route.subject.as_str(), key.event_type, key.schema_version))
            .collect();
        routes.sort_unstable();
        let mut hash = Sha256::new();
        hash_text(&mut hash, dlq_stream);
        hash_text(&mut hash, dlq_subject);
        for (subject, event_type, version) in routes {
            hash_text(&mut hash, subject);
            hash_text(&mut hash, event_type);
            hash.update(version.to_be_bytes());
        }
        HashMap::from([(
            DURABLE_CONTRACT.to_owned(),
            wire::prefixed_digest_hex("sha256-", &hash.finalize().into()),
        )])
    }
}

#[derive(Debug)]
pub(crate) struct TopologyAdmission {
    pub(crate) consumer: Option<ConsumerAdmission>,
    // template:begin outbox:messaging-outbox-topology-field
    pub(crate) outbox_source: Result<(), OutboxSourceRefusal>,
    // template:end outbox:messaging-outbox-topology-field
}

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
    pub(crate) fn admit_max_subject_bytes(
        &self,
        registry: &Registry,
        filter: &str,
        server_max_payload_bytes: usize,
    ) -> Result<usize, MessagingError> {
        let subject_bytes = if filter.contains(['*', '>']) {
            registry
                .routes()
                .filter(|(_, route)| wire::matches_filter(filter, &route.subject))
                .map(|(_, route)| route.subject.len())
                .fold(WILDCARD_SUBJECT_BYTES, usize::max)
        } else {
            filter.len()
        };
        let bounds = wire::dead_letter_bounds(
            self.source_max_message_bytes,
            subject_bytes,
            self.stream.len(),
        )
        .ok_or_else(|| {
            Refusal::DeadLetterBoundsUnavailable.report("dead_letter_stream", 0, None)
        })?;
        if bounds.header_bytes > wire::NATIVE_HEADER_LIMIT_BYTES {
            return Err(Refusal::DeadLetterHeaderSize.report(
                "dead_letter_stream",
                bounds.header_bytes,
                i64::try_from(wire::NATIVE_HEADER_LIMIT_BYTES).ok(),
            ));
        }
        if bounds.total_bytes > server_max_payload_bytes {
            return Err(Refusal::ServerMaxPayload.report(
                "server",
                bounds.total_bytes,
                i64::try_from(server_max_payload_bytes).ok(),
            ));
        }
        if let Some(limit) = self
            .dead_letter_max_message_bytes
            .filter(|limit| limit.get() < bounds.total_bytes)
        {
            return Err(Refusal::StreamMessageSize.report(
                "dead_letter_stream",
                bounds.total_bytes,
                i64::try_from(limit.get()).ok(),
            ));
        }
        Ok(subject_bytes)
    }
}

pub(crate) async fn admit_topology(
    options: &MessagingOptions,
    envelope_limit_bytes: usize,
    client: &async_nats::Client,
    jetstream: &async_nats::jetstream::Context,
    context: &OperationContext,
) -> Result<TopologyAdmission, MessagingError> {
    validate_server(client, envelope_limit_bytes).map_err(|refusal| {
        refusal.report(
            "server",
            envelope_limit_bytes,
            i64::try_from(client.server_info().max_payload).ok(),
        )
    })?;
    let source = get_stream(jetstream, &options.source_stream, "source", context).await?;
    let source_config = &source.cached_info().config;
    validate_stream_publication(source_config)
        .map_err(|refusal| refusal.report("source_stream", envelope_limit_bytes, None))?;
    let consumer = match &options.consumer {
        Some(consumer) => Some(
            admit_consumer_topology(
                consumer,
                &options.source_stream,
                source_config.max_message_size,
                envelope_limit_bytes,
                jetstream,
                context,
            )
            .await?,
        ),
        None => None,
    };
    Ok(TopologyAdmission {
        consumer,
        // template:begin outbox:messaging-outbox-topology-value
        outbox_source: validate_outbox_source(source_config),
        // template:end outbox:messaging-outbox-topology-value
    })
}

fn dead_letter_subject_covered(config: &StreamConfig, subject: &str) -> bool {
    wire::valid_subject(subject)
        && config
            .subjects
            .iter()
            .any(|filter| wire::matches_filter(filter, subject))
}

async fn admit_consumer_topology(
    consumer: &ConsumerOptions,
    source_stream: &str,
    source_max_message_size: i32,
    envelope_limit_bytes: usize,
    jetstream: &async_nats::jetstream::Context,
    context: &OperationContext,
) -> Result<ConsumerAdmission, MessagingError> {
    let source_max_message_bytes =
        admit_source_max_message_bytes(source_max_message_size, envelope_limit_bytes).map_err(
            |refusal| {
                refusal.report(
                    "source_stream",
                    envelope_limit_bytes,
                    Some(source_max_message_size.into()),
                )
            },
        )?;
    let dead_letter_name = match &consumer.dlq_stream {
        Some(name) => name.clone(),
        None => stream_name_for_subject(jetstream, &consumer.dlq_subject, context).await?,
    };
    if dead_letter_name == source_stream {
        return Err(Refusal::DeadLetterIsSource.report(
            "dead_letter_stream",
            envelope_limit_bytes,
            None,
        ));
    }
    let dead_letter = get_stream(jetstream, &dead_letter_name, "dead_letter", context).await?;
    let dead_letter_config = &dead_letter.cached_info().config;
    validate_stream_publication(dead_letter_config)
        .map_err(|refusal| refusal.report("dead_letter_stream", envelope_limit_bytes, None))?;
    if !dead_letter_subject_covered(dead_letter_config, &consumer.dlq_subject) {
        return Err(Refusal::DeadLetterSubjectMismatch.report(
            "dead_letter_stream",
            envelope_limit_bytes,
            None,
        ));
    }
    Ok(ConsumerAdmission {
        options: consumer.clone(),
        dead_letter: DeadLetterTopology {
            stream: dead_letter_name,
            source_max_message_bytes,
            dead_letter_max_message_bytes: usize::try_from(dead_letter_config.max_message_size)
                .ok()
                .and_then(NonZeroUsize::new),
        },
    })
}

fn admit_source_max_message_bytes(raw: i32, envelope_limit_bytes: usize) -> Result<usize, Refusal> {
    match usize::try_from(raw) {
        Ok(limit) if limit > envelope_limit_bytes => Err(Refusal::StreamMessageSize),
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
    if config.retention == RetentionPolicy::Interest {
        return Err(Refusal::StreamInterestRetention);
    }
    if config.sealed {
        return Err(Refusal::StreamSealed);
    }
    Ok(())
}

// template:begin outbox:messaging-outbox-topology-validation
#[derive(Clone, Copy, Debug)]
pub(crate) enum OutboxSourceRefusal {
    DiscardOld,
    DiscardOldPerSubject,
    SubjectTransform,
}

impl OutboxSourceRefusal {
    const fn error_type(self) -> &'static str {
        match self {
            Self::DiscardOld => "outbox_discard_old",
            Self::DiscardOldPerSubject => "outbox_discard_old_per_subject",
            Self::SubjectTransform => "outbox_subject_transform",
        }
    }

    const fn failure(self) -> MessagingError {
        match self {
            Self::DiscardOld => {
                MessagingError::OutboxSource("bounded message or byte capacity requires DiscardNew")
            }
            Self::DiscardOldPerSubject => MessagingError::OutboxSource(
                "a per-subject message limit requires DiscardNewPerSubject",
            ),
            Self::SubjectTransform => MessagingError::OutboxSource(
                "subject transform must preserve the publication subject",
            ),
        }
    }

    pub(crate) fn report(self, required_bytes: usize) -> MessagingError {
        let failure = self.failure();
        log_admission_refusal(
            "source_stream",
            failure,
            self.error_type(),
            required_bytes,
            None,
        );
        failure
    }
}

fn validate_outbox_source(config: &StreamConfig) -> Result<(), OutboxSourceRefusal> {
    if (config.max_messages > 0 || config.max_bytes > 0) && config.discard != DiscardPolicy::New {
        return Err(OutboxSourceRefusal::DiscardOld);
    }
    if config.max_messages_per_subject > 0 && !config.discard_new_per_subject {
        return Err(OutboxSourceRefusal::DiscardOldPerSubject);
    }
    if config
        .subject_transform
        .as_ref()
        .is_some_and(|transform| !preserves_subject(transform))
    {
        return Err(OutboxSourceRefusal::SubjectTransform);
    }
    Ok(())
}

fn preserves_subject(transform: &SubjectTransform) -> bool {
    if transform.destination.is_empty() {
        return true;
    }
    let source = if transform.source.is_empty() {
        ">"
    } else {
        &transform.source
    };
    let mut destination = transform.destination.split('.');
    let mut capture = 0_usize;
    for token in source.split('.') {
        let Some(output) = destination.next() else {
            return false;
        };
        if token == "*" {
            capture += 1;
            if wildcard_index(output) != Some(capture) {
                return false;
            }
        } else if output != token || is_transform_reference(output) {
            return false;
        }
    }
    destination.next().is_none()
}

fn is_transform_reference(output: &str) -> bool {
    output
        .strip_prefix('$')
        .is_some_and(|value| value.parse::<i64>().is_ok())
        || (output.len() > 4 && output.starts_with("{{") && output.ends_with("}}"))
}

fn wildcard_index(output: &str) -> Option<usize> {
    let index_text = output.strip_prefix('$').or_else(|| {
        let function = output.strip_circumfix("{{", "}}")?.trim_ascii();
        let argument = function
            .strip_prefix("wildcard")
            .or_else(|| function.strip_prefix("Wildcard"))?
            .trim_ascii_start()
            .strip_circumfix('(', ')')?;
        Some(argument.trim_matches(' '))
    });
    index_text.and_then(|digits| digits.parse::<usize>().ok())
}

pub(crate) async fn recheck_outbox_source(
    shared: &crate::messaging::Shared,
    budget: BrokerBudget<'_>,
) -> Result<(), crate::PublishError> {
    use crate::PublishRejection;

    let info = bounded_with(budget, async {
        map_stream_error(
            fetch_stream_info(&shared.jetstream, &shared.source_stream_info_subject).await,
            "source",
        )
    })
    .await
    .map_err(|error| match error {
        MessagingError::Cancelled | MessagingError::TimedOut { .. } => {
            PublishRejection::ContextStopped
        }
        MessagingError::Configuration(_)
        | MessagingError::Connection
        | MessagingError::Authentication
        | MessagingError::Topology
        | MessagingError::OutboxSource(_)
        | MessagingError::Unavailable
        | MessagingError::Bounds => PublishRejection::BrokerRefused,
    })?;
    let config = &info.config;
    if let Err(refusal) = validate_stream_publication(config) {
        let _ = refusal.report("source_stream", shared.envelope_limit_bytes, None);
        return Err(PublishRejection::BrokerRefused.into());
    }
    if let Err(refusal) = validate_outbox_source(config) {
        let _ = refusal.report(shared.envelope_limit_bytes);
        return Err(PublishRejection::BrokerRefused.into());
    }
    if shared.refuses_work() {
        return Err(PublishRejection::Unavailable.into());
    }
    Ok(())
}
// template:end outbox:messaging-outbox-topology-validation

pub(crate) async fn fetch_stream_info(
    jetstream: &Context,
    info_subject: &async_nats::Subject,
) -> Result<StreamInfo, GetStreamError> {
    let response: Response<StreamInfo> = jetstream
        .request(wire::BorrowedSubject(info_subject), &())
        .await
        .map_err(|error| GetStreamError::with_source(GetStreamErrorKind::Request, error))?;
    match response {
        Response::Err { error } => Err(GetStreamError::new(GetStreamErrorKind::JetStream(error))),
        Response::Ok(info) => Ok(info),
    }
}

fn map_stream_error<T>(
    result: Result<T, GetStreamError>,
    role: &'static str,
) -> Result<T, MessagingError> {
    result.map_err(|error| {
        let broker = stream_broker_error(&error);
        report_topology_failure("get_stream", Some(role), &error, broker.as_ref())
    })
}

async fn get_stream(
    jetstream: &async_nats::jetstream::Context,
    name: &str,
    role: &'static str,
    context: &OperationContext,
) -> Result<Stream, MessagingError> {
    bounded_operation(context, async {
        map_stream_error(jetstream.get_stream(name).await, role)
    })
    .await
}

#[warn(clippy::wildcard_enum_match_arm)]
fn stream_broker_error(error: &GetStreamError) -> Option<async_nats::jetstream::Error> {
    match error.kind() {
        GetStreamErrorKind::JetStream(broker) => Some(broker),
        GetStreamErrorKind::EmptyName
        | GetStreamErrorKind::Request
        | GetStreamErrorKind::InvalidStreamName => None,
    }
}

async fn stream_name_for_subject(
    jetstream: &async_nats::jetstream::Context,
    subject: &str,
    context: &OperationContext,
) -> Result<String, MessagingError> {
    bounded_operation(context, async {
        jetstream
            .stream_by_subject(subject.to_owned())
            .await
            .map_err(|error| {
                #[warn(clippy::wildcard_enum_match_arm)]
                let broker = match error.kind() {
                    GetStreamByNameErrorKind::JetStream(broker) => Some(broker),
                    GetStreamByNameErrorKind::Request
                    | GetStreamByNameErrorKind::NotFound
                    | GetStreamByNameErrorKind::InvalidSubject => None,
                };
                report_topology_failure("stream_by_subject", None, &error, broker.as_ref())
            })
    })
    .await
}

#[warn(clippy::wildcard_enum_match_arm)]
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
            ConsumerErrorKind::JetStream(broker) => Err(report_topology_failure(
                "get_consumer",
                None,
                &error,
                Some(&broker),
            )),
            ConsumerErrorKind::TimedOut => Err(BROKER_REQUEST_TIMED_OUT),
            ConsumerErrorKind::Request
            | ConsumerErrorKind::InvalidConsumerType
            | ConsumerErrorKind::InvalidName
            | ConsumerErrorKind::Other => {
                Err(report_topology_failure("get_consumer", None, &error, None))
            }
        },
    }
}

pub(crate) async fn bind_durable(
    jetstream: &Context,
    stream: &str,
    name: &str,
    config: PullConfig,
) -> Result<PullConsumer, MessagingError> {
    let existing = find_durable(jetstream, stream, name).await?;
    let pull = match existing {
        Some(pull) => pull,
        None => create_or_find_durable(jetstream, stream, name, config.clone()).await?,
    };
    let expected = config.into_consumer_config();
    if let Some(error_type) = durable_contract_error_type(&expected, &pull.cached_info().config) {
        return Err(report_bind_refusal(
            error_type,
            expected.metadata.get(DURABLE_CONTRACT).map(String::as_str),
        ));
    }
    Ok(pull)
}

#[warn(clippy::wildcard_enum_match_arm)]
async fn create_or_find_durable(
    jetstream: &Context,
    stream: &str,
    name: &str,
    config: PullConfig,
) -> Result<PullConsumer, MessagingError> {
    let error = match jetstream
        .create_consumer_strict_on_stream(config, stream)
        .await
    {
        Ok(pull) => return Ok(pull),
        Err(error) => error,
    };
    match error.kind() {
        ConsumerCreateStrictErrorKind::AlreadyExists => find_durable(jetstream, stream, name)
            .await?
            .ok_or(MessagingError::Topology),
        ConsumerCreateStrictErrorKind::JetStream(broker) => Err(report_topology_failure(
            "create_consumer",
            None,
            &error,
            Some(&broker),
        )),
        ConsumerCreateStrictErrorKind::TimedOut => Err(BROKER_REQUEST_TIMED_OUT),
        ConsumerCreateStrictErrorKind::Request
        | ConsumerCreateStrictErrorKind::InvalidConsumerType
        | ConsumerCreateStrictErrorKind::InvalidName
        | ConsumerCreateStrictErrorKind::Other => Err(report_topology_failure(
            "create_consumer",
            None,
            &error,
            None,
        )),
    }
}

pub(crate) fn durable_contract_error_type(
    expected: &ConsumerConfig,
    actual: &ConsumerConfig,
) -> Option<&'static str> {
    fn filters(config: &ConsumerConfig) -> impl Iterator<Item = &str> {
        std::iter::once(config.filter_subject.as_str())
            .filter(|filter| !filter.is_empty())
            .chain(config.filter_subjects.iter().map(String::as_str))
    }
    if !actual.inactive_threshold.is_zero() {
        return Some("durable_inactive_threshold");
    }
    if actual.memory_storage {
        return Some("durable_memory_storage");
    }
    let compatible = expected.durable_name == actual.durable_name
        && expected
            .metadata
            .get(DURABLE_CONTRACT)
            .is_some_and(|digest| actual.metadata.get(DURABLE_CONTRACT) == Some(digest))
        && expected.deliver_subject == actual.deliver_subject
        && filters(expected).eq(filters(actual))
        && expected.deliver_policy == actual.deliver_policy
        && expected.ack_policy == actual.ack_policy
        && expected.ack_wait == actual.ack_wait
        && expected.max_deliver == actual.max_deliver
        && expected.replay_policy == actual.replay_policy
        && expected.backoff == actual.backoff
        && expected.headers_only == actual.headers_only;
    (!compatible).then_some("durable_configuration")
}

fn report_topology_failure(
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

pub(crate) fn report_bind_refusal(
    error_type: &'static str,
    expected_contract: Option<&str>,
) -> MessagingError {
    let failure = MessagingError::Topology;
    tracing::warn!(
        operation = "bind_consumer",
        error.type = error_type,
        expected_contract,
        reason = %failure,
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
    StreamInterestRetention,
    StreamSealed,
    DeadLetterBoundsUnavailable,
    DeadLetterHeaderSize,
}

impl Refusal {
    fn report(
        self,
        component: &'static str,
        required_bytes: usize,
        limit_bytes: Option<i64>,
    ) -> MessagingError {
        let failure = self.failure();
        log_admission_refusal(
            component,
            failure,
            self.error_type(),
            required_bytes,
            limit_bytes,
        );
        failure
    }

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
            Self::StreamInterestRetention => "stream_interest_retention",
            Self::StreamSealed => "stream_sealed",
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
            | Self::StreamNoAck
            | Self::StreamInterestRetention
            | Self::StreamSealed => MessagingError::Topology,
        }
    }
}

fn log_admission_refusal(
    component: &'static str,
    failure: MessagingError,
    error_type: &'static str,
    required_bytes: usize,
    limit_bytes: Option<i64>,
) {
    tracing::warn!(
        operation = component,
        reason = %failure,
        error.type = error_type,
        required_bytes,
        limit_bytes,
        "messaging_admission_failed"
    );
}

pub(crate) fn validate_server(
    client: &async_nats::Client,
    envelope_limit_bytes: usize,
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
    if info.max_payload < envelope_limit_bytes {
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

#[warn(clippy::wildcard_enum_match_arm)]
fn request_failure(error: &async_nats::jetstream::context::RequestError) -> Option<MessagingError> {
    use async_nats::jetstream::context::RequestErrorKind;

    match error.kind() {
        RequestErrorKind::TimedOut => Some(BROKER_REQUEST_TIMED_OUT),
        RequestErrorKind::InvalidSubject => {
            Some(MessagingError::Configuration("broker subject is invalid"))
        }
        RequestErrorKind::NoResponders | RequestErrorKind::Other => None,
    }
}

fn classify_cause(error: &(dyn std::error::Error + 'static)) -> Option<MessagingError> {
    if let Some(failure) = error
        .downcast_ref::<async_nats::jetstream::context::RequestError>()
        .and_then(request_failure)
    {
        return Some(failure);
    }
    let refused = match error
        .downcast_ref::<GetStreamError>()
        .and_then(stream_broker_error)
    {
        Some(broker_error) => is_auth_refusal(&broker_error),
        None => error
            .downcast_ref::<async_nats::jetstream::Error>()
            .is_some_and(is_auth_refusal),
    };
    refused.then_some(MessagingError::Authentication)
}

#[cfg(test)]
mod tests {
    use async_nats::jetstream::context::{
        GetStreamError, GetStreamErrorKind, RequestError, RequestErrorKind,
    };

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
    fn stream_lookup_preserves_broker_authentication_refusals() {
        for code in [401, 403, 404] {
            let broker = serde_json::from_value(serde_json::json!({ "code": code })).unwrap();
            let error = GetStreamError::new(GetStreamErrorKind::JetStream(broker));
            let failure = classify_topology(&error);
            if code == 404 {
                std::assert_matches!(failure, MessagingError::Topology);
            } else {
                std::assert_matches!(failure, MessagingError::Authentication);
            }
        }
    }

    #[test]
    fn an_unrecognized_chain_is_a_topology_failure() {
        let error = Wrapper(Box::new(RequestError::new(RequestErrorKind::Other)));
        std::assert_matches!(classify_topology(&error), MessagingError::Topology);
    }
}
