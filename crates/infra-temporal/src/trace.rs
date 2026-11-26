use std::collections::HashMap;

use futures_util::future::BoxFuture;
use opentelemetry::global;
#[cfg(any(feature = "worker", test))]
use opentelemetry::propagation::Extractor;
use opentelemetry::propagation::Injector;
#[cfg(any(feature = "worker", test))]
use opentelemetry::trace::TraceContextExt as _;
use temporalio_client::errors::{
    WorkflowInteractionError, WorkflowQueryError, WorkflowStartError, WorkflowUpdateError,
    WorkflowUpdateWithStartError,
};
use temporalio_client::{
    CancelWorkflowInput, ClientInterceptor, DescribeWorkflowInput, DescribeWorkflowOutput,
    FetchWorkflowHistoryPageInput, FetchWorkflowHistoryPageOutput, Next, PollWorkflowUpdateInput,
    PollWorkflowUpdateOutput, QueryWorkflowInput, QueryWorkflowOutput,
    SignalWithStartWorkflowInput, SignalWorkflowInput, StartWorkflowInput, StartWorkflowOutput,
    StartWorkflowUpdateInput, StartWorkflowUpdateOutput, TerminateWorkflowInput,
    UpdateWithStartWorkflowInput, UpdateWithStartWorkflowOutput,
};
use temporalio_common::data_converters::{
    DataConverter, GenericPayloadConverter as _, PayloadConversionError, PayloadConverter,
    SerializationContext, SerializationContextData, WorkflowSerializationContext,
};
use temporalio_common::protos::temporal::api::common::v1::{Header, Payload};
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use crate::payload::{
    ClassifiedPayloadError, PayloadError, classify_payload_error, encode_codec_error,
};

pub(crate) const TRACER_DATA_HEADER: &str = "_tracer-data";

const TRACEPARENT_KEY: &str = "traceparent";
const TRACESTATE_KEY: &str = "tracestate";
const TRACEPARENT_MAX_BYTES: usize = 256;
const TRACESTATE_MAX_BYTES: usize = 512;

fn max_bytes_for_key(key: &str) -> Option<usize> {
    match key {
        TRACEPARENT_KEY => Some(TRACEPARENT_MAX_BYTES),
        TRACESTATE_KEY => Some(TRACESTATE_MAX_BYTES),
        _ => None,
    }
}

fn is_valid_header_value(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value
            .bytes()
            .all(|byte| byte.is_ascii() && !byte.is_ascii_control())
}

#[derive(Default)]
struct SanitizedCarrier(HashMap<String, String>);

impl Injector for SanitizedCarrier {
    fn set(&mut self, key: &str, value: String) {
        let Some(max_bytes) = max_bytes_for_key(key) else {
            return;
        };
        if is_valid_header_value(&value, max_bytes) {
            self.0.insert(key.to_owned(), value);
        }
    }
}

#[cfg(any(feature = "worker", test))]
impl Extractor for SanitizedCarrier {
    fn get(&self, key: &str) -> Option<&str> {
        let max_bytes = max_bytes_for_key(key)?;
        let value = self.0.get(key).map(String::as_str)?;
        is_valid_header_value(value, max_bytes).then_some(value)
    }

    fn keys(&self) -> Vec<&str> {
        let mut keys = Vec::new();
        if self.get(TRACEPARENT_KEY).is_some() {
            keys.push(TRACEPARENT_KEY);
            if self.get(TRACESTATE_KEY).is_some() {
                keys.push(TRACESTATE_KEY);
            }
        }
        keys
    }
}

pub(crate) fn inject_current_context_into_headers(
    headers: &mut HashMap<String, Payload>,
    converter: &PayloadConverter,
) -> Result<(), PayloadError> {
    let context = tracing::Span::current().context();
    let mut carrier = SanitizedCarrier::default();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut carrier);
    });
    if !carrier.0.contains_key(TRACEPARENT_KEY) {
        return Ok(());
    }
    let context_data = SerializationContextData::None;
    let payload = converter
        .to_payload(
            &SerializationContext::new(&context_data, converter),
            &carrier.0,
        )
        .map_err(|error| match classify_payload_error(&error) {
            ClassifiedPayloadError::Encode(err) | ClassifiedPayloadError::Decode(err) => err,
            ClassifiedPayloadError::EncodeCodec | ClassifiedPayloadError::Unknown => {
                PayloadError::Serialize
            }
        })?;
    headers.insert(TRACER_DATA_HEADER.to_owned(), payload);
    Ok(())
}

