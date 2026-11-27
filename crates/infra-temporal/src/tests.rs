use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use compact_str::CompactString;
use futures::FutureExt as _;
use futures_util::future::BoxFuture;
use health::Probe as _;
use metrics_exporter_prometheus::PrometheusBuilder;
use metrics_util::MetricKind;
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use opentelemetry::baggage::BaggageExt as _;
use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
use opentelemetry::{Context as OtelContext, KeyValue, global};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use opentelemetry_sdk::trace::SdkTracerProvider;
use operation_context::{OperationContext, Stopped};
use prost::Message as _;
use rcgen::{CertificateParams, KeyPair};
use serde::{Deserialize, Serialize};
use temporalio_client::callback_based::{
    CallbackBasedGrpcService, GrpcRequest, GrpcSuccessResponse,
};
use temporalio_client::errors::{
    WorkflowGetResultError as SdkWorkflowGetResultError,
    WorkflowStartError as SdkWorkflowStartError, WorkflowUpdateError as SdkWorkflowUpdateError,
};
use temporalio_client::{MESSAGE_TOO_LARGE_KEY, ResolvesClientCert};
use temporalio_common::WorkflowDefinition as _;
use temporalio_common::data_converters::{
    GenericPayloadConverter as _, PayloadConversionError, SerializationContext,
    SerializationContextData, WorkflowSerializationContext,
};
use temporalio_common::protos::temporal::api::common::v1::{
    Payload, Payloads, WorkflowExecution, WorkflowType,
};
use temporalio_common::protos::temporal::api::enums::v1::{
    UpdateWorkflowExecutionLifecycleStage, WorkflowExecutionStatus as ProtoWorkflowExecutionStatus,
    WorkflowIdConflictPolicy as ProtoWorkflowIdConflictPolicy,
    WorkflowIdReusePolicy as ProtoWorkflowIdReusePolicy,
};
use temporalio_common::protos::temporal::api::history::v1::{
    History, HistoryEvent, WorkflowExecutionCanceledEventAttributes,
    WorkflowExecutionCompletedEventAttributes, WorkflowExecutionContinuedAsNewEventAttributes,
    WorkflowExecutionFailedEventAttributes, WorkflowExecutionTerminatedEventAttributes,
    WorkflowExecutionTimedOutEventAttributes, history_event,
};
use temporalio_common::protos::temporal::api::query::v1::QueryRejected;
use temporalio_common::protos::temporal::api::update::v1::{Outcome, UpdateRef, outcome};
use temporalio_common::protos::temporal::api::workflow::v1::WorkflowExecutionInfo;
use temporalio_common::protos::temporal::api::workflowservice::v1::{
    DescribeNamespaceResponse, DescribeWorkflowExecutionResponse, ExecuteMultiOperationRequest,
    ExecuteMultiOperationResponse, GetWorkflowExecutionHistoryResponse, QueryWorkflowResponse,
    RequestCancelWorkflowExecutionRequest, SignalWithStartWorkflowExecutionRequest,
    SignalWithStartWorkflowExecutionResponse, SignalWorkflowExecutionRequest,
    StartWorkflowExecutionRequest, StartWorkflowExecutionResponse, UpdateWorkflowExecutionRequest,
    UpdateWorkflowExecutionResponse, execute_multi_operation_request,
    execute_multi_operation_response,
};
use temporalio_common::protos::{ENCODING_PAYLOAD_KEY, JSON_ENCODING_VAL};
use temporalio_common::telemetry::metrics::{
    CoreMeter as _, MetricKeyValue, MetricParameters, NewAttributes,
};
use temporalio_macros::{workflow, workflow_methods};
use temporalio_workflow::{SyncWorkflowContext, WorkflowContext, WorkflowResult};
use tokio::sync::{Notify, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tonic::Status;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::layer::SubscriberExt as _;
use url::Url;

use crate::connection::{
    build_connection_options, build_tls_setup, load_initial_api_key, proxy_options_from_url,
};
use crate::credentials::load_api_key_file;
use crate::error::{map_result_error, map_start_error, map_start_status, map_update_error};
use crate::payload::{ClassifiedPayloadError, build_data_converter, classify_payload_error};
use crate::trace::{
    TRACER_DATA_HEADER, extract_context_from_headers, inject_current_context_into_headers,
};
use crate::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct FloatPayload {
    amount: f64,
}

#[workflow]
#[derive(Default)]
struct ClientContractWorkflow;

#[workflow_methods]
impl ClientContractWorkflow {
    #[run]
    async fn run(ctx: &mut WorkflowContext<Self>, input: String) -> WorkflowResult<String> {
        ctx.timer(Duration::from_millis(1)).await;
        Ok(input)
    }

    #[signal]
    #[expect(
        clippy::unused_self,
        reason = "workflow_methods requires a receiver on signal handlers"
    )]
    fn notify(&mut self, _ctx: &mut SyncWorkflowContext<Self>, _note: String) {}

    #[query]
    #[expect(
        clippy::unused_self,
        reason = "workflow_methods requires a receiver on query handlers"
    )]
    fn status(&self, _ctx: &temporalio_workflow::WorkflowContextView, _arg: String) -> String {
        "ok".to_owned()
    }

    #[update]
    async fn apply(ctx: &mut WorkflowContext<Self>, input: String) -> String {
        ctx.timer(Duration::from_millis(1)).await;
        input
    }
}

#[workflow]
#[derive(Default)]
struct FloatWorkflow;

#[workflow_methods]
impl FloatWorkflow {
    #[run]
    async fn run(
        ctx: &mut WorkflowContext<Self>,
        input: FloatPayload,
    ) -> WorkflowResult<FloatPayload> {
        ctx.timer(Duration::from_millis(1)).await;
        Ok(input)
    }

    #[update]
    async fn adjust(ctx: &mut WorkflowContext<Self>, input: FloatPayload) -> FloatPayload {
        ctx.timer(Duration::from_millis(1)).await;
        input
    }
}

fn base_options() -> TemporalOptions {
    TemporalOptions {
        endpoint: Url::parse("http://127.0.0.1:7233").expect("valid loopback url"),
        namespace: "default".to_owned(),
        identity: "test-client@0.1.0".to_owned(),
        auth: TemporalAuth::None,
        tls: None,
        allow_plaintext: true,
        allow_unauthenticated: true,
        trusted_network: false,
        credential_refresh_interval: Duration::from_secs(30),
        connect_timeout: Duration::from_secs(5),
        rpc_timeout: Duration::from_secs(5),
        keep_alive_interval: Duration::from_secs(30),
        keep_alive_timeout: Duration::from_secs(15),
        dns_load_balancing: false,
        http_proxy_url: None,
        max_payload_bytes: 64 * 1024,
        probe_budget: Duration::from_secs(1),
        payload_codec: None,
    }
}

fn json_payload(bytes: &[u8]) -> Payload {
    Payload {
        metadata: HashMap::from([(
            ENCODING_PAYLOAD_KEY.to_owned(),
            JSON_ENCODING_VAL.as_bytes().to_vec(),
        )]),
        data: bytes.to_vec(),
        external_payloads: Vec::new(),
    }
}

fn generate_cert_and_key(cn: &str) -> (String, String) {
    let key = KeyPair::generate().expect("keypair generates");
    let params = CertificateParams::new(vec![cn.to_owned()]).expect("cert params");
    let cert = params.self_signed(&key).expect("self-signed cert");
    (cert.pem(), key.serialize_pem())
}

fn simple_callback<F>(handler: F) -> CallbackBasedGrpcService
where
    F: Fn(GrpcRequest) -> Result<Vec<u8>, Status> + Send + Sync + 'static,
{
    let handler = Arc::new(handler);
    CallbackBasedGrpcService {
        callback: Arc::new(move |mut req: GrpcRequest| {
            let handler = Arc::clone(&handler);
            async move {
                let mut headers = req.headers.clone();
                headers.clear();
                if req.rpc == "GetSystemInfo" {
                    req.headers.clear();
                    return Ok(GrpcSuccessResponse {
                        headers: req.headers,
                        proto: Vec::new(),
                    });
                }
                let proto = handler(req)?;
                Ok(GrpcSuccessResponse { headers, proto })
            }
            .boxed()
        }),
    }
}

async fn admit_with_callback(
    options: TemporalOptions,
    callback: CallbackBasedGrpcService,
) -> (TemporalStartup, TemporalConnection) {
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));
    let mut startup = TemporalStartup::prepare(options, &ctx).with_service_override(callback);
    let conn = startup
        .admit(CancellationToken::new())
        .await
        .expect("startup admits callback service");
    (startup, conn)
}

#[test]
fn workflow_macro_expands_and_client_is_send_sync() {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TemporalClient>();
    assert_send_sync::<TemporalConnection>();
    assert_send_sync::<TemporalProbe>();
    assert_eq!(ClientContractWorkflow::run.name(), "ClientContractWorkflow");
}

#[test]
fn validated_identifiers_reject_invalid_values_without_echoing_input() {
    assert_eq!(WorkflowId::try_from("wf-1").unwrap().as_str(), "wf-1");
    assert_eq!(RunId::try_from("run-1").unwrap().to_string(), "run-1");
    assert_eq!(UpdateId::try_from("upd-1").unwrap().as_ref(), "upd-1");
    assert_eq!(NonBlankName::try_from("orders").unwrap().as_str(), "orders");
    assert_eq!(RequestKey::try_from("key-1").unwrap().as_str(), "key-1");

    for bad in ["", "   ", "\t\n"] {
        assert_eq!(WorkflowId::try_from(bad), Err(IdentifierError::Empty));
        assert_eq!(UpdateId::try_from(bad), Err(IdentifierError::Empty));
        assert_eq!(RequestKey::try_from(bad), Err(IdentifierError::Empty));
    }
    for bad in [" lead", "trail ", "bad\nsecret-id", "bad\0id"] {
        let err = WorkflowId::try_from(bad).unwrap_err();
        assert_eq!(err, IdentifierError::InvalidCharacters);
        assert!(!err.to_string().contains("secret-id"));
        assert!(!format!("{err:?}").contains("secret-id"));
    }
    let long = "a".repeat(256);
    assert_eq!(
        UpdateId::try_from(long.as_str()),
        Err(IdentifierError::TooLong)
    );
    assert_eq!(
        NonBlankName::try_from(long.as_str()),
        Err(IdentifierError::TooLong)
    );
    assert!(WorkflowId::try_from(long).is_ok());
    assert_eq!(
        WorkflowId::try_from("a".repeat(1001)),
        Err(IdentifierError::TooLong)
    );
}

#[test]
fn core_meter_forwards_prometheus_attributes_with_single_prefix_and_custom_counters() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();

    ::metrics::with_local_recorder(&recorder, || {
        describe_metrics();
        WorkspaceMetricsMeter::record_drain_forced();
        let meter = WorkspaceMetricsMeter::new(METRIC_PREFIX);
        let base = meter.new_attributes(NewAttributes::new(vec![MetricKeyValue::new(
            "task-queue",
            "orders",
        )]));
        let extended = meter.extend_attributes(
            base,
            NewAttributes::new(vec![MetricKeyValue::new("namespace", "default")]),
        );

        let counter = meter.counter(MetricParameters::from("request"));
        counter.add(2, &extended);

        let prefixed_counter = meter.counter(MetricParameters::from("temporal_request"));
        prefixed_counter.add(1, &extended);

        let duration = meter.histogram_duration(MetricParameters::from("request_latency"));
        duration.record(Duration::from_millis(250), &extended);

        let gauge = meter.gauge(MetricParameters::from("sticky_cache_size"));
        gauge.record(7, &extended);

        let up_down = meter.up_down_counter(MetricParameters::from("active_tasks"));
        up_down.add(3, &extended);
        up_down.add(-1, &extended);
    });

    let snapshot = snapshotter.snapshot().into_vec();
    let mut saw_counter = false;
    let mut saw_histogram = false;
    let mut saw_gauge = false;
    let mut saw_up_down = false;
    let mut saw_drain_forced = false;

    for (ckey, _, _, value) in snapshot {
        let name = ckey.key().name();
        assert!(
            !name.starts_with("temporal_temporal_"),
            "double prefix: {name}"
        );
        let labels: Vec<(&str, &str)> = ckey
            .key()
            .labels()
            .map(|label| (label.key(), label.value()))
            .collect();
        match (ckey.kind(), name) {
            (MetricKind::Counter, "temporal_drain_forced_total") => {
                assert_eq!(value, DebugValue::Counter(1));
                assert_eq!(labels, []);
                saw_drain_forced = true;
            }
            (MetricKind::Counter, "temporal_request") => {
                assert_eq!(
                    labels,
                    vec![("namespace", "default"), ("task_queue", "orders")]
                );
                assert_eq!(value, DebugValue::Counter(3));
                saw_counter = true;
            }
            (MetricKind::Histogram, "temporal_request_latency") => {
                assert_eq!(
                    labels,
                    vec![("namespace", "default"), ("task_queue", "orders")]
                );
                assert_eq!(value, DebugValue::Histogram(vec![0.25.into()]));
                saw_histogram = true;
            }
            (MetricKind::Gauge, "temporal_sticky_cache_size") => {
                assert_eq!(value, DebugValue::Gauge(7.0.into()));
                saw_gauge = true;
            }
            (MetricKind::Gauge, "temporal_active_tasks") => {
                assert_eq!(value, DebugValue::Gauge(2.0.into()));
                saw_up_down = true;
            }
            other => panic!("unexpected metric recorded: {other:?}"),
        }
    }

    assert!(saw_counter && saw_histogram && saw_gauge && saw_up_down && saw_drain_forced);

    ::metrics::with_local_recorder(&recorder, || {
        let meter = WorkspaceMetricsMeter::new(METRIC_PREFIX);
        let attrs = meter.new_attributes(NewAttributes::new(vec![
            MetricKeyValue::new("task-queue", "orders"),
            MetricKeyValue::new("namespace", "default"),
        ]));
        let up_down = meter.up_down_counter(MetricParameters::from("active_tasks"));
        up_down.add(-4, &attrs);
    });

    let active_tasks =
        snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .find_map(|(ckey, _, _, value)| {
                (ckey.kind() == MetricKind::Gauge && ckey.key().name() == "temporal_active_tasks")
                    .then_some(value)
            });
    assert_eq!(active_tasks, Some(DebugValue::Gauge((-4.0).into())));
}

