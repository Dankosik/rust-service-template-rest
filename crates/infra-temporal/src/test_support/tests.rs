use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures_util::future::BoxFuture;
use opentelemetry::global;
use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use temporalio_common::ActivityDefinition;
use temporalio_common::data_converters::{
    GenericPayloadConverter as _, PayloadCodec, PayloadConversionError, SerializationContext,
    SerializationContextData, WorkflowSerializationContext,
};
use temporalio_common::protos::temporal::api::common::v1::{
    Header, Payload, Payloads, WorkflowType,
};
use temporalio_common::protos::temporal::api::enums::v1::{EventType, TaskQueueKind};
use temporalio_common::protos::temporal::api::history::v1::{
    History, HistoryEvent, TimerFiredEventAttributes, TimerStartedEventAttributes,
    WorkflowExecutionCompletedEventAttributes, WorkflowExecutionStartedEventAttributes,
    WorkflowTaskCompletedEventAttributes, WorkflowTaskScheduledEventAttributes,
    WorkflowTaskStartedEventAttributes, history_event,
};
use temporalio_common::protos::temporal::api::taskqueue::v1::TaskQueue;
use temporalio_common::protos::{ENCODING_PAYLOAD_KEY, JSON_ENCODING_VAL};
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{ActivityOptions, WorkflowContext, WorkflowResult};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt as _;

use super::{Preempted, ReplayError, ReplaySuite, run_with_preemption};
use crate::payload::build_data_converter;
use crate::trace::TRACER_DATA_HEADER;

#[workflow]
#[derive(Default)]
struct DeterministicTimerWorkflow;

#[workflow_methods]
impl DeterministicTimerWorkflow {
    #[run]
    async fn run(ctx: &mut WorkflowContext<Self>, input: String) -> WorkflowResult<String> {
        ctx.timer(Duration::from_secs(1)).await;
        Ok(format!("done:{input}"))
    }
}

#[workflow]
#[derive(Default)]
struct CommandMismatchWorkflow;

#[workflow_methods]
impl CommandMismatchWorkflow {
    #[run(name = "DeterministicTimerWorkflow")]
    async fn run(ctx: &mut WorkflowContext<Self>, input: String) -> WorkflowResult<String> {
        let _ = ctx
            .execute_activity(
                PlaceholderActivity,
                input.clone(),
                ActivityOptions::start_to_close_timeout(Duration::from_secs(5)),
            )
            .await;
        Ok(input)
    }
}

struct PlaceholderActivity;

impl ActivityDefinition for PlaceholderActivity {
    type Input = String;
    type Output = String;

    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "ActivityDefinition trait signature returns &str"
    )]
    fn name(&self) -> &str {
        "PlaceholderActivity"
    }
}

#[workflow]
#[derive(Default)]
struct TokioSleepWorkflow;

#[workflow_methods]
impl TokioSleepWorkflow {
    #[run(name = "DeterministicTimerWorkflow")]
    async fn run(_ctx: &mut WorkflowContext<Self>, input: String) -> WorkflowResult<String> {
        tokio::time::sleep(Duration::from_millis(1)).await;
        Ok(input)
    }
}

struct PrefixCodec;

impl PayloadCodec for PrefixCodec {
    fn encode(
        &self,
        _context: &SerializationContextData,
        payloads: Vec<Payload>,
    ) -> BoxFuture<'static, Result<Vec<Payload>, PayloadConversionError>> {
        Box::pin(async move {
            Ok(payloads
                .into_iter()
                .map(|mut payload| {
                    let mut encoded = vec![b'X'];
                    encoded.extend_from_slice(&payload.data);
                    payload.data = encoded;
                    payload
                })
                .collect())
        })
    }

    fn decode(
        &self,
        _context: &SerializationContextData,
        payloads: Vec<Payload>,
    ) -> BoxFuture<'static, Result<Vec<Payload>, PayloadConversionError>> {
        Box::pin(async move {
            payloads
                .into_iter()
                .map(|mut payload| {
                    if payload.data.first() == Some(&b'X') {
                        payload.data.remove(0);
                        Ok(payload)
                    } else {
                        Err(PayloadConversionError::EncodingError(
                            "missing prefix".into(),
                        ))
                    }
                })
                .collect()
        })
    }
}

fn json_payload(value: &str, encode_prefix: bool) -> Payload {
    let mut data = serde_json::to_vec(value).unwrap();
    if encode_prefix {
        let mut prefixed = vec![b'X'];
        prefixed.extend_from_slice(&data);
        data = prefixed;
    }
    Payload {
        metadata: HashMap::from([(
            ENCODING_PAYLOAD_KEY.to_owned(),
            JSON_ENCODING_VAL.as_bytes().to_vec(),
        )]),
        data,
        external_payloads: Vec::new(),
    }
}