#[cfg(any(feature = "worker", test))]
pub(crate) fn extract_context_from_headers(
    headers: &HashMap<String, Payload>,
    converter: &PayloadConverter,
) -> opentelemetry::Context {
    let Some(payload) = headers.get(TRACER_DATA_HEADER) else {
        return opentelemetry::Context::new();
    };
    let context_data = SerializationContextData::None;
    let Ok(raw_map) = converter.from_payload::<std::collections::BTreeMap<String, String>>(
        &SerializationContext::new(&context_data, converter),
        payload.clone(),
    ) else {
        return opentelemetry::Context::new();
    };
    let mut sanitized = SanitizedCarrier::default();
    for (key, value) in raw_map {
        sanitized.set(&key, value);
    }
    if !sanitized.0.contains_key(TRACEPARENT_KEY) {
        return opentelemetry::Context::new();
    }
    let extracted = global::get_text_map_propagator(|propagator| {
        propagator.extract_with_context(&opentelemetry::Context::new(), &sanitized)
    });
    if extracted.span().span_context().is_valid() {
        extracted
    } else {
        opentelemetry::Context::new()
    }
}

async fn inject_into_optional_header(
    header: &mut Option<Header>,
    data_converter: &DataConverter,
) -> Result<(), PayloadConversionError> {
    let mut tracing_fields = HashMap::new();
    inject_current_context_into_headers(&mut tracing_fields, data_converter.payload_converter())
        .map_err(|error| PayloadConversionError::EncodingError(Box::new(error)))?;
    let Some(payload) = tracing_fields.remove(TRACER_DATA_HEADER) else {
        return Ok(());
    };
    let context = SerializationContextData::Workflow(WorkflowSerializationContext::new());
    let mut encoded = data_converter
        .codec()
        .encode(&context, vec![payload])
        .await
        .map_err(|_| encode_codec_error())?;
    if encoded.len() != 1 {
        return Err(encode_codec_error());
    }
    header
        .get_or_insert_default()
        .fields
        .insert(TRACER_DATA_HEADER.to_owned(), encoded.remove(0));
    Ok(())
}

pub(crate) struct TraceClientInterceptor {
    data_converter: DataConverter,
}

impl TraceClientInterceptor {
    #[must_use]
    pub(crate) const fn new(data_converter: DataConverter) -> Self {
        Self { data_converter }
    }
}