fn xor_payloads(payloads: &mut [Payload], mask: u8) {
    for payload in payloads {
        for byte in &mut payload.data {
            *byte ^= mask;
        }
    }
}

#[tokio::test]
async fn finite_bounded_json_converter_enforces_nan_size_binary_null_and_codec() {
    struct XorCodec(u8);

    impl PayloadCodec for XorCodec {
        fn encode(
            &self,
            _context: &SerializationContextData,
            mut payloads: Vec<Payload>,
        ) -> BoxFuture<'static, Result<Vec<Payload>, PayloadConversionError>> {
            let mask = self.0;
            Box::pin(async move {
                xor_payloads(&mut payloads, mask);
                Ok(payloads)
            })
        }

        fn decode(
            &self,
            _context: &SerializationContextData,
            mut payloads: Vec<Payload>,
        ) -> BoxFuture<'static, Result<Vec<Payload>, PayloadConversionError>> {
            let mask = self.0;
            Box::pin(async move {
                xor_payloads(&mut payloads, mask);
                Ok(payloads)
            })
        }
    }

    let dc = build_data_converter(32, None);
    let pc = dc.payload_converter();
    let ctx_data = SerializationContextData::None;
    let ctx = SerializationContext::new(&ctx_data, pc);

    for bad_float in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let err = pc
            .to_payload(&ctx, &FloatPayload { amount: bad_float })
            .unwrap_err();
        assert_eq!(
            classify_payload_error(&err),
            ClassifiedPayloadError::Encode(PayloadError::Serialize)
        );
    }

    let oversized = "x".repeat(40);
    let err = pc.to_payload(&ctx, &oversized).unwrap_err();
    assert_eq!(
        classify_payload_error(&err),
        ClassifiedPayloadError::Encode(PayloadError::TooLarge { bytes: 42 })
    );

    let null_payload = pc.to_payload(&ctx, &Option::<String>::None).unwrap();
    assert_eq!(
        null_payload
            .metadata
            .get(ENCODING_PAYLOAD_KEY)
            .map(Vec::as_slice),
        Some(b"binary/null".as_slice())
    );
    assert_eq!(null_payload.data, b"");

    let decoded_none: Option<String> = pc.from_payload(&ctx, null_payload).unwrap();
    assert_eq!(decoded_none, None);

    let legacy_null: Option<String> = pc.from_payload(&ctx, json_payload(b"null")).unwrap();
    assert_eq!(legacy_null, None);

    let bad_null = Payload {
        metadata: HashMap::from([(ENCODING_PAYLOAD_KEY.to_owned(), b"binary/null".to_vec())]),
        data: b"extra".to_vec(),
        external_payloads: Vec::new(),
    };
    let bad_null_err = pc
        .from_payload::<Option<String>>(&ctx, bad_null)
        .unwrap_err();
    assert_eq!(
        classify_payload_error(&bad_null_err),
        ClassifiedPayloadError::Decode(PayloadError::InvalidEncoding)
    );

    let oversized_inbound = json_payload(&[b'"'; 40]);
    let decode_large = pc
        .from_payload::<String>(&ctx, oversized_inbound)
        .unwrap_err();
    assert_eq!(
        classify_payload_error(&decode_large),
        ClassifiedPayloadError::Decode(PayloadError::TooLarge { bytes: 40 })
    );

    let corrupt_json = pc
        .from_payload::<String>(&ctx, json_payload(b"{broken"))
        .unwrap_err();
    assert_eq!(
        classify_payload_error(&corrupt_json),
        ClassifiedPayloadError::Decode(PayloadError::Decode)
    );

    let codec_dc = build_data_converter(128, Some(Arc::new(XorCodec(0x5A))));
    let encoded = codec_dc
        .to_payload(&ctx_data, &"secret-payload".to_owned())
        .await
        .unwrap();
    assert_ne!(encoded.data, br#""secret-payload""#);
    let decoded: String = codec_dc.from_payload(&ctx_data, encoded).await.unwrap();
    assert_eq!(decoded, "secret-payload");
}

fn header_from_map(
    pc: &temporalio_common::data_converters::PayloadConverter,
    map: &HashMap<String, String>,
) -> HashMap<String, Payload> {
    let payload = pc
        .to_payload(
            &SerializationContext::new(&SerializationContextData::None, pc),
            map,
        )
        .unwrap();
    HashMap::from([(TRACER_DATA_HEADER.to_owned(), payload)])
}

fn install_test_propagator() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        global::set_text_map_propagator(TextMapCompositePropagator::new(vec![
            Box::new(TraceContextPropagator::new()),
            Box::new(BaggagePropagator::new()),
        ]));
    });
}

#[test]
fn trace_context_injects_extracts_and_strips_baggage_controls_and_oversized_headers() {
    install_test_propagator();
    let provider = SdkTracerProvider::builder().build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));

    let dc = build_data_converter(4096, None);
    let pc = dc.payload_converter();

    let mut empty_headers = HashMap::new();
    inject_current_context_into_headers(&mut empty_headers, pc).unwrap();
    assert!(empty_headers.is_empty());

    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("outbound_op");
        let parent_with_baggage =
            OtelContext::new().with_baggage(vec![KeyValue::new("user_id", "secret-123")]);
        assert!(span.set_parent(parent_with_baggage).is_ok());
        assert_eq!(
            span.context()
                .baggage()
                .get("user_id")
                .map(opentelemetry::StringValue::as_str),
            Some("secret-123")
        );
        let expected_trace_id = span.context().span().span_context().trace_id();
        let expected_span_id = span.context().span().span_context().span_id();
        let mut headers = HashMap::new();
        span.in_scope(|| {
            inject_current_context_into_headers(&mut headers, pc).unwrap();
        });
        assert!(headers.contains_key(TRACER_DATA_HEADER));

        let extracted = extract_context_from_headers(&headers, pc);
        let remote = extracted.span().span_context().clone();
        assert!(remote.is_valid());
        assert_eq!(remote.trace_id(), expected_trace_id);
        assert_eq!(remote.span_id(), expected_span_id);
        assert!(extracted.baggage().is_empty());

        let raw_map: HashMap<String, String> = pc
            .from_payload(
                &SerializationContext::new(&SerializationContextData::None, pc),
                headers[TRACER_DATA_HEADER].clone(),
            )
            .unwrap();
        assert!(raw_map.contains_key("traceparent"));
        assert!(!raw_map.contains_key("baggage"), "{raw_map:?}");
        let valid_traceparent = raw_map["traceparent"].clone();

        let polluted = HashMap::from([
            ("traceparent".to_owned(), valid_traceparent.clone()),
            ("tracestate".to_owned(), "vendor=ok".to_owned()),
            ("baggage".to_owned(), "secret=1".to_owned()),
        ]);
        let unsanitized =
            global::get_text_map_propagator(|propagator| propagator.extract(&polluted));
        assert_eq!(
            unsanitized
                .baggage()
                .get("secret")
                .map(opentelemetry::StringValue::as_str),
            Some("1")
        );
        let extracted_polluted = extract_context_from_headers(&header_from_map(pc, &polluted), pc);
        assert_eq!(
            extracted_polluted.span().span_context().trace_id(),
            expected_trace_id
        );
        assert_eq!(
            extracted_polluted
                .span()
                .span_context()
                .trace_state()
                .get("vendor"),
            Some("ok")
        );
        assert!(extracted_polluted.baggage().is_empty());
        assert!(extracted_polluted.baggage().get("secret").is_none());

        let bad_cases = [
            HashMap::from([(
                "traceparent".to_owned(),
                format!("{valid_traceparent}\r\ninjected: 1"),
            )]),
            HashMap::from([("traceparent".to_owned(), "0".repeat(257))]),
            HashMap::from([("tracestate".to_owned(), "vendor=only-state".to_owned())]),
        ];
        for bad_map in bad_cases {
            let ctx = extract_context_from_headers(&header_from_map(pc, &bad_map), pc);
            assert!(!ctx.span().span_context().is_valid());
        }

        let bad_state = HashMap::from([
            ("traceparent".to_owned(), valid_traceparent),
            ("tracestate".to_owned(), format!("k={}", "a".repeat(520))),
        ]);
        let ctx_without_state = extract_context_from_headers(&header_from_map(pc, &bad_state), pc);
        assert!(ctx_without_state.span().span_context().is_valid());
        assert_eq!(
            ctx_without_state
                .span()
                .span_context()
                .trace_state()
                .header(),
            ""
        );
    });
}

#[tokio::test]
async fn options_validation_and_debug_redaction_fail_closed_without_leaking_secrets() {
    let mut plaintext_disallowed = base_options();
    plaintext_disallowed.allow_plaintext = false;
    plaintext_disallowed.trusted_network = false;
    let mut startup =
        TemporalStartup::prepare(plaintext_disallowed, &OperationContext::unbounded());
    std::assert_matches!(
        startup.admit(CancellationToken::new()).await,
        Err(ConnectError::InvalidConfig(_))
    );

    let mut unauth_tls = base_options();
    unauth_tls.endpoint = Url::parse("https://temporal.example.invalid:7233").unwrap();
    unauth_tls.allow_unauthenticated = false;
    unauth_tls.trusted_network = false;
    let mut startup = TemporalStartup::prepare(unauth_tls, &OperationContext::unbounded());
    std::assert_matches!(
        startup.admit(CancellationToken::new()).await,
        Err(ConnectError::InvalidConfig(_))
    );

    let mut dynamic_with_dns_lb = base_options();
    dynamic_with_dns_lb.endpoint = Url::parse("https://temporal.example.invalid:7233").unwrap();
    dynamic_with_dns_lb.dns_load_balancing = true;
    dynamic_with_dns_lb.tls = Some(TemporalTlsOptions {
        server_root_ca_path: Some("/secret/path/ca.pem".into()),
        domain: Some("temporal.example.invalid".to_owned()),
        mtls: Some(TemporalMtlsOptions {
            client_cert_path: "/secret/path/client.crt".into(),
            client_key_path: "/secret/path/client.key".into(),
            rotation: TlsRotationMode::Dynamic,
        }),
    });
    dynamic_with_dns_lb.auth =
        TemporalAuth::ApiKey(Zeroizing::new("super-secret-token".to_owned()));
    let debug_dump = format!("{dynamic_with_dns_lb:?}");
    for secret in [
        "super-secret-token",
        "/secret/path/ca.pem",
        "/secret/path/client.crt",
        "/secret/path/client.key",
    ] {
        assert!(
            !debug_dump.contains(secret),
            "leaked {secret} in {debug_dump}"
        );
    }
    let mut startup = TemporalStartup::prepare(dynamic_with_dns_lb, &OperationContext::unbounded());
    std::assert_matches!(
        startup.admit(CancellationToken::new()).await,
        Err(ConnectError::InvalidConfig(_))
    );

    let raw_status = Status::unavailable("internal-endpoint-10.0.0.9:7233 bearer=top-secret");
    let mapped = map_start_status(&raw_status);
    assert_eq!(
        mapped,
        StartWorkflowError::OutcomeUnknown(UnknownOutcome::Unavailable)
    );
    let formatted = format!("{mapped}; {mapped:?}");
    assert!(!formatted.contains("10.0.0.9"));
    assert!(!formatted.contains("top-secret"));

    let sdk_already = SdkWorkflowStartError::Rpc(Status::already_exists("secret-run-id-123"));
    assert_eq!(
        map_start_error(&sdk_already),
        StartWorkflowError::NotApplied(StartFailure::AlreadyStarted)
    );
}

fn extract_auth_header(req: &GrpcRequest) -> Option<String> {
    req.headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn last_auth(captured: &Mutex<Vec<Option<String>>>) -> Option<String> {
    captured.lock().unwrap().last().cloned().flatten()
}

fn recording_describe_callback(
    captured: Arc<Mutex<Vec<Option<String>>>>,
) -> CallbackBasedGrpcService {
    simple_callback(move |req: GrpcRequest| {
        captured.lock().unwrap().push(extract_auth_header(&req));
        match req.rpc.as_str() {
            "DescribeNamespace" => Ok(DescribeNamespaceResponse::default().encode_to_vec()),
            _ => Err(Status::unimplemented("unexpected")),
        }
    })
}

fn reload_counter(scrape: &str, kind: &str, outcome: &str) -> u64 {
    let prefix =
        format!(r#"temporal_credential_reloads_total{{kind="{kind}",outcome="{outcome}"}} "#);
    scrape
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

async fn wait_next_step(steps: &mut tokio::sync::watch::Receiver<u64>) {
    let current = *steps.borrow_and_update();
    tokio::time::timeout(
        Duration::from_secs(5),
        steps.wait_for(|count| *count > current),
    )
    .await
    .expect("timed out waiting for credential watcher tick")
    .expect("credential watcher exited unexpectedly");
}

async fn wait_for_watcher<F>(steps: &mut tokio::sync::watch::Receiver<u64>, mut condition: F)
where
    F: FnMut() -> bool,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            steps
                .changed()
                .await
                .expect("credential watcher exited before condition was satisfied");
        }
    })
    .await
    .expect("timed out waiting for credential watcher update");
}

async fn wait_watcher_closed(steps: &mut tokio::sync::watch::Receiver<u64>) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while steps.changed().await.is_ok() {}
    })
    .await
    .expect("timed out waiting for background credential watcher to stop");
}

#[derive(Default)]
struct CapturedEvent {
    message: String,
    fields: String,
}

#[derive(Clone, Default)]
struct CaptureLayer {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

struct FieldRecorder<'a>(&'a mut CapturedEvent);