fn history_event(
    event_id: i64,
    event_type: EventType,
    attributes: history_event::Attributes,
) -> HistoryEvent {
    let base_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let offset = u64::try_from(event_id).unwrap_or(0);
    HistoryEvent {
        event_id,
        event_time: Some((base_time + Duration::from_secs(offset)).into()),
        event_type: event_type as i32,
        attributes: Some(attributes),
        ..Default::default()
    }
}

fn wft_events(scheduled_id: i64) -> [HistoryEvent; 3] {
    let started_id = scheduled_id + 1;
    let completed_id = scheduled_id + 2;
    [
        history_event(
            scheduled_id,
            EventType::WorkflowTaskScheduled,
            history_event::Attributes::WorkflowTaskScheduledEventAttributes(
                WorkflowTaskScheduledEventAttributes::default(),
            ),
        ),
        history_event(
            started_id,
            EventType::WorkflowTaskStarted,
            history_event::Attributes::WorkflowTaskStartedEventAttributes(
                WorkflowTaskStartedEventAttributes {
                    scheduled_event_id: scheduled_id,
                    ..Default::default()
                },
            ),
        ),
        history_event(
            completed_id,
            EventType::WorkflowTaskCompleted,
            history_event::Attributes::WorkflowTaskCompletedEventAttributes(
                WorkflowTaskCompletedEventAttributes {
                    scheduled_event_id: scheduled_id,
                    started_event_id: started_id,
                    ..Default::default()
                },
            ),
        ),
    ]
}

fn timer_workflow_history_json(
    workflow_type: &str,
    input: &str,
    output: &str,
    encode_prefix: bool,
    header: Option<Header>,
) -> Vec<u8> {
    let mut events = vec![history_event(
        1,
        EventType::WorkflowExecutionStarted,
        history_event::Attributes::WorkflowExecutionStartedEventAttributes(
            WorkflowExecutionStartedEventAttributes {
                workflow_type: Some(WorkflowType {
                    name: workflow_type.to_owned(),
                }),
                task_queue: Some(TaskQueue {
                    name: "replay-queue".to_owned(),
                    kind: TaskQueueKind::Normal as i32,
                    normal_name: String::new(),
                }),
                original_execution_run_id: "replay-run-1".to_owned(),
                input: Some(Payloads {
                    payloads: vec![json_payload(input, encode_prefix)],
                }),
                header,
                workflow_id: "replay-wf-1".to_owned(),
                ..Default::default()
            },
        ),
    )];
    events.extend(wft_events(2));
    events.push(history_event(
        5,
        EventType::TimerStarted,
        history_event::Attributes::TimerStartedEventAttributes(TimerStartedEventAttributes {
            timer_id: "1".to_owned(),
            workflow_task_completed_event_id: 4,
            ..Default::default()
        }),
    ));
    events.push(history_event(
        6,
        EventType::TimerFired,
        history_event::Attributes::TimerFiredEventAttributes(TimerFiredEventAttributes {
            timer_id: "1".to_owned(),
            started_event_id: 5,
        }),
    ));
    events.extend(wft_events(7));
    events.push(history_event(
        10,
        EventType::WorkflowExecutionCompleted,
        history_event::Attributes::WorkflowExecutionCompletedEventAttributes(
            WorkflowExecutionCompletedEventAttributes {
                result: Some(Payloads {
                    payloads: vec![json_payload(output, encode_prefix)],
                }),
                workflow_task_completed_event_id: 9,
                ..Default::default()
            },
        ),
    ));
    serde_json::to_vec(&History { events }).unwrap()
}