impl ClientInterceptor for TraceClientInterceptor {
    fn start_workflow<'a>(
        &'a self,
        mut input: StartWorkflowInput,
        next: Next<
            'a,
            StartWorkflowInput,
            BoxFuture<'a, Result<StartWorkflowOutput, WorkflowStartError>>,
        >,
    ) -> BoxFuture<'a, Result<StartWorkflowOutput, WorkflowStartError>> {
        let span = tracing::info_span!(
            "temporal_start_workflow",
            otel.kind = "client",
            rpc.system = "grpc",
            temporal.workflow_type = %input.workflow_type,
        );
        Box::pin(
            async move {
                inject_into_optional_header(&mut input.options.header, &self.data_converter)
                    .await?;
                next.run(input).await
            }
            .instrument(span),
        )
    }

    fn signal_with_start_workflow<'a>(
        &'a self,
        mut input: SignalWithStartWorkflowInput,
        next: Next<
            'a,
            SignalWithStartWorkflowInput,
            BoxFuture<'a, Result<StartWorkflowOutput, WorkflowStartError>>,
        >,
    ) -> BoxFuture<'a, Result<StartWorkflowOutput, WorkflowStartError>> {
        let span = tracing::info_span!(
            "temporal_signal_with_start",
            otel.kind = "client",
            rpc.system = "grpc",
            temporal.workflow_type = %input.workflow_type,
            temporal.signal_name = %input.signal_name,
        );
        Box::pin(
            async move {
                inject_into_optional_header(&mut input.options.header, &self.data_converter)
                    .await?;
                next.run(input).await
            }
            .instrument(span),
        )
    }

    fn update_with_start_workflow<'a>(
        &'a self,
        mut input: UpdateWithStartWorkflowInput,
        next: Next<
            'a,
            UpdateWithStartWorkflowInput,
            BoxFuture<'a, Result<UpdateWithStartWorkflowOutput, WorkflowUpdateWithStartError>>,
        >,
    ) -> BoxFuture<'a, Result<UpdateWithStartWorkflowOutput, WorkflowUpdateWithStartError>> {
        let span = tracing::info_span!(
            "temporal_update_with_start",
            otel.kind = "client",
            rpc.system = "grpc",
            temporal.workflow_type = %input.workflow_type,
            temporal.update_name = %input.update_name,
        );
        Box::pin(
            async move {
                inject_into_optional_header(&mut input.options.start_header, &self.data_converter)
                    .await?;
                inject_into_optional_header(&mut input.options.update_header, &self.data_converter)
                    .await?;
                next.run(input).await
            }
            .instrument(span),
        )
    }

    fn signal_workflow<'a>(
        &'a self,
        mut input: SignalWorkflowInput,
        next: Next<'a, SignalWorkflowInput, BoxFuture<'a, Result<(), WorkflowInteractionError>>>,
    ) -> BoxFuture<'a, Result<(), WorkflowInteractionError>> {
        let span = tracing::info_span!(
            "temporal_signal_workflow",
            otel.kind = "client",
            rpc.system = "grpc",
            temporal.signal_name = %input.signal_name,
        );
        Box::pin(
            async move {
                inject_into_optional_header(&mut input.options.header, &self.data_converter)
                    .await?;
                next.run(input).await
            }
            .instrument(span),
        )
    }

    fn query_workflow<'a>(
        &'a self,
        mut input: QueryWorkflowInput,
        next: Next<
            'a,
            QueryWorkflowInput,
            BoxFuture<'a, Result<QueryWorkflowOutput, WorkflowQueryError>>,
        >,
    ) -> BoxFuture<'a, Result<QueryWorkflowOutput, WorkflowQueryError>> {
        let span = tracing::info_span!(
            "temporal_query_workflow",
            otel.kind = "client",
            rpc.system = "grpc",
            temporal.query_name = %input.query_name,
        );
        Box::pin(
            async move {
                inject_into_optional_header(&mut input.options.header, &self.data_converter)
                    .await?;
                next.run(input).await
            }
            .instrument(span),
        )
    }

    fn start_workflow_update<'a>(
        &'a self,
        mut input: StartWorkflowUpdateInput,
        next: Next<
            'a,
            StartWorkflowUpdateInput,
            BoxFuture<'a, Result<StartWorkflowUpdateOutput, WorkflowUpdateError>>,
        >,
    ) -> BoxFuture<'a, Result<StartWorkflowUpdateOutput, WorkflowUpdateError>> {
        let span = tracing::info_span!(
            "temporal_start_workflow_update",
            otel.kind = "client",
            rpc.system = "grpc",
            temporal.update_name = %input.update_name,
        );
        Box::pin(
            async move {
                inject_into_optional_header(&mut input.options.header, &self.data_converter)
                    .await?;
                next.run(input).await
            }
            .instrument(span),
        )
    }

    fn poll_workflow_update<'a>(
        &'a self,
        input: PollWorkflowUpdateInput,
        next: Next<
            'a,
            PollWorkflowUpdateInput,
            BoxFuture<'a, Result<PollWorkflowUpdateOutput, WorkflowUpdateError>>,
        >,
    ) -> BoxFuture<'a, Result<PollWorkflowUpdateOutput, WorkflowUpdateError>> {
        let span = tracing::info_span!(
            "temporal_poll_workflow_update",
            otel.kind = "client",
            rpc.system = "grpc",
        );
        Box::pin(async move { next.run(input).await }.instrument(span))
    }

    fn describe_workflow<'a>(
        &'a self,
        input: DescribeWorkflowInput,
        next: Next<
            'a,
            DescribeWorkflowInput,
            BoxFuture<'a, Result<DescribeWorkflowOutput, WorkflowInteractionError>>,
        >,
    ) -> BoxFuture<'a, Result<DescribeWorkflowOutput, WorkflowInteractionError>> {
        let span = tracing::info_span!(
            "temporal_describe_workflow",
            otel.kind = "client",
            rpc.system = "grpc",
        );
        Box::pin(async move { next.run(input).await }.instrument(span))
    }

    fn cancel_workflow<'a>(
        &'a self,
        input: CancelWorkflowInput,
        next: Next<'a, CancelWorkflowInput, BoxFuture<'a, Result<(), WorkflowInteractionError>>>,
    ) -> BoxFuture<'a, Result<(), WorkflowInteractionError>> {
        let span = tracing::info_span!(
            "temporal_cancel_workflow",
            otel.kind = "client",
            rpc.system = "grpc",
        );
        Box::pin(async move { next.run(input).await }.instrument(span))
    }

    fn terminate_workflow<'a>(
        &'a self,
        input: TerminateWorkflowInput,
        next: Next<'a, TerminateWorkflowInput, BoxFuture<'a, Result<(), WorkflowInteractionError>>>,
    ) -> BoxFuture<'a, Result<(), WorkflowInteractionError>> {
        let span = tracing::info_span!(
            "temporal_terminate_workflow",
            otel.kind = "client",
            rpc.system = "grpc",
        );
        Box::pin(async move { next.run(input).await }.instrument(span))
    }

    fn fetch_workflow_history_page<'a>(
        &'a self,
        input: FetchWorkflowHistoryPageInput,
        next: Next<
            'a,
            FetchWorkflowHistoryPageInput,
            BoxFuture<'a, Result<FetchWorkflowHistoryPageOutput, WorkflowInteractionError>>,
        >,
    ) -> BoxFuture<'a, Result<FetchWorkflowHistoryPageOutput, WorkflowInteractionError>> {
        let span = tracing::info_span!(
            "temporal_fetch_workflow_history",
            otel.kind = "client",
            rpc.system = "grpc",
        );
        Box::pin(async move { next.run(input).await }.instrument(span))
    }
}