impl tracing::field::Visit for FieldRecorder<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.message = format!("{value:?}");
        } else {
            let _ = write!(self.0.fields, "{}={value:?} ", field.name());
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0.message = value.to_owned();
        } else {
            let _ = write!(self.0.fields, "{}={value} ", field.name());
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut captured = CapturedEvent::default();
        event.record(&mut FieldRecorder(&mut captured));
        self.events.lock().unwrap().push(captured);
    }
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary credential files are written synchronously before watcher assertions"
)]
async fn live_api_key_file_rotation_updates_headers_and_preserves_last_known_good_on_error() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _guard = ::metrics::set_default_local_recorder(&recorder);

    let dir = tempfile::tempdir().unwrap();
    let api_key_path = dir.path().join("sentinel-api-key.txt");
    std::fs::write(&api_key_path, "token-v1\n").unwrap();

    let captured_auth = Arc::new(Mutex::new(Vec::new()));
    let mut options = base_options();
    options.endpoint = Url::parse("https://temporal.example.invalid:7233").unwrap();
    options.auth = TemporalAuth::ApiKeyFile(api_key_path.clone());
    options.credential_refresh_interval = Duration::from_millis(5);

    let (startup, conn) = admit_with_callback(
        options,
        recording_describe_callback(Arc::clone(&captured_auth)),
    )
    .await;
    let mut steps = startup.watcher_steps().expect("watcher spawned");
    let probe = conn.probe();
    probe.check().await.expect("initial api key succeeds");
    assert_eq!(
        last_auth(&captured_auth).as_deref(),
        Some("Bearer token-v1")
    );

    wait_next_step(&mut steps).await;
    let initial_scrape = recorder.handle().render();
    assert_eq!(reload_counter(&initial_scrape, "api_key", "ok"), 0);
    assert_eq!(reload_counter(&initial_scrape, "api_key", "failed"), 0);

    let failed_before_blank = reload_counter(&recorder.handle().render(), "api_key", "failed");
    std::fs::write(&api_key_path, "   \n").unwrap();
    wait_for_watcher(&mut steps, || {
        reload_counter(&recorder.handle().render(), "api_key", "failed") > failed_before_blank
    })
    .await;
    probe.check().await.unwrap();
    assert_eq!(
        last_auth(&captured_auth).as_deref(),
        Some("Bearer token-v1")
    );

    let failed_before_ctrl = reload_counter(&recorder.handle().render(), "api_key", "failed");
    std::fs::write(&api_key_path, "bad\x07key").unwrap();
    wait_for_watcher(&mut steps, || {
        reload_counter(&recorder.handle().render(), "api_key", "failed") > failed_before_ctrl
    })
    .await;
    probe.check().await.unwrap();
    assert_eq!(
        last_auth(&captured_auth).as_deref(),
        Some("Bearer token-v1")
    );

    let failed_before_remove = reload_counter(&recorder.handle().render(), "api_key", "failed");
    std::fs::remove_file(&api_key_path).unwrap();
    wait_for_watcher(&mut steps, || {
        reload_counter(&recorder.handle().render(), "api_key", "failed") > failed_before_remove
    })
    .await;
    probe.check().await.unwrap();
    assert_eq!(
        last_auth(&captured_auth).as_deref(),
        Some("Bearer token-v1")
    );
    assert_eq!(
        reload_counter(&recorder.handle().render(), "api_key", "ok"),
        0
    );

    std::fs::write(&api_key_path, "token-v2\r\n").unwrap();
    wait_for_watcher(&mut steps, || {
        reload_counter(&recorder.handle().render(), "api_key", "ok") >= 1
    })
    .await;
    probe.check().await.unwrap();
    assert_eq!(
        last_auth(&captured_auth).as_deref(),
        Some("Bearer token-v2")
    );

    wait_next_step(&mut steps).await;
    let scrape = recorder.handle().render();
    assert!(
        reload_counter(&scrape, "api_key", "failed") >= 3,
        "{scrape}"
    );
    assert_eq!(reload_counter(&scrape, "api_key", "ok"), 1, "{scrape}");
    for secret in ["token-v1", "token-v2", "sentinel-api-key.txt"] {
        assert!(!scrape.contains(secret), "leaked {secret} in {scrape}");
    }
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
    wait_watcher_closed(&mut steps).await;
    assert!(probe.check().await.is_err());
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary certificate files are written synchronously before watcher assertions"
)]
async fn live_dynamic_mtls_rotation_swaps_valid_keys_and_preserves_prior_on_malformed_update() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _guard = ::metrics::set_default_local_recorder(&recorder);

    let dir = tempfile::tempdir().unwrap();
    let cert_path = dir.path().join("sentinel-client.crt");
    let key_path = dir.path().join("sentinel-client.key");
    let (cert1, key1) = generate_cert_and_key("client-v1.test");
    let (cert2, key2) = generate_cert_and_key("client-v2.test");
    let (_, mismatched_key) = generate_cert_and_key("other.test");

    std::fs::write(&cert_path, &cert1).unwrap();
    std::fs::write(&key_path, &key1).unwrap();

    let mut options = base_options();
    options.endpoint = Url::parse("https://temporal.example.invalid:7233").unwrap();
    options.dns_load_balancing = false;
    options.credential_refresh_interval = Duration::from_millis(5);
    options.tls = Some(TemporalTlsOptions {
        server_root_ca_path: None,
        domain: None,
        mtls: Some(TemporalMtlsOptions {
            client_cert_path: cert_path.clone(),
            client_key_path: key_path.clone(),
            rotation: TlsRotationMode::Dynamic,
        }),
    });

    let (startup, _conn) = admit_with_callback(
        options,
        simple_callback(|req: GrpcRequest| match req.rpc.as_str() {
            "DescribeNamespace" => Ok(DescribeNamespaceResponse::default().encode_to_vec()),
            _ => Err(Status::unimplemented("unexpected")),
        }),
    )
    .await;
    let mut steps = startup.watcher_steps().expect("watcher spawned");
    let resolver = startup.mtls_resolver().expect("dynamic mTLS resolver");
    let cert1_der = resolver.current_cert_der();
    assert!(resolver.has_certs());
    assert!(
        resolver
            .resolve(
                &[],
                &[temporalio_client::SignatureScheme::ECDSA_NISTP256_SHA256]
            )
            .is_some()
    );

    wait_next_step(&mut steps).await;
    let initial_scrape = recorder.handle().render();
    assert_eq!(reload_counter(&initial_scrape, "mtls", "ok"), 0);
    assert_eq!(reload_counter(&initial_scrape, "mtls", "failed"), 0);

    let failed_before_truncated = reload_counter(&recorder.handle().render(), "mtls", "failed");
    std::fs::write(&cert_path, "-----BEGIN CERTIFICATE-----\ntruncated").unwrap();
    wait_for_watcher(&mut steps, || {
        reload_counter(&recorder.handle().render(), "mtls", "failed") > failed_before_truncated
    })
    .await;
    assert_eq!(resolver.current_cert_der(), cert1_der);

    let failed_before_mismatch = reload_counter(&recorder.handle().render(), "mtls", "failed");
    std::fs::write(&cert_path, &cert2).unwrap();
    std::fs::write(&key_path, &mismatched_key).unwrap();
    wait_for_watcher(&mut steps, || {
        reload_counter(&recorder.handle().render(), "mtls", "failed") > failed_before_mismatch
    })
    .await;
    assert_eq!(resolver.current_cert_der(), cert1_der);
    assert_eq!(reload_counter(&recorder.handle().render(), "mtls", "ok"), 0);

    std::fs::write(&cert_path, &cert2).unwrap();
    std::fs::write(&key_path, &key2).unwrap();
    wait_for_watcher(&mut steps, || resolver.current_cert_der() != cert1_der).await;
    let cert2_der = resolver.current_cert_der();
    assert_ne!(cert2_der, cert1_der);

    wait_next_step(&mut steps).await;
    let scrape = recorder.handle().render();
    assert!(reload_counter(&scrape, "mtls", "failed") >= 2, "{scrape}");
    assert_eq!(reload_counter(&scrape, "mtls", "ok"), 1, "{scrape}");
    assert!(!scrape.contains("sentinel-client"));
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
    wait_watcher_closed(&mut steps).await;
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary credential file verifies Drop cancellation and panic observation"
)]
async fn dropping_startup_cancels_background_watcher_and_close_observes_watcher_panic() {
    let dir = tempfile::tempdir().unwrap();
    let api_key_path = dir.path().join("drop-api-key.txt");
    std::fs::write(&api_key_path, "token-drop-v1\n").unwrap();

    let mut options = base_options();
    options.endpoint = Url::parse("https://temporal.example.invalid:7233").unwrap();
    options.auth = TemporalAuth::ApiKeyFile(api_key_path.clone());
    options.credential_refresh_interval = Duration::from_mins(1);

    let (startup, _conn) = admit_with_callback(
        options.clone(),
        simple_callback(|_| Ok(DescribeNamespaceResponse::default().encode_to_vec())),
    )
    .await;
    let mut steps = startup.watcher_steps().expect("watcher spawned");
    assert!(steps.has_changed().is_ok());

    drop(startup);
    wait_watcher_closed(&mut steps).await;

    let capture = CaptureLayer::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _dispatch = tracing::subscriber::set_default(subscriber);

    let (mut panic_startup, _conn) = admit_with_callback(
        options,
        simple_callback(|_| Ok(DescribeNamespaceResponse::default().encode_to_vec())),
    )
    .await;
    let panic_task = tokio::spawn(async {
        panic!("secret-watcher-panic-sentinel");
    });
    panic_startup.replace_watcher_task(panic_task);

    assert_eq!(
        panic_startup
            .close(Instant::now() + Duration::from_secs(2))
            .await,
        CloseOutcome::Complete
    );

    let events = capture.events.lock().unwrap();
    let panic_event = events
        .iter()
        .find(|event| event.message == "temporal_credential_watcher_failed")
        .expect("expected sanitized watcher failure log event");
    assert!(
        panic_event.fields.contains("reason=panicked"),
        "{}",
        panic_event.fields
    );
    assert!(
        !panic_event.fields.contains("secret-watcher-panic-sentinel")
            && !panic_event
                .message
                .contains("secret-watcher-panic-sentinel")
    );
}

#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned FIFO fixture verifies O_NONBLOCK regular-file rejection"
)]
async fn credential_reader_rejects_fifo_and_oversized_files_without_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("api-key.fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();
    let reader = Arc::new(Semaphore::new(1));
    let fifo_err = tokio::time::timeout(Duration::from_secs(2), load_api_key_file(fifo, &reader))
        .await
        .expect("FIFO open must not block")
        .unwrap_err();
    assert_eq!(fifo_err, ConnectError::CredentialRead { kind: "api_key" });

    let huge = dir.path().join("huge.key");
    std::fs::write(&huge, vec![b'a'; 1024 * 1024 + 1]).unwrap();
    let huge_err = load_api_key_file(huge, &reader).await.unwrap_err();
    assert_eq!(huge_err, ConnectError::CredentialRead { kind: "api_key" });
}

#[derive(Default)]
struct MockServerState {
    call_counts: Mutex<HashMap<String, usize>>,
    start: Mutex<Option<StartWorkflowExecutionRequest>>,
    signal: Mutex<Option<SignalWorkflowExecutionRequest>>,
    cancel: Mutex<Option<RequestCancelWorkflowExecutionRequest>>,
    update: Mutex<Option<UpdateWorkflowExecutionRequest>>,
    uws: Mutex<Option<ExecuteMultiOperationRequest>>,
}

fn mock_multi_op_response() -> Vec<u8> {
    ExecuteMultiOperationResponse {
        responses: vec![
            execute_multi_operation_response::Response {
                response: Some(
                    execute_multi_operation_response::response::Response::StartWorkflow(
                        StartWorkflowExecutionResponse {
                            run_id: "run-uws-1".to_owned(),
                            ..Default::default()
                        },
                    ),
                ),
            },
            execute_multi_operation_response::Response {
                response: Some(
                    execute_multi_operation_response::response::Response::UpdateWorkflow(
                        UpdateWorkflowExecutionResponse {
                            update_ref: Some(UpdateRef {
                                workflow_execution: Some(WorkflowExecution {
                                    workflow_id: "wf-uws".to_owned(),
                                    run_id: "run-uws-1".to_owned(),
                                }),
                                update_id: "upd-uws-1".to_owned(),
                            }),
                            outcome: Some(Outcome {
                                value: Some(outcome::Value::Success(Payloads {
                                    payloads: vec![json_payload(br#""uws-ok""#)],
                                })),
                            }),
                            stage: UpdateWorkflowExecutionLifecycleStage::Completed as i32,
                            ..Default::default()
                        },
                    ),
                ),
            },
        ],
    }
    .encode_to_vec()
}

fn mock_describe_response() -> Vec<u8> {
    DescribeWorkflowExecutionResponse {
        workflow_execution_info: Some(WorkflowExecutionInfo {
            execution: Some(WorkflowExecution {
                workflow_id: "wf-1".to_owned(),
                run_id: "run-start-1".to_owned(),
            }),
            r#type: Some(WorkflowType {
                name: "ClientContractWorkflow".to_owned(),
            }),
            status: ProtoWorkflowExecutionStatus::Running as i32,
            task_queue: "orders".to_owned(),
            history_length: 4,
            ..Default::default()
        }),
        ..Default::default()
    }
    .encode_to_vec()
}