#[test]
fn replay_suite_passes_deterministic_workflow_and_suppresses_replay_spans() {
    global::set_text_map_propagator(TextMapCompositePropagator::new(vec![
        Box::new(TraceContextPropagator::new()),
        Box::new(BaggagePropagator::new()),
    ]));
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("replay-test")));

    let dc = build_data_converter(64 * 1024, None);
    let converter = dc.payload_converter();
    let ctx_data = SerializationContextData::Workflow(WorkflowSerializationContext::new());
    let header_payload = converter
        .to_payload(
            &SerializationContext::new(&ctx_data, converter),
            &HashMap::from([(
                "traceparent".to_owned(),
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_owned(),
            )]),
        )
        .unwrap();
    let header = Some(Header {
        fields: HashMap::from([(TRACER_DATA_HEADER.to_owned(), header_payload)]),
    });
    let history = timer_workflow_history_json(
        "DeterministicTimerWorkflow",
        "hello",
        "done:hello",
        false,
        header,
    );

    let mut suite = ReplaySuite::new(64 * 1024, None);
    suite
        .register_workflow::<DeterministicTimerWorkflow>()
        .unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = tracing::subscriber::with_default(subscriber, || {
        runtime.block_on(suite.replay_json(&history))
    });
    assert_eq!(result, Ok(()));
    let _ = provider.force_flush();
    let spans = exporter.get_finished_spans().unwrap();
    let temporal_spans: Vec<_> = spans
        .iter()
        .filter(|span| span.name.starts_with("temporal_"))
        .collect();
    assert!(
        temporal_spans.is_empty(),
        "replay must not export duplicate interceptor workflow spans: {temporal_spans:?}"
    );
    assert!(
        !spans.iter().any(|span| {
            span.links.iter().any(|link| {
                link.span_context.trace_id().to_string() == "4bf92f3577b34da6a3ce929d0e0e4736"
            }) || span.span_context.trace_id().to_string() == "4bf92f3577b34da6a3ce929d0e0e4736"
        }),
        "replay must not attach or export caller trace context spans: {spans:?}"
    );
}

#[tokio::test]
async fn replay_suite_supports_custom_payload_codec() {
    let history = timer_workflow_history_json(
        "DeterministicTimerWorkflow",
        "encoded",
        "done:encoded",
        true,
        None,
    );
    let mut suite = ReplaySuite::new(64 * 1024, Some(Arc::new(PrefixCodec)));
    suite
        .register_workflow::<DeterministicTimerWorkflow>()
        .unwrap();
    assert_eq!(suite.replay_json(&history).await, Ok(()));
}

#[tokio::test]
async fn replay_suite_detects_command_order_nondeterminism() {
    let history = timer_workflow_history_json(
        "DeterministicTimerWorkflow",
        "hello",
        "done:hello",
        false,
        None,
    );
    let mut suite = ReplaySuite::new(64 * 1024, None);
    suite
        .register_workflow::<CommandMismatchWorkflow>()
        .unwrap();
    let err = suite.replay_json(&history).await.unwrap_err();
    std::assert_matches!(err, ReplayError::Nondeterminism(_), "{err:?}");
}

#[tokio::test]
async fn replay_suite_detects_nondeterministic_tokio_sleep_future() {
    let history = timer_workflow_history_json(
        "DeterministicTimerWorkflow",
        "hello",
        "done:hello",
        false,
        None,
    );
    let mut suite = ReplaySuite::new(64 * 1024, None);
    suite.register_workflow::<TokioSleepWorkflow>().unwrap();
    let err = suite.replay_json(&history).await.unwrap_err();
    std::assert_matches!(err, ReplayError::Nondeterminism(_), "{err:?}");
}

#[tokio::test]
async fn replay_suite_rejects_unregistered_workflows_and_invalid_json() {
    let empty_suite = ReplaySuite::new(64 * 1024, None);
    assert_eq!(
        empty_suite.replay_json(b"{}").await,
        Err(ReplayError::Registration)
    );

    let mut suite = ReplaySuite::new(64 * 1024, None);
    suite
        .register_workflow::<DeterministicTimerWorkflow>()
        .unwrap();
    assert_eq!(
        suite.replay_json(b"{not-valid-json").await,
        Err(ReplayError::InvalidHistoryJson)
    );
    assert_eq!(
        suite.replay_json(b"{}").await,
        Err(ReplayError::InvalidHistoryJson)
    );
    let missing_queue_history = serde_json::to_vec(&History {
        events: vec![HistoryEvent {
            event_id: 1,
            event_type: EventType::WorkflowExecutionStarted as i32,
            attributes: Some(
                history_event::Attributes::WorkflowExecutionStartedEventAttributes(
                    WorkflowExecutionStartedEventAttributes {
                        workflow_type: Some(WorkflowType {
                            name: "DeterministicTimerWorkflow".to_owned(),
                        }),
                        ..Default::default()
                    },
                ),
            ),
            ..Default::default()
        }],
    })
    .unwrap();
    assert_eq!(
        suite.replay_json(&missing_queue_history).await,
        Err(ReplayError::InvalidHistoryJson)
    );
}

#[tokio::test]
async fn reexported_run_with_preemption_works_under_test_support() {
    let token = CancellationToken::new();
    assert_eq!(run_with_preemption(&token, async { 7 }).await, Ok(7));
    token.cancel();
    assert_eq!(
        run_with_preemption(&token, async { 7 }).await,
        Err(Preempted)
    );
}