fn mock_update_response() -> Vec<u8> {
    UpdateWorkflowExecutionResponse {
        update_ref: Some(UpdateRef {
            workflow_execution: Some(WorkflowExecution {
                workflow_id: "wf-1".to_owned(),
                run_id: "run-start-1".to_owned(),
            }),
            update_id: "upd-explicit-1".to_owned(),
        }),
        outcome: Some(Outcome {
            value: Some(outcome::Value::Success(Payloads {
                payloads: vec![json_payload(br#""updated-ok""#)],
            })),
        }),
        stage: UpdateWorkflowExecutionLifecycleStage::Completed as i32,
        ..Default::default()
    }
    .encode_to_vec()
}

fn mock_server_callback(state: Arc<MockServerState>) -> CallbackBasedGrpcService {
    simple_callback(move |req: GrpcRequest| {
        *state
            .call_counts
            .lock()
            .unwrap()
            .entry(req.rpc.clone())
            .or_insert(0) += 1;
        match req.rpc.as_str() {
            "StartWorkflowExecution" => {
                let decoded = StartWorkflowExecutionRequest::decode(req.proto.as_ref()).unwrap();
                *state.start.lock().unwrap() = Some(decoded);
                Ok(StartWorkflowExecutionResponse {
                    run_id: "run-start-1".to_owned(),
                    ..Default::default()
                }
                .encode_to_vec())
            }
            "SignalWithStartWorkflowExecution" => {
                let decoded =
                    SignalWithStartWorkflowExecutionRequest::decode(req.proto.as_ref()).unwrap();
                if decoded.workflow_id == "already-exists" {
                    return Err(Status::already_exists("already started"));
                }
                if decoded.workflow_id == "retry-check" {
                    return Err(Status::unavailable("dropped"));
                }
                Ok(SignalWithStartWorkflowExecutionResponse {
                    run_id: "run-sws-1".to_owned(),
                    ..Default::default()
                }
                .encode_to_vec())
            }
            "SignalWorkflowExecution" => {
                let decoded = SignalWorkflowExecutionRequest::decode(req.proto.as_ref()).unwrap();
                let wf_id = decoded
                    .workflow_execution
                    .as_ref()
                    .map_or_default(|e| e.workflow_id.as_str());
                if wf_id == "unkeyed-fail" {
                    return Err(Status::unavailable("transient"));
                }
                *state.signal.lock().unwrap() = Some(decoded);
                Ok(Vec::new())
            }
            "RequestCancelWorkflowExecution" => {
                let decoded =
                    RequestCancelWorkflowExecutionRequest::decode(req.proto.as_ref()).unwrap();
                let wf_id = decoded
                    .workflow_execution
                    .as_ref()
                    .map_or_default(|e| e.workflow_id.as_str());
                if wf_id == "unkeyed-fail" {
                    return Err(Status::unavailable("transient"));
                }
                *state.cancel.lock().unwrap() = Some(decoded);
                Ok(Vec::new())
            }
            "TerminateWorkflowExecution" => Err(Status::unavailable("transient")),
            "QueryWorkflow" => Ok(QueryWorkflowResponse {
                query_result: Some(Payloads {
                    payloads: vec![json_payload(br#""healthy""#)],
                }),
                query_rejected: None,
                ..Default::default()
            }
            .encode_to_vec()),
            "DescribeWorkflowExecution" => Ok(mock_describe_response()),
            "UpdateWorkflowExecution" => {
                let decoded = UpdateWorkflowExecutionRequest::decode(req.proto.as_ref()).unwrap();
                *state.update.lock().unwrap() = Some(decoded);
                Ok(mock_update_response())
            }
            "ExecuteMultiOperation" => {
                let decoded = ExecuteMultiOperationRequest::decode(req.proto.as_ref()).unwrap();
                *state.uws.lock().unwrap() = Some(decoded);
                Ok(mock_multi_op_response())
            }
            "DescribeNamespace" => Err(Status::unavailable("down")),
            other => Err(Status::unimplemented(format!("unexpected rpc {other}"))),
        }
    })
}

#[tokio::test]
async fn client_rejects_expired_nan_oversized_and_oversized_memo_pre_wire() {
    let state = Arc::new(MockServerState::default());
    let mut opts = base_options();
    opts.max_payload_bytes = 256;
    let (startup, conn) = admit_with_callback(opts, mock_server_callback(Arc::clone(&state))).await;
    let client = conn.client();
    let op = OperationContext::with_timeout(Duration::from_secs(5));

    let expired_op = OperationContext::with_timeout(Duration::ZERO);
    let pre_expired = client
        .start_workflow(
            &expired_op,
            StartRequest::new(
                ClientContractWorkflow,
                NonBlankName::try_from("orders").unwrap(),
                WorkflowId::try_from("wf-expired").unwrap(),
                StartIdPolicy::RejectDuplicate,
                "hi".to_owned(),
            ),
        )
        .await;
    assert_eq!(
        pre_expired.unwrap_err(),
        StartWorkflowError::NotApplied(StartFailure::Stopped(Stopped::Deadline))
    );

    let nan_err = client
        .start_workflow(
            &op,
            StartRequest::new(
                FloatWorkflow,
                NonBlankName::try_from("orders").unwrap(),
                WorkflowId::try_from("wf-nan").unwrap(),
                StartIdPolicy::RejectDuplicate,
                FloatPayload { amount: f64::NAN },
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(
        nan_err,
        StartWorkflowError::NotApplied(StartFailure::InvalidInput)
    );

    let large_err = client
        .start_workflow(
            &op,
            StartRequest::new(
                ClientContractWorkflow,
                NonBlankName::try_from("orders").unwrap(),
                WorkflowId::try_from("wf-large").unwrap(),
                StartIdPolicy::RejectDuplicate,
                "a".repeat(300),
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(
        large_err,
        StartWorkflowError::NotApplied(StartFailure::TooLarge)
    );

    let mut huge_memo = BTreeMap::new();
    huge_memo.insert("blob".to_owned(), serde_json::json!("m".repeat(300)));
    let large_memo_err = client
        .start_workflow(
            &op,
            StartRequest::new(
                ClientContractWorkflow,
                NonBlankName::try_from("orders").unwrap(),
                WorkflowId::try_from("wf-large-memo").unwrap(),
                StartIdPolicy::RejectDuplicate,
                "ok".to_owned(),
            )
            .memo(huge_memo),
        )
        .await
        .unwrap_err();
    assert_eq!(
        large_memo_err,
        StartWorkflowError::NotApplied(StartFailure::TooLarge)
    );
    assert!(state.call_counts.lock().unwrap().is_empty());
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

fn contract_start_request(workflow_id: &str) -> StartRequest<ClientContractWorkflow> {
    StartRequest::new(
        ClientContractWorkflow,
        NonBlankName::try_from("orders").unwrap(),
        WorkflowId::try_from(workflow_id).unwrap(),
        StartIdPolicy::RejectDuplicate,
        "in".to_owned(),
    )
}

#[tokio::test]
async fn signal_with_start_maps_already_started_and_enforces_zero_retries() {
    let state = Arc::new(MockServerState::default());
    let (startup, conn) =
        admit_with_callback(base_options(), mock_server_callback(Arc::clone(&state))).await;
    let client = conn.client();
    let op = OperationContext::with_timeout(Duration::from_secs(5));

    let sws_already = client
        .signal_with_start(
            &op,
            contract_start_request("already-exists"),
            ClientContractWorkflow::notify,
            "sig".to_owned(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        sws_already,
        StartWorkflowError::NotApplied(StartFailure::AlreadyStarted)
    );

    let sws_retry = client
        .signal_with_start(
            &op,
            contract_start_request("retry-check"),
            ClientContractWorkflow::notify,
            "sig".to_owned(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        sws_retry,
        StartWorkflowError::OutcomeUnknown(UnknownOutcome::Unavailable)
    );
    assert_eq!(
        state.call_counts.lock().unwrap()["SignalWithStartWorkflowExecution"],
        2
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
}

#[tokio::test]
async fn client_enforces_zero_retries_and_request_ids() {
    let state = Arc::new(MockServerState::default());
    let (startup, conn) =
        admit_with_callback(base_options(), mock_server_callback(Arc::clone(&state))).await;
    let client = conn.client();
    let op = OperationContext::with_timeout(Duration::from_secs(5));

    let handle = client.workflow_handle::<ClientContractWorkflow>(
        WorkflowId::try_from("wf-1").unwrap(),
        Some(RunId::try_from("run-start-1").unwrap()),
    );
    handle
        .signal(
            &op,
            ClientContractWorkflow::notify,
            "note".to_owned(),
            MutationDedupe::Key(RequestKey::try_from("sig-key-1").unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(
        state.signal.lock().unwrap().as_ref().unwrap().request_id,
        "sig-key-1"
    );

    let unkeyed = client.workflow_handle::<ClientContractWorkflow>(
        WorkflowId::try_from("unkeyed-fail").unwrap(),
        None,
    );
    assert_eq!(
        unkeyed
            .signal(
                &op,
                ClientContractWorkflow::notify,
                "note".to_owned(),
                MutationDedupe::Unkeyed,
            )
            .await
            .unwrap_err(),
        MutationError::OutcomeUnknown(UnknownOutcome::Unavailable)
    );
    assert_eq!(
        state.call_counts.lock().unwrap()["SignalWorkflowExecution"],
        2
    );

    handle
        .cancel(
            &op,
            MutationDedupe::Key(RequestKey::try_from("cancel-key-1").unwrap()),
            Some(CompactString::const_new("operator")),
        )
        .await
        .unwrap();
    assert_eq!(
        state.cancel.lock().unwrap().as_ref().unwrap().request_id,
        "cancel-key-1"
    );
    assert_eq!(
        unkeyed
            .cancel(&op, MutationDedupe::Unkeyed, None)
            .await
            .unwrap_err(),
        MutationError::OutcomeUnknown(UnknownOutcome::Unavailable)
    );
    assert_eq!(
        state.call_counts.lock().unwrap()["RequestCancelWorkflowExecution"],
        2
    );
    assert_eq!(
        handle
            .terminate(&op, Some(CompactString::const_new("stop")))
            .await
            .unwrap_err(),
        MutationError::OutcomeUnknown(UnknownOutcome::Unavailable)
    );
    assert_eq!(
        state.call_counts.lock().unwrap()["TerminateWorkflowExecution"],
        1
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
}

#[tokio::test]
async fn probe_enforces_zero_retries_and_draining_short_circuit() {
    let state = Arc::new(MockServerState::default());
    let (startup, conn) =
        admit_with_callback(base_options(), mock_server_callback(Arc::clone(&state))).await;
    let probe = conn.probe();
    assert_eq!(probe.name(), PROBE_NAME);
    assert!(probe.check().await.is_err());
    assert_eq!(state.call_counts.lock().unwrap()["DescribeNamespace"], 1);
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
    assert!(probe.check().await.is_err());
    assert_eq!(state.call_counts.lock().unwrap()["DescribeNamespace"], 1);
}

#[tokio::test]
async fn client_propagates_trace_context_memo_update_query_and_describe() {
    install_test_propagator();
    let provider = SdkTracerProvider::builder().build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));

    let state = Arc::new(MockServerState::default());
    let mut opts = base_options();
    opts.max_payload_bytes = 256;
    let (startup, conn) = admit_with_callback(opts, mock_server_callback(Arc::clone(&state))).await;
    let client = conn.client();
    let op = OperationContext::with_timeout(Duration::from_secs(5));
    let dc = build_data_converter(256, None);

    let _dispatch = tracing::subscriber::set_default(subscriber);
    let parent_span = tracing::info_span!("api_request");
    let parent_trace_id = parent_span.context().span().span_context().trace_id();
    let mut memo = BTreeMap::new();
    memo.insert("env".to_owned(), serde_json::json!("test"));
    let req = StartRequest::new(
        ClientContractWorkflow,
        NonBlankName::try_from("orders").unwrap(),
        WorkflowId::try_from("wf-1").unwrap(),
        StartIdPolicy::AllowDuplicate {
            running_conflict: RunningConflictPolicy::UseExisting,
        },
        "hello".to_owned(),
    )
    .execution_timeout(Duration::from_mins(1))
    .run_timeout(Duration::from_secs(30))
    .task_timeout(Duration::from_secs(10))
    .memo(memo)
    .static_summary("summary")
    .static_details("details");
    let handle = tracing::Instrument::instrument(client.start_workflow(&op, req), parent_span)
        .await
        .unwrap();
    assert_eq!(handle.workflow_id(), "wf-1");
    assert_eq!(handle.run_id(), Some("run-start-1"));

    let start_req = state.start.lock().unwrap().clone().unwrap();
    assert_eq!(
        start_req.workflow_id_reuse_policy,
        ProtoWorkflowIdReusePolicy::AllowDuplicate as i32
    );
    assert_eq!(
        start_req.workflow_id_conflict_policy,
        ProtoWorkflowIdConflictPolicy::UseExisting as i32
    );
    assert!(
        start_req
            .memo
            .as_ref()
            .is_some_and(|m| m.fields.contains_key("env"))
    );
    assert_eq!(
        extract_context_from_headers(&start_req.header.unwrap().fields, dc.payload_converter())
            .span()
            .span_context()
            .trace_id(),
        parent_trace_id
    );

    assert_eq!(
        handle
            .query(&op, ClientContractWorkflow::status, "arg".to_owned())
            .await
            .unwrap(),
        "healthy"
    );
    let described = handle.describe(&op).await.unwrap();
    assert_eq!(described.id(), "wf-1");
    assert_eq!(described.status(), WorkflowExecutionStatus::Running);

    let upd_out = handle
        .execute_update(
            &op,
            ClientContractWorkflow::apply,
            UpdateId::try_from("upd-explicit-1").unwrap(),
            "in".to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(upd_out, "updated-ok");
    let captured_upd = state.update.lock().unwrap().clone().unwrap();
    assert_eq!(
        captured_upd.request.unwrap().meta.unwrap().update_id,
        "upd-explicit-1"
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

#[tokio::test]
async fn client_update_with_start_injects_trace_headers_into_both_operations() {
    install_test_propagator();
    let provider = SdkTracerProvider::builder().build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));

    let state = Arc::new(MockServerState::default());
    let (startup, conn) =
        admit_with_callback(base_options(), mock_server_callback(Arc::clone(&state))).await;
    let client = conn.client();
    let op = OperationContext::with_timeout(Duration::from_secs(5));
    let dc = build_data_converter(256, None);

    let _dispatch = tracing::subscriber::set_default(subscriber);
    let parent_span = tracing::info_span!("uws_request");
    let parent_trace_id = parent_span.context().span().span_context().trace_id();
    let uws_req_data = UpdateWithStartRequest::new(
        StartRequest::new(
            ClientContractWorkflow,
            NonBlankName::try_from("orders").unwrap(),
            WorkflowId::try_from("wf-uws").unwrap(),
            StartIdPolicy::AllowDuplicate {
                running_conflict: RunningConflictPolicy::UseExisting,
            },
            "in".to_owned(),
        ),
        ClientContractWorkflow::apply,
        UpdateId::try_from("upd-uws-1").unwrap(),
        "arg".to_owned(),
    );
    let uws_out = tracing::Instrument::instrument(
        client.execute_update_with_start(&op, uws_req_data),
        parent_span,
    )
    .await
    .unwrap();
    assert_eq!(uws_out, "uws-ok");

    let uws_req = state.uws.lock().unwrap().clone().unwrap();
    let Some(execute_multi_operation_request::operation::Operation::StartWorkflow(start_op)) =
        uws_req.operations[0].operation.as_ref()
    else {
        panic!("expected StartWorkflow operation");
    };
    let Some(execute_multi_operation_request::operation::Operation::UpdateWorkflow(update_op)) =
        uws_req.operations[1].operation.as_ref()
    else {
        panic!("expected UpdateWorkflow operation");
    };
    assert_eq!(
        extract_context_from_headers(
            &start_op.header.as_ref().unwrap().fields,
            dc.payload_converter()
        )
        .span()
        .span_context()
        .trace_id(),
        parent_trace_id
    );
    let update_fields = &update_op
        .request
        .as_ref()
        .unwrap()
        .input
        .as_ref()
        .unwrap()
        .header
        .as_ref()
        .unwrap()
        .fields;
    assert_eq!(
        extract_context_from_headers(update_fields, dc.payload_converter())
            .span()
            .span_context()
            .trace_id(),
        parent_trace_id
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

#[tokio::test(start_paused = true)]
async fn stalled_rpc_and_probe_honor_short_operation_context_deadlines() {
    let dispatched = Arc::new(Notify::new());
    let dispatched_cb = Arc::clone(&dispatched);
    let callback = CallbackBasedGrpcService {
        callback: Arc::new(move |mut req: GrpcRequest| {
            let dispatched_cb = Arc::clone(&dispatched_cb);
            async move {
                if req.rpc == "GetSystemInfo" {
                    req.headers.clear();
                    return Ok(GrpcSuccessResponse {
                        headers: req.headers,
                        proto: Vec::new(),
                    });
                }
                dispatched_cb.notify_one();
                std::future::pending::<Result<GrpcSuccessResponse, Status>>().await
            }
            .boxed()
        }),
    };

    let mut opts = base_options();
    opts.probe_budget = Duration::from_millis(50);
    let (startup, conn) = admit_with_callback(opts, callback).await;
    let client = conn.client();

    let short_op = OperationContext::with_timeout(Duration::from_millis(20));
    let start_task = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .start_workflow(
                    &short_op,
                    StartRequest::new(
                        ClientContractWorkflow,
                        NonBlankName::try_from("orders").unwrap(),
                        WorkflowId::try_from("wf-stall").unwrap(),
                        StartIdPolicy::RejectDuplicate,
                        "hi".to_owned(),
                    ),
                )
                .await
        }
    });
    dispatched.notified().await;
    tokio::time::advance(Duration::from_millis(25)).await;
    assert_eq!(
        start_task.await.unwrap().unwrap_err(),
        StartWorkflowError::OutcomeUnknown(UnknownOutcome::Stopped(Stopped::Deadline))
    );

    let cancel_op = OperationContext::with_timeout(Duration::from_secs(30));
    let cancel_task = tokio::spawn({
        let client = client.clone();
        let op = cancel_op.clone();
        async move {
            client
                .workflow_handle::<ClientContractWorkflow>(
                    WorkflowId::try_from("wf-cancel-inflight").unwrap(),
                    None,
                )
                .signal(
                    &op,
                    ClientContractWorkflow::notify,
                    "note".to_owned(),
                    MutationDedupe::Unkeyed,
                )
                .await
        }
    });
    dispatched.notified().await;
    cancel_op.cancel();
    assert_eq!(
        cancel_task.await.unwrap().unwrap_err(),
        MutationError::OutcomeUnknown(UnknownOutcome::Stopped(Stopped::Cancelled))
    );

    let probe = conn.probe();
    let probe_task = tokio::spawn(async move { probe.check().await });
    dispatched.notified().await;
    tokio::time::advance(Duration::from_millis(60)).await;
    let probe_err = probe_task.await.unwrap().unwrap_err();
    assert!(probe_err.to_string().contains("timed out"), "{probe_err}");

    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

fn history_and_rejected_query_callback(
    attrs: history_event::Attributes,
) -> CallbackBasedGrpcService {
    simple_callback(move |req: GrpcRequest| match req.rpc.as_str() {
        "GetWorkflowExecutionHistory" => Ok(GetWorkflowExecutionHistoryResponse {
            history: Some(History {
                events: vec![HistoryEvent {
                    event_id: 2,
                    attributes: Some(attrs.clone()),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        }
        .encode_to_vec()),
        "QueryWorkflow" => Ok(QueryWorkflowResponse {
            query_result: None,
            query_rejected: Some(QueryRejected {
                status: ProtoWorkflowExecutionStatus::Completed as i32,
            }),
            ..Default::default()
        }
        .encode_to_vec()),
        _ => Err(Status::unimplemented("unexpected")),
    })
}

#[tokio::test]
async fn get_result_and_query_classify_terminal_and_rejection_outcomes() {
    let cases: [(
        &str,
        history_event::Attributes,
        Result<&str, WorkflowResultError>,
    ); 6] = [
        (
            "completed",
            history_event::Attributes::WorkflowExecutionCompletedEventAttributes(
                WorkflowExecutionCompletedEventAttributes {
                    result: Some(Payloads {
                        payloads: vec![json_payload(br#""done""#)],
                    }),
                    ..Default::default()
                },
            ),
            Ok("done"),
        ),
        (
            "failed",
            history_event::Attributes::WorkflowExecutionFailedEventAttributes(
                WorkflowExecutionFailedEventAttributes::default(),
            ),
            Err(WorkflowResultError::WorkflowFailed),
        ),
        (
            "cancelled",
            history_event::Attributes::WorkflowExecutionCanceledEventAttributes(
                WorkflowExecutionCanceledEventAttributes::default(),
            ),
            Err(WorkflowResultError::Cancelled),
        ),
        (
            "terminated",
            history_event::Attributes::WorkflowExecutionTerminatedEventAttributes(
                WorkflowExecutionTerminatedEventAttributes::default(),
            ),
            Err(WorkflowResultError::Terminated),
        ),
        (
            "timed_out",
            history_event::Attributes::WorkflowExecutionTimedOutEventAttributes(
                WorkflowExecutionTimedOutEventAttributes::default(),
            ),
            Err(WorkflowResultError::TimedOut),
        ),
        (
            "continued",
            history_event::Attributes::WorkflowExecutionContinuedAsNewEventAttributes(
                WorkflowExecutionContinuedAsNewEventAttributes {
                    new_execution_run_id: String::new(),
                    ..Default::default()
                },
            ),
            Err(WorkflowResultError::Internal),
        ),
    ];

    for (wf_id, attrs, expected) in cases {
        let (startup, conn) =
            admit_with_callback(base_options(), history_and_rejected_query_callback(attrs)).await;
        let handle = conn.client().workflow_handle::<ClientContractWorkflow>(
            WorkflowId::try_from(wf_id).unwrap(),
            Some(RunId::try_from("run-1").unwrap()),
        );
        let op = OperationContext::with_timeout(Duration::from_secs(5));
        let actual = handle.get_result(&op).await;
        match expected {
            Ok(val) => assert_eq!(actual.unwrap(), val, "{wf_id}"),
            Err(err) => assert_eq!(actual.unwrap_err(), err, "{wf_id}"),
        }
        assert_eq!(
            handle
                .query(&op, ClientContractWorkflow::status, "q".to_owned())
                .await
                .unwrap_err(),
            ReadError::QueryRejected
        );
        assert_eq!(
            startup.close(Instant::now() + Duration::from_secs(1)).await,
            CloseOutcome::Complete
        );
    }
}

#[test]
fn error_mappers_classify_status_update_and_continued_variants() {
    let mut too_large_status = Status::resource_exhausted("too big");
    too_large_status
        .metadata_mut()
        .insert(MESSAGE_TOO_LARGE_KEY, 0.into());
    assert_eq!(
        map_start_status(&too_large_status),
        StartWorkflowError::NotApplied(StartFailure::TooLarge)
    );
    assert_eq!(
        map_start_status(&Status::resource_exhausted("rate limited")),
        StartWorkflowError::NotApplied(StartFailure::Busy)
    );
    assert_eq!(
        map_start_status(&Status::unauthenticated("bad token")),
        StartWorkflowError::NotApplied(StartFailure::Unauthenticated)
    );
    assert_eq!(
        map_start_status(&Status::permission_denied("no access")),
        StartWorkflowError::NotApplied(StartFailure::PermissionDenied)
    );
    assert_eq!(
        map_start_status(&Status::failed_precondition("conflict")),
        StartWorkflowError::NotApplied(StartFailure::Conflict)
    );
    assert_eq!(
        map_start_status(&Status::deadline_exceeded("timeout")),
        StartWorkflowError::OutcomeUnknown(UnknownOutcome::Stopped(Stopped::Deadline))
    );
    assert_eq!(
        map_result_error(&SdkWorkflowGetResultError::ContinuedAsNew),
        WorkflowResultError::ContinuedAsNew
    );
    assert_eq!(
        map_update_error(&SdkWorkflowUpdateError::Failed(Box::default())),
        UpdateWorkflowError::NotApplied(UpdateFailure::UpdateRejected)
    );
    assert_eq!(
        map_update_error(&SdkWorkflowUpdateError::Rpc(Status::already_exists(
            "secret-update-id"
        ))),
        UpdateWorkflowError::NotApplied(UpdateFailure::AlreadyStarted)
    );
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary invalid CA file verifies fail-closed TlsSetup"
)]
async fn startup_admit_classifies_cancelled_timed_out_missing_credential_and_bad_ca_without_leaks()
{
    let dir = tempfile::tempdir().unwrap();

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let mut startup = TemporalStartup::prepare(base_options(), &OperationContext::unbounded());
    let err = startup.admit(cancelled).await.unwrap_err();
    assert_eq!(err, ConnectError::Cancelled);

    let expired = OperationContext::with_timeout(Duration::ZERO);
    let mut startup = TemporalStartup::prepare(base_options(), &expired);
    let err = startup.admit(CancellationToken::new()).await.unwrap_err();
    assert_eq!(err, ConnectError::TimedOut);

    let missing_key = dir.path().join("sentinel-missing-api.key");
    let mut opts = base_options();
    opts.endpoint = Url::parse("https://temporal.example.invalid:7233").unwrap();
    opts.auth = TemporalAuth::ApiKeyFile(missing_key);
    let mut startup = TemporalStartup::prepare(opts, &OperationContext::unbounded());
    let err = startup.admit(CancellationToken::new()).await.unwrap_err();
    assert_eq!(err, ConnectError::CredentialRead { kind: "api_key" });
    let dump = format!("{err}; {err:?}");
    assert!(!dump.contains("sentinel-missing-api.key"));

    let bad_ca = dir.path().join("sentinel-bad-ca.pem");
    std::fs::write(&bad_ca, "not-a-valid-certificate").unwrap();
    let mut opts = base_options();
    opts.endpoint = Url::parse("https://temporal.example.invalid:7233").unwrap();
    opts.tls = Some(TemporalTlsOptions {
        server_root_ca_path: Some(bad_ca),
        domain: None,
        mtls: None,
    });
    let mut startup = TemporalStartup::prepare(opts, &OperationContext::unbounded());
    let err = startup.admit(CancellationToken::new()).await.unwrap_err();
    std::assert_matches!(err, ConnectError::TlsSetup(_));
    let dump = format!("{err}; {err:?}");
    assert!(!dump.contains("sentinel-bad-ca.pem"));
    assert!(!dump.contains("temporal.example.invalid"));
}

struct StrictXorCodec(u8);

impl StrictXorCodec {
    const ENCODING: &'static [u8] = b"binary/xor";
}

impl PayloadCodec for StrictXorCodec {
    fn encode(
        &self,
        _context: &SerializationContextData,
        payloads: Vec<Payload>,
    ) -> BoxFuture<'static, Result<Vec<Payload>, PayloadConversionError>> {
        let mask = self.0;
        Box::pin(async move {
            Ok(payloads
                .into_iter()
                .map(|payload| {
                    let mut encoded_bytes = payload.encode_to_vec();
                    for byte in &mut encoded_bytes {
                        *byte ^= mask;
                    }
                    Payload {
                        metadata: HashMap::from([(
                            ENCODING_PAYLOAD_KEY.to_owned(),
                            Self::ENCODING.to_vec(),
                        )]),
                        data: encoded_bytes,
                        external_payloads: Vec::new(),
                    }
                })
                .collect())
        })
    }

    fn decode(
        &self,
        _context: &SerializationContextData,
        payloads: Vec<Payload>,
    ) -> BoxFuture<'static, Result<Vec<Payload>, PayloadConversionError>> {
        let mask = self.0;
        Box::pin(async move {
            payloads
                .into_iter()
                .map(|mut payload| {
                    if payload
                        .metadata
                        .get(ENCODING_PAYLOAD_KEY)
                        .map(Vec::as_slice)
                        != Some(Self::ENCODING)
                    {
                        return Err(PayloadConversionError::WrongEncoding);
                    }
                    for byte in &mut payload.data {
                        *byte ^= mask;
                    }
                    Payload::decode(payload.data.as_slice())
                        .map_err(|err| PayloadConversionError::EncodingError(Box::new(err)))
                })
                .collect()
        })
    }
}

struct FaultyEncodeCodec {
    return_count: Option<usize>,
}

impl PayloadCodec for FaultyEncodeCodec {
    fn encode(
        &self,
        _context: &SerializationContextData,
        payloads: Vec<Payload>,
    ) -> BoxFuture<'static, Result<Vec<Payload>, PayloadConversionError>> {
        let return_count = self.return_count;
        Box::pin(async move {
            match return_count {
                Some(0) => Ok(Vec::new()),
                Some(count) => Ok(std::iter::repeat_n(
                    payloads.into_iter().next().unwrap_or_default(),
                    count,
                )
                .collect()),
                None => Err(PayloadConversionError::EncodingError(
                    std::io::Error::other("secret-kms-key-id-99").into(),
                )),
            }
        })
    }

    fn decode(
        &self,
        _context: &SerializationContextData,
        payloads: Vec<Payload>,
    ) -> BoxFuture<'static, Result<Vec<Payload>, PayloadConversionError>> {
        Box::pin(async move { Ok(payloads) })
    }
}

struct FailingDecodeCodec;

impl PayloadCodec for FailingDecodeCodec {
    fn encode(
        &self,
        _context: &SerializationContextData,
        payloads: Vec<Payload>,
    ) -> BoxFuture<'static, Result<Vec<Payload>, PayloadConversionError>> {
        Box::pin(async move { Ok(payloads) })
    }

    fn decode(
        &self,
        _context: &SerializationContextData,
        _payloads: Vec<Payload>,
    ) -> BoxFuture<'static, Result<Vec<Payload>, PayloadConversionError>> {
        Box::pin(async move {
            Err(PayloadConversionError::EncodingError(
                std::io::Error::other("secret-decrypt-error").into(),
            ))
        })
    }
}

async fn assert_xor_encoded_trace_header(
    mut fields: HashMap<String, Payload>,
    dc: &temporalio_common::data_converters::DataConverter,
    expected_trace_id: opentelemetry::trace::TraceId,
) {
    let wire_header = fields
        .remove(TRACER_DATA_HEADER)
        .expect("wire _tracer-data header present");
    assert_eq!(
        wire_header
            .metadata
            .get(ENCODING_PAYLOAD_KEY)
            .map(Vec::as_slice),
        Some(StrictXorCodec::ENCODING)
    );
    let wf_ctx = SerializationContextData::Workflow(WorkflowSerializationContext::new());
    let mut decoded = dc.codec().decode(&wf_ctx, vec![wire_header]).await.unwrap();
    assert_eq!(decoded.len(), 1);
    let extracted = extract_context_from_headers(
        &HashMap::from([(TRACER_DATA_HEADER.to_owned(), decoded.remove(0))]),
        dc.payload_converter(),
    );
    assert_eq!(
        extracted.span().span_context().trace_id(),
        expected_trace_id
    );
}

fn codec_trace_callback(
    state: Arc<MockServerState>,
    encoded_update_payload: Payload,
) -> CallbackBasedGrpcService {
    simple_callback(move |req: GrpcRequest| match req.rpc.as_str() {
        "StartWorkflowExecution" => {
            *state.start.lock().unwrap() =
                Some(StartWorkflowExecutionRequest::decode(req.proto.as_ref()).unwrap());
            Ok(StartWorkflowExecutionResponse {
                run_id: "run-1".to_owned(),
                ..Default::default()
            }
            .encode_to_vec())
        }
        "SignalWorkflowExecution" => {
            *state.signal.lock().unwrap() =
                Some(SignalWorkflowExecutionRequest::decode(req.proto.as_ref()).unwrap());
            Ok(Vec::new())
        }
        "ExecuteMultiOperation" => {
            *state.uws.lock().unwrap() =
                Some(ExecuteMultiOperationRequest::decode(req.proto.as_ref()).unwrap());
            Ok(multi_op_response_with_update(update_response_with_outcome(
                Some(Outcome {
                    value: Some(outcome::Value::Success(Payloads {
                        payloads: vec![encoded_update_payload.clone()],
                    })),
                }),
            )))
        }
        _ => Err(Status::unimplemented("unexpected")),
    })
}

#[tokio::test]
async fn client_trace_interceptor_encodes_tracer_data_header_with_payload_codec() {
    install_test_propagator();
    let provider = SdkTracerProvider::builder().build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));

    let codec: Arc<dyn PayloadCodec + Send + Sync> = Arc::new(StrictXorCodec(0x5A));
    let dc = build_data_converter(4096, Some(Arc::clone(&codec)));
    let wf_ctx = SerializationContextData::Workflow(WorkflowSerializationContext::new());
    let encoded_update_payload = dc
        .to_payload(&wf_ctx, &"codec-updated".to_owned())
        .await
        .unwrap();
    let state = Arc::new(MockServerState::default());

    let mut opts = base_options();
    opts.payload_codec = Some(Arc::clone(&codec));
    let (startup, conn) = admit_with_callback(
        opts,
        codec_trace_callback(Arc::clone(&state), encoded_update_payload),
    )
    .await;
    let client = conn.client();
    let op = OperationContext::with_timeout(Duration::from_secs(5));

    let _dispatch = tracing::subscriber::set_default(subscriber);
    let parent_span = tracing::info_span!("codec_trace_req");
    let parent_trace_id = parent_span.context().span().span_context().trace_id();
    let handle = tracing::Instrument::instrument(
        client.start_workflow(&op, contract_start_request("wf-codec-trace")),
        parent_span.clone(),
    )
    .await
    .unwrap();
    tracing::Instrument::instrument(
        handle.signal(
            &op,
            ClientContractWorkflow::notify,
            "sig".to_owned(),
            MutationDedupe::Unkeyed,
        ),
        parent_span.clone(),
    )
    .await
    .unwrap();
    let uws_out = tracing::Instrument::instrument(
        client.execute_update_with_start(
            &op,
            UpdateWithStartRequest::new(
                contract_start_request("wf-codec-uws"),
                ClientContractWorkflow::apply,
                UpdateId::try_from("upd-codec-1").unwrap(),
                "arg".to_owned(),
            ),
        ),
        parent_span,
    )
    .await
    .unwrap();
    assert_eq!(uws_out, "codec-updated");

    let start_req = state.start.lock().unwrap().clone().unwrap();
    assert_xor_encoded_trace_header(start_req.header.unwrap().fields, &dc, parent_trace_id).await;
    let sig_req = state.signal.lock().unwrap().clone().unwrap();
    assert_xor_encoded_trace_header(sig_req.header.unwrap().fields, &dc, parent_trace_id).await;

    let uws_req = state.uws.lock().unwrap().clone().unwrap();
    let Some(execute_multi_operation_request::operation::Operation::StartWorkflow(start_op)) =
        uws_req.operations[0].operation.as_ref()
    else {
        panic!("expected StartWorkflow operation");
    };
    let Some(execute_multi_operation_request::operation::Operation::UpdateWorkflow(update_op)) =
        uws_req.operations[1].operation.as_ref()
    else {
        panic!("expected UpdateWorkflow operation");
    };
    assert_xor_encoded_trace_header(
        start_op.header.clone().unwrap().fields,
        &dc,
        parent_trace_id,
    )
    .await;
    assert_xor_encoded_trace_header(
        update_op
            .request
            .clone()
            .unwrap()
            .input
            .unwrap()
            .header
            .unwrap()
            .fields,
        &dc,
        parent_trace_id,
    )
    .await;

    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

#[tokio::test]
async fn client_trace_interceptor_fails_closed_on_codec_encode_errors() {
    install_test_propagator();
    let provider = SdkTracerProvider::builder().build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let op = OperationContext::with_timeout(Duration::from_secs(5));
    let _dispatch = tracing::subscriber::set_default(subscriber);
    let parent_span = tracing::info_span!("codec_trace_err");

    for return_count in [None, Some(0), Some(2)] {
        let err_state = Arc::new(MockServerState::default());
        let mut bad_opts = base_options();
        bad_opts.payload_codec = Some(Arc::new(FaultyEncodeCodec { return_count }));
        let (bad_startup, bad_conn) =
            admit_with_callback(bad_opts, mock_server_callback(Arc::clone(&err_state))).await;
        let err = tracing::Instrument::instrument(
            bad_conn
                .client()
                .start_workflow(&op, contract_start_request("wf-bad-codec")),
            parent_span.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err,
            StartWorkflowError::NotApplied(StartFailure::Internal),
            "return_count={return_count:?}"
        );
        let upd_err = tracing::Instrument::instrument(
            bad_conn
                .client()
                .workflow_handle::<ClientContractWorkflow>(
                    WorkflowId::try_from("wf-bad-codec").unwrap(),
                    None,
                )
                .execute_update(
                    &op,
                    ClientContractWorkflow::apply,
                    UpdateId::try_from("upd-bad-codec").unwrap(),
                    "ok".to_owned(),
                ),
            parent_span.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            upd_err,
            UpdateWorkflowError::NotApplied(UpdateFailure::Internal),
            "return_count={return_count:?}"
        );
        assert!(err_state.call_counts.lock().unwrap().is_empty());
        assert!(!format!("{err}; {err:?}").contains("secret-kms-key-id-99"));
        assert!(!format!("{upd_err}; {upd_err:?}").contains("secret-kms-key-id-99"));
        assert_eq!(
            bad_startup
                .close(Instant::now() + Duration::from_secs(1))
                .await,
            CloseOutcome::Complete
        );
    }
}

fn update_response_with_outcome(outcome: Option<Outcome>) -> UpdateWorkflowExecutionResponse {
    UpdateWorkflowExecutionResponse {
        update_ref: Some(UpdateRef {
            workflow_execution: Some(WorkflowExecution {
                workflow_id: "wf-1".to_owned(),
                run_id: "run-1".to_owned(),
            }),
            update_id: "upd-1".to_owned(),
        }),
        outcome,
        stage: UpdateWorkflowExecutionLifecycleStage::Completed as i32,
        ..Default::default()
    }
}

fn multi_op_response_with_update(update_response: UpdateWorkflowExecutionResponse) -> Vec<u8> {
    ExecuteMultiOperationResponse {
        responses: vec![
            execute_multi_operation_response::Response {
                response: Some(
                    execute_multi_operation_response::response::Response::StartWorkflow(
                        StartWorkflowExecutionResponse {
                            run_id: "run-1".to_owned(),
                            ..Default::default()
                        },
                    ),
                ),
            },
            execute_multi_operation_response::Response {
                response: Some(
                    execute_multi_operation_response::response::Response::UpdateWorkflow(
                        update_response,
                    ),
                ),
            },
        ],
    }
    .encode_to_vec()
}

#[tokio::test]
async fn post_commit_update_decode_and_malformed_outcome_errors_classify_as_outcome_unknown() {
    let op = OperationContext::with_timeout(Duration::from_secs(5));
    let oversized_json = format!(r#""{}""#, "x".repeat(300));
    let bad_outcomes: [(&str, Option<Outcome>); 5] = [
        (
            "type_mismatch_json",
            Some(Outcome {
                value: Some(outcome::Value::Success(Payloads {
                    payloads: vec![json_payload(b"123")],
                })),
            }),
        ),
        (
            "malformed_json",
            Some(Outcome {
                value: Some(outcome::Value::Success(Payloads {
                    payloads: vec![json_payload(b"{broken")],
                })),
            }),
        ),
        (
            "oversized_json",
            Some(Outcome {
                value: Some(outcome::Value::Success(Payloads {
                    payloads: vec![json_payload(oversized_json.as_bytes())],
                })),
            }),
        ),
        (
            "empty_payloads",
            Some(Outcome {
                value: Some(outcome::Value::Success(Payloads {
                    payloads: Vec::new(),
                })),
            }),
        ),
        ("missing_outcome_value", Some(Outcome { value: None })),
    ];

    for (label, outcome) in bad_outcomes {
        let outcome_for_cb = outcome.clone();
        let callback = simple_callback(move |req: GrpcRequest| match req.rpc.as_str() {
            "UpdateWorkflowExecution" => {
                Ok(update_response_with_outcome(outcome_for_cb.clone()).encode_to_vec())
            }
            "ExecuteMultiOperation" => Ok(multi_op_response_with_update(
                update_response_with_outcome(outcome_for_cb.clone()),
            )),
            _ => Err(Status::unimplemented("unexpected")),
        });
        let mut opts = base_options();
        opts.max_payload_bytes = 256;
        let (startup, conn) = admit_with_callback(opts, callback).await;
        let client = conn.client();
        let handle = client.workflow_handle::<ClientContractWorkflow>(
            WorkflowId::try_from("wf-1").unwrap(),
            Some(RunId::try_from("run-1").unwrap()),
        );

        let upd_err = handle
            .execute_update(
                &op,
                ClientContractWorkflow::apply,
                UpdateId::try_from("upd-1").unwrap(),
                "ok".to_owned(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            upd_err,
            UpdateWorkflowError::OutcomeUnknown(UnknownOutcome::Unavailable),
            "execute_update case={label}"
        );

        let uws_err = client
            .execute_update_with_start(
                &op,
                UpdateWithStartRequest::new(
                    contract_start_request("wf-1"),
                    ClientContractWorkflow::apply,
                    UpdateId::try_from("upd-1").unwrap(),
                    "ok".to_owned(),
                ),
            )
            .await
            .unwrap_err();
        assert_eq!(
            uws_err,
            UpdateWorkflowError::OutcomeUnknown(UnknownOutcome::Unavailable),
            "execute_update_with_start case={label}"
        );

        assert_eq!(
            startup.close(Instant::now() + Duration::from_secs(1)).await,
            CloseOutcome::Complete
        );
    }
}

#[tokio::test]
async fn post_commit_update_codec_and_malformed_multi_op_classify_as_outcome_unknown() {
    let op = OperationContext::with_timeout(Duration::from_secs(5));
    let codec_cb = simple_callback(|req: GrpcRequest| match req.rpc.as_str() {
        "UpdateWorkflowExecution" => Ok(mock_update_response()),
        "ExecuteMultiOperation" => Ok(mock_multi_op_response()),
        _ => Err(Status::unimplemented("unexpected")),
    });
    let mut codec_opts = base_options();
    codec_opts.payload_codec = Some(Arc::new(FailingDecodeCodec));
    let (startup, conn) = admit_with_callback(codec_opts, codec_cb).await;
    let client = conn.client();
    let handle = client
        .workflow_handle::<ClientContractWorkflow>(WorkflowId::try_from("wf-1").unwrap(), None);
    assert_eq!(
        handle
            .execute_update(
                &op,
                ClientContractWorkflow::apply,
                UpdateId::try_from("upd-codec").unwrap(),
                "ok".to_owned(),
            )
            .await
            .unwrap_err(),
        UpdateWorkflowError::OutcomeUnknown(UnknownOutcome::Unavailable)
    );
    assert_eq!(
        client
            .execute_update_with_start(
                &op,
                UpdateWithStartRequest::new(
                    contract_start_request("wf-1"),
                    ClientContractWorkflow::apply,
                    UpdateId::try_from("upd-codec-uws").unwrap(),
                    "ok".to_owned(),
                ),
            )
            .await
            .unwrap_err(),
        UpdateWorkflowError::OutcomeUnknown(UnknownOutcome::Unavailable)
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );

    let malformed_multi_cb = simple_callback(|req: GrpcRequest| match req.rpc.as_str() {
        "ExecuteMultiOperation" => Ok(ExecuteMultiOperationResponse {
            responses: Vec::new(),
        }
        .encode_to_vec()),
        _ => Err(Status::unimplemented("unexpected")),
    });
    let (startup, conn) = admit_with_callback(base_options(), malformed_multi_cb).await;
    assert_eq!(
        conn.client()
            .execute_update_with_start(
                &op,
                UpdateWithStartRequest::new(
                    contract_start_request("wf-1"),
                    ClientContractWorkflow::apply,
                    UpdateId::try_from("upd-malformed-multi").unwrap(),
                    "ok".to_owned(),
                ),
            )
            .await
            .unwrap_err(),
        UpdateWorkflowError::OutcomeUnknown(UnknownOutcome::Unavailable)
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

#[tokio::test]
async fn pre_dispatch_update_payload_errors_remain_not_applied() {
    let op = OperationContext::with_timeout(Duration::from_secs(5));
    let pre_state = Arc::new(MockServerState::default());
    let mut pre_opts = base_options();
    pre_opts.max_payload_bytes = 256;
    let (startup, conn) =
        admit_with_callback(pre_opts, mock_server_callback(Arc::clone(&pre_state))).await;
    let client = conn.client();
    let float_handle =
        client.workflow_handle::<FloatWorkflow>(WorkflowId::try_from("wf-float").unwrap(), None);
    assert_eq!(
        float_handle
            .execute_update(
                &op,
                FloatWorkflow::adjust,
                UpdateId::try_from("upd-nan").unwrap(),
                FloatPayload { amount: f64::NAN },
            )
            .await
            .unwrap_err(),
        UpdateWorkflowError::NotApplied(UpdateFailure::InvalidInput)
    );
    let str_handle = client
        .workflow_handle::<ClientContractWorkflow>(WorkflowId::try_from("wf-big").unwrap(), None);
    assert_eq!(
        str_handle
            .execute_update(
                &op,
                ClientContractWorkflow::apply,
                UpdateId::try_from("upd-big").unwrap(),
                "z".repeat(300),
            )
            .await
            .unwrap_err(),
        UpdateWorkflowError::NotApplied(UpdateFailure::TooLarge)
    );
    assert_eq!(
        client
            .execute_update_with_start(
                &op,
                UpdateWithStartRequest::new(
                    contract_start_request("wf-big-uws"),
                    ClientContractWorkflow::apply,
                    UpdateId::try_from("upd-big-uws").unwrap(),
                    "z".repeat(300),
                ),
            )
            .await
            .unwrap_err(),
        UpdateWorkflowError::NotApplied(UpdateFailure::TooLarge)
    );
    assert!(pre_state.call_counts.lock().unwrap().is_empty());
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

#[tokio::test]
async fn proxy_url_validation_admits_only_plaintext_http_without_userinfo() {
    for bad_proxy in [
        "https://proxy.internal:8443",
        "http://user:secret@proxy.internal:8080",
        "http://user@proxy.internal:8080",
        "http://proxy.internal:8080?q=1",
        "http://proxy.internal:8080#frag",
        "socks5://proxy.internal:1080",
    ] {
        let parsed = Url::parse(bad_proxy).unwrap();
        let mut opts = base_options();
        opts.dns_load_balancing = false;
        opts.http_proxy_url = Some(parsed.clone());
        let mut startup = TemporalStartup::prepare(opts, &OperationContext::unbounded());
        let err = startup.admit(CancellationToken::new()).await.unwrap_err();
        std::assert_matches!(err, ConnectError::InvalidConfig(_), "{bad_proxy}: {err:?}");
        if matches!(parsed.scheme(), "https" | "socks5")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            std::assert_matches!(
                proxy_options_from_url(&parsed),
                Err(ConnectError::InvalidConfig(_)),
                "{bad_proxy}"
            );
        }
    }

    let valid_url = Url::parse("http://proxy.internal:8080").unwrap();
    let mut valid_opts = base_options();
    valid_opts.dns_load_balancing = false;
    valid_opts.http_proxy_url = Some(valid_url.clone());
    assert!(valid_opts.validate().is_ok());
    let proxy_opts = proxy_options_from_url(&valid_url).unwrap();
    assert_eq!(proxy_opts.target_addr, "proxy.internal:8080");
    assert_eq!(proxy_opts.basic_auth, None);
}

#[tokio::test]
async fn http_endpoints_reject_tls_options_even_with_plaintext_or_trusted_network() {
    let tls_cases: [(&str, TemporalTlsOptions); 5] = [
        (
            "root_ca",
            TemporalTlsOptions {
                server_root_ca_path: Some("/etc/temporal/ca.pem".into()),
                domain: None,
                mtls: None,
            },
        ),
        (
            "domain",
            TemporalTlsOptions {
                server_root_ca_path: None,
                domain: Some("temporal.internal".to_owned()),
                mtls: None,
            },
        ),
        (
            "mtls_static",
            TemporalTlsOptions {
                server_root_ca_path: None,
                domain: None,
                mtls: Some(TemporalMtlsOptions {
                    client_cert_path: "/etc/temporal/client.crt".into(),
                    client_key_path: "/etc/temporal/client.key".into(),
                    rotation: TlsRotationMode::Static,
                }),
            },
        ),
        (
            "mtls_dynamic",
            TemporalTlsOptions {
                server_root_ca_path: None,
                domain: None,
                mtls: Some(TemporalMtlsOptions {
                    client_cert_path: "/etc/temporal/client.crt".into(),
                    client_key_path: "/etc/temporal/client.key".into(),
                    rotation: TlsRotationMode::Dynamic,
                }),
            },
        ),
        (
            "empty_tls",
            TemporalTlsOptions {
                server_root_ca_path: None,
                domain: None,
                mtls: None,
            },
        ),
    ];

    for (allow_plaintext, trusted_network) in [(true, false), (false, true)] {
        for (label, tls) in &tls_cases {
            let mut opts = base_options();
            opts.endpoint = Url::parse("http://127.0.0.1:7233").unwrap();
            opts.allow_plaintext = allow_plaintext;
            opts.trusted_network = trusted_network;
            opts.dns_load_balancing = false;
            opts.tls = Some(tls.clone());
            let mut startup = TemporalStartup::prepare(opts, &OperationContext::unbounded())
                .with_service_override(simple_callback(|_| Ok(Vec::new())));
            let err = startup.admit(CancellationToken::new()).await.unwrap_err();
            std::assert_matches!(
                err,
                ConnectError::InvalidConfig(_),
                "allow_plaintext={allow_plaintext} trusted_network={trusted_network} case={label}: {err:?}"
            );
        }
    }
}

async fn build_conn_opts_for_test(
    options: &TemporalOptions,
) -> (temporalio_client::ConnectionOptions, bool) {
    options.validate().expect("options validate");
    let reader = Arc::new(Semaphore::new(1));
    let op = OperationContext::with_timeout(Duration::from_secs(5));
    let connect_budget = op.bounded(options.connect_timeout);
    let cancel = CancellationToken::new();
    let initial_api_key = load_initial_api_key(&options.auth, &reader, &connect_budget, &cancel)
        .await
        .expect("api key loads");
    let (tls_options, mtls_watch_seed) =
        build_tls_setup(options, &reader, &connect_budget, &cancel)
            .await
            .expect("tls setup succeeds");
    let meter = WorkspaceMetricsMeter::new(METRIC_PREFIX);
    let conn_opts = build_connection_options(
        options,
        &meter,
        options.connect_timeout,
        tls_options,
        initial_api_key.as_ref(),
        None,
    );
    (conn_opts, mtls_watch_seed.is_some())
}

fn https_base_options() -> TemporalOptions {
    let mut opts = base_options();
    opts.endpoint = Url::parse("https://temporal.example.invalid:7233").unwrap();
    opts.allow_plaintext = false;
    opts.allow_unauthenticated = false;
    opts
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary certificate files verify build_tls_setup and build_connection_options"
)]
async fn https_connection_options_always_enable_tls_and_configure_ca_and_mtls() {
    let dir = tempfile::tempdir().unwrap();
    let (ca_pem, _) = generate_cert_and_key("temporal-ca.test");
    let (cert_pem, key_pem) = generate_cert_and_key("temporal-client.test");
    let ca_path = dir.path().join("ca.pem");
    let cert_path = dir.path().join("client.crt");
    let key_path = dir.path().join("client.key");
    std::fs::write(&ca_path, &ca_pem).unwrap();
    std::fs::write(&cert_path, &cert_pem).unwrap();
    std::fs::write(&key_path, &key_pem).unwrap();

    let mut https_api_key = https_base_options();
    https_api_key.auth = TemporalAuth::ApiKey(Zeroizing::new("inline-secret".to_owned()));
    assert!(https_api_key.tls_active());
    let (conn_opts, has_mtls_watch) = build_conn_opts_for_test(&https_api_key).await;
    let tls = conn_opts
        .tls_options
        .expect("https with ApiKey and tls: None must populate tls_options");
    assert!(tls.server_root_ca_cert.is_none());
    assert!(tls.domain.is_none());
    assert!(tls.client_tls_options.is_none());
    assert!(tls.client_cert_resolver.is_none());
    assert_eq!(conn_opts.api_key.as_deref(), Some("inline-secret"));
    assert!(!has_mtls_watch);

    let mut https_unauth = https_base_options();
    https_unauth.allow_unauthenticated = true;
    let (conn_opts, _) = build_conn_opts_for_test(&https_unauth).await;
    assert!(conn_opts.tls_options.is_some());

    let mut https_custom_ca = https_api_key;
    https_custom_ca.tls = Some(TemporalTlsOptions {
        server_root_ca_path: Some(ca_path.clone()),
        domain: Some("temporal.internal".to_owned()),
        mtls: None,
    });
    let (conn_opts, has_mtls_watch) = build_conn_opts_for_test(&https_custom_ca).await;
    let tls = conn_opts.tls_options.expect("custom CA sets tls_options");
    assert_eq!(tls.server_root_ca_cert.as_deref(), Some(ca_pem.as_bytes()));
    assert_eq!(tls.domain.as_deref(), Some("temporal.internal"));
    assert!(tls.client_tls_options.is_none());
    assert!(tls.client_cert_resolver.is_none());
    assert!(!has_mtls_watch);

    let mut https_static_mtls = https_base_options();
    https_static_mtls.tls = Some(TemporalTlsOptions {
        server_root_ca_path: Some(ca_path),
        domain: None,
        mtls: Some(TemporalMtlsOptions {
            client_cert_path: cert_path.clone(),
            client_key_path: key_path.clone(),
            rotation: TlsRotationMode::Static,
        }),
    });
    let (conn_opts, has_mtls_watch) = build_conn_opts_for_test(&https_static_mtls).await;
    let tls = conn_opts.tls_options.expect("static mTLS sets tls_options");
    assert!(tls.server_root_ca_cert.is_some());
    let client_tls = tls
        .client_tls_options
        .expect("static mTLS sets client_tls_options");
    assert_eq!(client_tls.client_cert, cert_pem.as_bytes());
    assert_eq!(client_tls.client_private_key, key_pem.as_bytes());
    assert!(tls.client_cert_resolver.is_none());
    assert!(!has_mtls_watch);

    let mut https_dynamic_mtls = https_base_options();
    https_dynamic_mtls.dns_load_balancing = false;
    https_dynamic_mtls.tls = Some(TemporalTlsOptions {
        server_root_ca_path: None,
        domain: None,
        mtls: Some(TemporalMtlsOptions {
            client_cert_path: cert_path,
            client_key_path: key_path,
            rotation: TlsRotationMode::Dynamic,
        }),
    });
    let (conn_opts, has_mtls_watch) = build_conn_opts_for_test(&https_dynamic_mtls).await;
    let tls = conn_opts
        .tls_options
        .expect("dynamic mTLS sets tls_options");
    assert!(tls.client_tls_options.is_none());
    assert!(tls.client_cert_resolver.is_some());
    assert!(has_mtls_watch);
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary credential file verifies plaintext http:// build_connection_options"
)]
async fn http_connection_options_keep_tls_disabled_even_with_api_key_auth() {
    let dir = tempfile::tempdir().unwrap();
    let api_key_path = dir.path().join("api.key");
    std::fs::write(&api_key_path, "file-secret\n").unwrap();

    let http_plain = base_options();
    assert!(!http_plain.tls_active());
    let (conn_opts, has_mtls_watch) = build_conn_opts_for_test(&http_plain).await;
    assert!(conn_opts.tls_options.is_none());
    assert!(!has_mtls_watch);

    let mut http_api_key_file = base_options();
    http_api_key_file.auth = TemporalAuth::ApiKeyFile(api_key_path);
    assert!(!http_api_key_file.tls_active());
    let (conn_opts, _) = build_conn_opts_for_test(&http_api_key_file).await;
    assert!(conn_opts.tls_options.is_none());
    assert_eq!(conn_opts.api_key.as_deref(), Some("file-secret"));

    let mut http_trusted_api_key = base_options();
    http_trusted_api_key.allow_plaintext = false;
    http_trusted_api_key.trusted_network = true;
    http_trusted_api_key.auth = TemporalAuth::ApiKey(Zeroizing::new("net-secret".to_owned()));
    assert!(!http_trusted_api_key.tls_active());
    let (conn_opts, _) = build_conn_opts_for_test(&http_trusted_api_key).await;
    assert!(conn_opts.tls_options.is_none());
    assert_eq!(conn_opts.api_key.as_deref(), Some("net-secret"));
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary credential file verifies plaintext http:// ApiKeyFile admission"
)]
async fn inline_api_key_rejects_invalid_values_and_sends_bearer_header_on_rpcs() {
    for invalid in [
        "",
        "   ",
        " tok-secret",
        "tok-secret ",
        " tok-secret ",
        "tok-secret\n",
        "tok-secret\r",
        "tok-secret\t",
        "tok-secret\x7f",
        "tok-secret-é",
    ] {
        let mut opts = https_base_options();
        opts.auth = TemporalAuth::ApiKey(Zeroizing::new(invalid.to_owned()));
        let mut startup = TemporalStartup::prepare(opts, &OperationContext::unbounded())
            .with_service_override(simple_callback(|_| Ok(Vec::new())));
        let err = startup.admit(CancellationToken::new()).await.unwrap_err();
        std::assert_matches!(err, ConnectError::InvalidConfig(_), "{invalid:?}: {err:?}");
        let rendered = format!("{err}; {err:?}");
        assert!(!rendered.contains("tok-secret"), "{rendered}");
    }

    let captured_auth = Arc::new(Mutex::new(Vec::new()));
    let mut opts = base_options();
    opts.endpoint = Url::parse("https://temporal.example.invalid:7233").unwrap();
    opts.allow_plaintext = false;
    opts.allow_unauthenticated = false;
    opts.auth = TemporalAuth::ApiKey(Zeroizing::new("inline-bearer-token".to_owned()));
    let (startup, conn) = admit_with_callback(
        opts,
        recording_describe_callback(Arc::clone(&captured_auth)),
    )
    .await;
    assert!(startup.watcher_steps().is_none());
    conn.probe().check().await.expect("inline api key succeeds");
    assert_eq!(
        last_auth(&captured_auth).as_deref(),
        Some("Bearer inline-bearer-token")
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );

    let dir = tempfile::tempdir().unwrap();
    let api_key_path = dir.path().join("http-api.key");
    std::fs::write(&api_key_path, "http-file-token\n").unwrap();
    let http_captured = Arc::new(Mutex::new(Vec::new()));
    let mut http_opts = base_options();
    http_opts.endpoint = Url::parse("http://127.0.0.1:7233").unwrap();
    http_opts.allow_plaintext = true;
    http_opts.auth = TemporalAuth::ApiKeyFile(api_key_path);
    let (http_startup, http_conn) = admit_with_callback(
        http_opts,
        recording_describe_callback(Arc::clone(&http_captured)),
    )
    .await;
    http_conn
        .probe()
        .check()
        .await
        .expect("http api key file succeeds");
    assert_eq!(
        last_auth(&http_captured).as_deref(),
        Some("Bearer http-file-token")
    );
    assert_eq!(
        http_startup
            .close(Instant::now() + Duration::from_secs(1))
            .await,
        CloseOutcome::Complete
    );
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary credential file verifies close timeout and watcher task abort"
)]
async fn close_times_out_and_aborts_hung_credential_watcher() {
    let dir = tempfile::tempdir().unwrap();
    let api_key_path = dir.path().join("timeout-api.key");
    std::fs::write(&api_key_path, "token-timeout-v1\n").unwrap();

    let mut opts = base_options();
    opts.endpoint = Url::parse("https://temporal.example.invalid:7233").unwrap();
    opts.auth = TemporalAuth::ApiKeyFile(api_key_path);
    opts.credential_refresh_interval = Duration::from_mins(1);

    let (mut startup, conn) = admit_with_callback(
        opts,
        simple_callback(|_| Ok(DescribeNamespaceResponse::default().encode_to_vec())),
    )
    .await;

    let (drop_tx, drop_rx) = tokio::sync::oneshot::channel::<()>();
    let started = Arc::new(Notify::new());
    let started_task = Arc::clone(&started);
    let hung_task = tokio::spawn(async move {
        let _drop_guard = drop_tx;
        started_task.notify_one();
        std::future::pending::<()>().await;
    });
    started.notified().await;
    startup.replace_watcher_task(hung_task);

    let past_deadline = Instant::now()
        .checked_sub(Duration::from_millis(1))
        .unwrap_or_else(Instant::now);
    assert_eq!(startup.close(past_deadline).await, CloseOutcome::TimedOut);

    let drop_result = tokio::time::timeout(Duration::from_secs(1), drop_rx)
        .await
        .expect("timed out waiting for aborted watcher task to drop guard");
    assert!(drop_result.is_err());
    assert!(conn.probe().check().await.is_err());
}

#[cfg(feature = "worker")]
mod worker_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use futures::FutureExt as _;
    use health::Probe as _;
    use metrics_util::MetricKind;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use prost::Message as _;
    use temporalio_client::callback_based::{
        CallbackBasedGrpcService, GrpcRequest, GrpcSuccessResponse,
    };
    use temporalio_client::{Client, ClientOptions, Connection, ConnectionOptions, Url};
    use temporalio_common::protos::temporal::api::workflowservice::v1::DescribeNamespaceResponse;
    use temporalio_common::telemetry::TaskQueueLabelStrategy;
    use temporalio_common::worker::{WorkerDeploymentOptions, WorkerDeploymentVersion};
    use temporalio_macros::{activities, workflow, workflow_methods};
    use temporalio_sdk::activities::{ActivityContext, ActivityError};
    use temporalio_sdk::runtime::worker_tuner::{ResourceBasedTuner, ResourceBasedTunerOptions};
    use temporalio_sdk::{ActivityOptions, Worker, WorkerOptions, WorkflowContext, WorkflowResult};
    use tokio::sync::Notify;
    use tokio::time::Instant;
    use tokio_util::sync::CancellationToken;

    use super::{admit_with_callback, base_options, simple_callback};
    use crate::worker::{build_runtime, spawn_worker_thread};
    use crate::{CloseOutcome, METRIC_PREFIX, WorkspaceMetricsMeter};

    #[workflow]
    #[derive(Default)]
    struct SmokeWorkflow;

    #[workflow_methods]
    impl SmokeWorkflow {
        #[run]
        async fn run(ctx: &mut WorkflowContext<Self>, input: String) -> WorkflowResult<String> {
            let output = ctx
                .execute_activity(
                    SmokeActivities::echo,
                    input,
                    ActivityOptions::start_to_close_timeout(Duration::from_secs(10)),
                )
                .await?;
            Ok(output)
        }
    }

    struct SmokeActivities;

    #[activities]
    impl SmokeActivities {
        #[activity]
        async fn echo(_ctx: ActivityContext, input: String) -> Result<String, ActivityError> {
            tokio::task::yield_now().await;
            Ok(input)
        }
    }

    fn assert_send<T: Send>() {}

    fn callback_grpc_service(
        shutdown: CancellationToken,
        poll_started: Option<Arc<Notify>>,
    ) -> CallbackBasedGrpcService {
        CallbackBasedGrpcService {
            callback: Arc::new(move |mut req: GrpcRequest| {
                let shutdown = shutdown.clone();
                let poll_started = poll_started.clone();
                async move {
                    if req.rpc.starts_with("Poll") {
                        if let Some(notify) = poll_started {
                            notify.notify_one();
                        }
                        shutdown.cancelled().await;
                    }
                    req.headers.clear();
                    Ok(GrpcSuccessResponse {
                        headers: req.headers,
                        proto: Vec::new(),
                    })
                }
                .boxed()
            }),
        }
    }

    #[tokio::test]
    async fn worker_connection_bridge_exposes_client_meter_converter_and_failure_probe() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_cb = Arc::clone(&calls);
        let callback = simple_callback(move |req: GrpcRequest| {
            calls_cb.fetch_add(1, Ordering::SeqCst);
            match req.rpc.as_str() {
                "DescribeNamespace" => Ok(DescribeNamespaceResponse::default().encode_to_vec()),
                _ => Err(tonic::Status::unimplemented("unexpected")),
            }
        });
        let (startup, conn) = admit_with_callback(base_options(), callback).await;
        assert_eq!(conn.namespace(), "default");
        assert_eq!(conn.raw_client().options().namespace, "default");
        assert!(conn.meter().telemetry_options().metrics.is_some());
        let _ = conn.data_converter().payload_converter();

        let failed = Arc::new(AtomicBool::new(false));
        let probe = conn.probe_with_failure(Arc::clone(&failed));
        assert!(probe.check().await.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        failed.store(true, Ordering::Release);
        assert!(probe.check().await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        failed.store(false, Ordering::Release);
        conn.mark_draining();
        assert!(probe.check().await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        assert_eq!(
            startup.close(Instant::now() + Duration::from_secs(1)).await,
            CloseOutcome::Complete
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_is_send_and_runs_on_os_thread_with_handle_block_on_and_resource_tuner() {
        assert_send::<Worker>();

        let shutdown = CancellationToken::new();
        let poll_started = Arc::new(Notify::new());
        let callback = callback_grpc_service(shutdown.clone(), Some(Arc::clone(&poll_started)));

        let meter = WorkspaceMetricsMeter::new(METRIC_PREFIX);
        let runtime = build_runtime(&meter).expect("CoreRuntime attaches to current Tokio pool");

        let conn_opts = ConnectionOptions::new(
            Url::parse("http://127.0.0.1:7233").expect("valid loopback url"),
        )
        .dns_load_balancing(None)
        .service_override(callback)
        .metrics_meter(meter.temporal_meter())
        .build();
        let connection = Connection::connect(conn_opts)
            .await
            .expect("callback connection succeeds");
        let client = Client::new(connection, ClientOptions::new("default").build())
            .expect("client constructs");

        let tuner = ResourceBasedTuner::builder()
            .tuner_options(
                ResourceBasedTunerOptions::builder()
                    .target_memory_usage(0.75)
                    .target_cpu_usage(0.80)
                    .build(),
            )
            .build();
        let deployment = WorkerDeploymentOptions::new(
            WorkerDeploymentVersion::builder()
                .deployment_name("service-temporal-worker")
                .build_id("0.1.0-test")
                .build(),
        )
        .build();
        let worker_opts = WorkerOptions::new("orders")
            .deployment_options(deployment)
            .tuner(tuner)
            .graceful_shutdown_period(Duration::from_secs(1))
            .register_workflow::<SmokeWorkflow>()
            .expect("workflow registers")
            .register_activities(SmokeActivities)
            .build();

        let worker = Worker::new(&runtime, client, worker_opts).expect("worker initializes");
        let join = spawn_worker_thread(
            "temporal-wf-0".to_owned(),
            tokio::runtime::Handle::current(),
            worker,
            shutdown.clone(),
        )
        .expect("worker OS thread spawns");

        tokio::time::timeout(Duration::from_secs(5), poll_started.notified())
            .await
            .expect("worker OS thread did not enter gRPC polling before timeout");
        shutdown.cancel();

        let result = tokio::task::spawn_blocking(move || join.join())
            .await
            .expect("join task completes")
            .expect("worker OS thread does not panic");
        assert!(
            result.is_ok(),
            "worker.run_until failed under Handle::block_on: {result:?}"
        );
    }

    #[tokio::test]
    async fn core_telemetry_options_preserve_single_prefix_through_worker_init() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();

        let meter = WorkspaceMetricsMeter::new(METRIC_PREFIX);
        let opts = meter.telemetry_options();
        assert_eq!(opts.metric_prefix, "");
        assert!(!opts.attach_service_name);
        assert!(opts.metrics.is_some());
        std::assert_matches!(
            opts.task_queue_label_strategy,
            TaskQueueLabelStrategy::UseNormal
        );
        std::assert_matches!(
            meter.temporal_meter().get_task_queue_label_strategy(),
            TaskQueueLabelStrategy::UseNormal
        );

        let runtime = build_runtime(&meter).expect("CoreRuntime initializes with CoreMeter");
        let shutdown = CancellationToken::new();
        let callback = callback_grpc_service(shutdown.clone(), None);

        let conn_opts = ConnectionOptions::new(
            Url::parse("http://127.0.0.1:7233").expect("valid loopback url"),
        )
        .dns_load_balancing(None)
        .service_override(callback)
        .build();

        let connection = Connection::connect(conn_opts)
            .await
            .expect("callback connection succeeds");
        let client = Client::new(connection, ClientOptions::new("default").build())
            .expect("client constructs");
        let deployment = WorkerDeploymentOptions::from_build_id("0.1.0-test".to_owned());
        let worker_opts = WorkerOptions::new("orders")
            .deployment_options(deployment)
            .graceful_shutdown_period(Duration::from_secs(1))
            .register_workflow::<SmokeWorkflow>()
            .expect("workflow registers")
            .register_activities(SmokeActivities)
            .build();
        let worker = ::metrics::with_local_recorder(&recorder, || {
            Worker::new(&runtime, client, worker_opts)
        })
        .expect("worker initializes");

        shutdown.cancel();
        let join = spawn_worker_thread(
            "temporal-wf-metrics".to_owned(),
            tokio::runtime::Handle::current(),
            worker,
            shutdown,
        )
        .expect("worker OS thread spawns");
        let result = tokio::task::spawn_blocking(move || join.join())
            .await
            .expect("join task completes")
            .expect("worker OS thread does not panic");
        assert!(result.is_ok(), "worker shutdown failed: {result:?}");

        let snapshot = snapshotter.snapshot().into_vec();
        assert!(
            !snapshot.is_empty(),
            "expected CoreMeter to record Core worker initialization metrics"
        );
        let mut saw_worker_start = false;
        for (ckey, _, _, value) in snapshot {
            let name = ckey.key().name();
            assert!(
                name.starts_with(METRIC_PREFIX),
                "missing temporal_ prefix: {name}"
            );
            assert!(
                !name.starts_with("temporal_temporal_"),
                "double temporal_ prefix: {name}"
            );
            let labels: Vec<(&str, &str)> = ckey
                .key()
                .labels()
                .map(|label| (label.key(), label.value()))
                .collect();
            assert!(
                !labels.iter().any(|(key, _)| *key == "service_name"),
                "unexpected service_name label on {name}: {labels:?}"
            );
            if (ckey.kind(), name) == (MetricKind::Counter, "temporal_worker_start") {
                assert_eq!(value, DebugValue::Counter(1));
                assert_eq!(
                    labels,
                    vec![("namespace", "default"), ("task_queue", "orders")]
                );
                saw_worker_start = true;
            }
        }
        assert!(
            saw_worker_start,
            "expected Core worker initialization to record temporal_worker_start"
        );
    }
}
