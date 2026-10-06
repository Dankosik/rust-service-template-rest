#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration tests make failures and broker setup explicit"
)]

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_nats::jetstream::{self, consumer, stream};
use bytes::Bytes;
use domain_events::{Event, EventPayload};
use futures_util::StreamExt;
use health::Probe as _;
use infra_messaging::{
    ConsumerError, ConsumerOptions, HandlerError, Messaging, MessagingOptions, PublishError,
    Registry, Route,
};
use tokio::sync::{Notify, oneshot};
use tokio::time::{Instant, timeout};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[path = "support/relay.rs"]
mod relay;
use relay::{AckDroppingRelay, is_stream_publish_ack, relay_target};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct ExampleEvent {
    value: String,
}

impl EventPayload for ExampleEvent {
    const EVENT_TYPE: &'static str = "test.example.created";
    const SCHEMA_VERSION: u16 = 1;
}

#[derive(Clone, serde::Serialize)]
struct UnhandledEvent;

impl EventPayload for UnhandledEvent {
    const EVENT_TYPE: &'static str = "test.example.unhandled";
    const SCHEMA_VERSION: u16 = 1;
}

struct Fixture {
    jetstream: jetstream::Context,
    stream: String,
    subject: String,
    durable: String,
    dlq_stream: String,
    dlq_subject: String,
}

impl Fixture {
    async fn create(with_consumer: bool) -> Self {
        Self::create_with_source_limit(with_consumer, 10).await
    }

    async fn create_with_source_limit(with_consumer: bool, max_messages: i64) -> Self {
        Self::create_with_limits(with_consumer, max_messages, 10).await
    }

    async fn create_with_limits(
        with_consumer: bool,
        source_max_messages: i64,
        dlq_max_messages: i64,
    ) -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let suffix = format!("{}_{}", std::process::id(), id);
        let stream = format!("TEST_JS_{suffix}");
        let subject = format!("test.jetstream.{suffix}");
        let durable = format!("test_js_{suffix}");
        let dlq_stream = format!("TEST_DLQ_{suffix}");
        let dlq_subject = format!("test.jetstream.{suffix}.dlq");
        let client = async_nats::connect(nats_url())
            .await
            .expect("NATS_URL must point to the JetStream broker selected for this suite");
        let jetstream = jetstream::new(client);
        let created = jetstream
            .create_stream(stream::Config {
                name: stream.clone(),
                subjects: vec![subject.clone()],
                max_messages: source_max_messages,
                max_message_size: 1024 + 8 * 1024,
                discard: stream::DiscardPolicy::New,
                ..Default::default()
            })
            .await
            .expect("test fixture stream must be created by the NATS test administrator");
        jetstream
            .create_stream(stream::Config {
                name: dlq_stream.clone(),
                subjects: vec![dlq_subject.clone()],
                max_messages: dlq_max_messages,
                // Leave room for original subject and transfer identity headers.
                max_message_size: 16 * 1024,
                discard: stream::DiscardPolicy::New,
                ..Default::default()
            })
            .await
            .expect("test fixture DLQ stream must be created by the NATS test administrator");

        if with_consumer {
            created
                .create_consumer(consumer::pull::Config {
                    name: Some(durable.clone()),
                    durable_name: Some(durable.clone()),
                    filter_subject: subject.clone(),
                    ack_wait: Duration::from_secs(41),
                    max_deliver: -1,
                    max_ack_pending: 1,
                    ..Default::default()
                })
                .await
                .expect(
                    "test fixture durable consumer must be created by the NATS test administrator",
                );
        }

        Self {
            jetstream,
            stream,
            subject,
            durable,
            dlq_stream,
            dlq_subject,
        }
    }

    async fn cleanup(self) {
        self.jetstream
            .delete_stream(&self.stream)
            .await
            .expect("test fixture stream must be removable");
        self.jetstream
            .delete_stream(&self.dlq_stream)
            .await
            .expect("test fixture DLQ stream must be removable");
    }

    async fn replace_stream(&self, config: stream::Config) {
        self.jetstream
            .delete_stream(&config.name)
            .await
            .expect("fixture stream is replaceable");
        self.jetstream
            .create_stream(config)
            .await
            .expect("replacement stream is valid");
    }
}

fn nats_url() -> String {
    std::env::var("NATS_URL")
        .expect("NATS_URL is required; run this suite through test-integration-messaging.sh")
}

/// Disconnects one real broker connection and holds replacement connections
/// until the test explicitly restores the network path.
struct OutageRelay {
    url: String,
    pause: CancellationToken,
    resume: CancellationToken,
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl OutageRelay {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("nats://{}", listener.local_addr().unwrap());
        let target = relay_target(&nats_url());
        let pause = CancellationToken::new();
        let resume = CancellationToken::new();
        let stop = CancellationToken::new();
        let (paused, restored, stopped) = (pause.clone(), resume.clone(), stop.clone());
        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    () = stopped.cancelled() => break,
                    accepted = listener.accept() => accepted,
                };
                let (mut client, _) = accepted.unwrap();
                if paused.is_cancelled() {
                    tokio::select! {
                        () = stopped.cancelled() => break,
                        () = restored.cancelled() => {},
                    }
                }
                let mut broker = TcpStream::connect(&target).await.unwrap();
                tokio::select! {
                    () = stopped.cancelled() => break,
                    () = paused.cancelled(), if !restored.is_cancelled() => {},
                    _ = tokio::io::copy_bidirectional(&mut client, &mut broker) => {},
                }
            }
        });
        Self {
            url,
            pause,
            resume,
            stop,
            task,
        }
    }

    async fn finish(self) {
        self.stop.cancel();
        timeout(Duration::from_secs(3), self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn event(id: &str) -> Event<ExampleEvent> {
    event_with_value(id, "payload")
}

fn event_with_value(id: &str, value: &str) -> Event<ExampleEvent> {
    Event {
        id: id.to_owned(),
        occurred_at: time::UtcDateTime::from_unix_timestamp(1_700_000_000)
            .expect("fixed event timestamp is valid"),
        payload: ExampleEvent {
            value: value.to_owned(),
        },
    }
}

fn options(
    fixture: &Fixture,
    consumer: Option<ConsumerOptions>,
    max_payload_bytes: usize,
) -> MessagingOptions {
    options_with_servers(fixture, vec![nats_url()], consumer, max_payload_bytes)
}

fn options_with_servers(
    fixture: &Fixture,
    servers: Vec<String>,
    consumer: Option<ConsumerOptions>,
    max_payload_bytes: usize,
) -> MessagingOptions {
    MessagingOptions {
        connection_name: "infra-messaging-tests".to_owned(),
        servers,
        credentials: None,
        credentials_file: None,
        root_ca_path: None,
        allow_plaintext: true,
        source_stream: fixture.stream.clone(),
        dlq_stream: Some(fixture.dlq_stream.clone()),
        max_payload_bytes,
        consumer,
    }
}

fn consumer_options(fixture: &Fixture) -> ConsumerOptions {
    ConsumerOptions {
        durable_name: fixture.durable.clone(),
        filter_subject: fixture.subject.clone(),
        dlq_subject: fixture.dlq_subject.clone(),
        concurrency: 1,
    }
}

fn registry(fixture: &Fixture) -> Registry {
    Registry::new([Route::new::<ExampleEvent>(fixture.subject.clone())])
        .expect("fixture route is valid")
}

async fn close(messaging: Messaging) {
    let cancel = CancellationToken::new();
    let outcome = messaging.close(deadline(), &cancel).await;
    assert_eq!(outcome, infra_messaging::CloseOutcome::Complete);
}

async fn wait_for_source_ack(fixture: &Fixture) {
    wait_for_source_ack_at_least(fixture, 1).await;
}

async fn wait_for_source_ack_at_least(fixture: &Fixture, stream_sequence: u64) {
    wait_for_source_ack_within(fixture, stream_sequence, Duration::from_secs(3)).await;
}

async fn wait_for_source_ack_within(fixture: &Fixture, stream_sequence: u64, budget: Duration) {
    timeout(budget, async {
        let mut cadence = tokio::time::interval(Duration::from_millis(10));
        loop {
            let stream = fixture
                .jetstream
                .get_stream(&fixture.stream)
                .await
                .expect("fixture stream remains available");
            let durable: consumer::PullConsumer = stream
                .get_consumer(&fixture.durable)
                .await
                .expect("fixture durable remains available");
            let info = durable
                .get_info()
                .await
                .expect("fixture consumer state is observable");
            if info.ack_floor.stream_sequence >= stream_sequence && info.num_ack_pending == 0 {
                return;
            }
            cadence.tick().await;
        }
    })
    .await
    .expect("successful handler must receive a confirmed source acknowledgment");
}

async fn wait_for_source_unacked(fixture: &Fixture) {
    timeout(Duration::from_secs(3), async {
        let mut cadence = tokio::time::interval(Duration::from_millis(10));
        loop {
            let stream = fixture
                .jetstream
                .get_stream(&fixture.stream)
                .await
                .expect("fixture stream remains available");
            let durable: consumer::PullConsumer = stream
                .get_consumer(&fixture.durable)
                .await
                .expect("fixture durable remains available");
            if durable
                .get_info()
                .await
                .expect("fixture consumer state is observable")
                .num_ack_pending
                >= 1
            {
                return;
            }
            cadence.tick().await;
        }
    })
    .await
    .expect("terminal source delivery must remain unacknowledged");
}

async fn wait_for_dead_letter(fixture: &Fixture) -> async_nats::jetstream::message::StreamMessage {
    wait_for_dead_letter_after(fixture, 0).await
}

async fn wait_for_dead_letter_after(
    fixture: &Fixture,
    after_sequence: u64,
) -> async_nats::jetstream::message::StreamMessage {
    timeout(Duration::from_secs(3), async {
        let mut cadence = tokio::time::interval(Duration::from_millis(10));
        loop {
            let stream = fixture
                .jetstream
                .get_stream(&fixture.dlq_stream)
                .await
                .expect("fixture DLQ stream remains available");
            if let Ok(record) = stream
                .get_last_raw_message_by_subject(&fixture.dlq_subject)
                .await
                && record.sequence > after_sequence
            {
                return record;
            }
            cadence.tick().await;
        }
    })
    .await
    .expect("terminal source delivery must be transferred to the DLQ")
}

async fn redeliver_until_attempt(fixture: &Fixture, target_attempt: i64) {
    let stream = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .expect("fixture stream remains available");
    let consumer: consumer::PullConsumer = stream
        .get_consumer(&fixture.durable)
        .await
        .expect("fixture durable remains available");
    for attempt in 1..target_attempt {
        let mut batch = consumer
            .fetch()
            .max_messages(1)
            .expires(Duration::from_secs(1))
            .messages()
            .await
            .expect("fixture source delivery request is admitted");
        let message = timeout(Duration::from_secs(2), batch.next())
            .await
            .expect("fixture source message arrives for controlled redelivery")
            .expect("fixture source batch contains a message")
            .expect("fixture source delivery is valid");
        assert_eq!(
            message
                .info()
                .expect("fixture delivery has JetStream metadata")
                .delivered,
            attempt
        );
        message
            .ack_with(async_nats::jetstream::AckKind::Nak(None))
            .await
            .expect("fixture source NAK requests the next controlled delivery");
    }
}

fn assert_dead_letter(
    record: &async_nats::jetstream::message::StreamMessage,
    fixture: &Fixture,
    reason: &str,
    expected_payload: &[u8],
) {
    assert_eq!(record.subject.as_ref(), fixture.dlq_subject);
    assert_eq!(record.payload.as_ref(), expected_payload);
    assert_eq!(
        record
            .headers
            .get("Original-Subject")
            .map(ToString::to_string),
        Some(fixture.subject.clone())
    );
    assert_eq!(
        record
            .headers
            .get("Dead-Letter-Reason")
            .map(ToString::to_string),
        Some(reason.to_owned())
    );
}

#[tokio::test]
async fn cancelled_topology_admission_retains_the_native_client_until_closed() {
    let fixture = Fixture::create(false).await;
    let topology_reached = Arc::new(Notify::new());
    let reached = Arc::clone(&topology_reached);
    let relay = AckDroppingRelay::start_filtering(move |payload| {
        let topology = std::str::from_utf8(payload)
            .is_ok_and(|body| body.contains("io.nats.jetstream.api.v1.stream_info_response"));
        if topology {
            reached.notify_one();
        }
        topology
    })
    .await;
    let mut startup = Messaging::prepare(
        options_with_servers(&fixture, vec![relay.url.clone()], None, 1024),
        deadline(),
        CancellationToken::new(),
    )
    .unwrap();
    let mut admission = Box::pin(startup.admit());
    tokio::select! {
        result = &mut admission => panic!("topology admission completed with its reply withheld: {result:?}"),
        reached = timeout(Duration::from_secs(3), topology_reached.notified()) => {
            reached.expect("the real broker must answer the topology request");
        }
    }
    drop(admission);
    assert_eq!(
        startup.close(deadline(), &CancellationToken::new()).await,
        infra_messaging::CloseOutcome::Complete,
        "the retained client must drain and deliver its native Closed notification"
    );
    relay.join().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn publication_requires_expected_stream_ack_deduplicates_and_rejects_definite_refusal() {
    let fixture = Fixture::create_with_source_limit(false, 1).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, None, 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let original = event("event-publication");
    let prepared = registry(&fixture)
        .prepare(&original, 1024)
        .expect("registered event is prepared once");

    let first = messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("first publication receives a JetStream acknowledgment");
    let duplicate = messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("retrying the same immutable publication identity is acknowledged");

    assert_eq!(first.stream, fixture.stream);
    assert!(!first.duplicate);
    assert_eq!(duplicate.stream, fixture.stream);
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.sequence, first.sequence);

    let rejected = registry(&fixture)
        .prepare(&event("event-refused"), 1024)
        .expect("second registered event is prepared");
    let result = messaging
        .producer()
        .publish(&rejected, deadline(), &cancel)
        .await;
    assert!(matches!(result, Err(PublishError::Rejected)));

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn a_prepared_event_obeys_the_receiving_resources_payload_limit() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let prepared = registry(&fixture)
        .prepare(&event("event-receiving-limit"), 1024)
        .expect("event fits the preparing resource");
    let receiving_limit = prepared.payload().len() - 1;
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, None, receiving_limit),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("smaller receiving resource is admitted");
    assert!(matches!(
        messaging
            .producer()
            .publish(&prepared, deadline(), &cancel)
            .await,
        Err(PublishError::Rejected)
    ));
    let mut source = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    assert_eq!(source.info().await.unwrap().state.messages, 0);
    close(messaging).await;

    let messaging = Box::pin(Messaging::connect(
        options(&fixture, None, receiving_limit + 1),
        deadline(),
        cancel.clone(),
    ))
    .await
    .unwrap();
    messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("the exact payload boundary is admitted");
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn cancelled_publications_hold_the_shared_window_until_native_cleanup() {
    let fixture = Fixture::create(false).await;
    let client = async_nats::connect(nats_url()).await.unwrap();
    let max_payload_bytes = client.server_info().max_payload.min(64 * 1024 * 1024) - 8192;
    client.drain().await.unwrap();
    let capacity = (64 * 1024 * 1024) / (max_payload_bytes + 8192);
    let observed = Arc::new(AtomicUsize::new(0));
    let dispatched = Arc::new(Notify::new());
    let (relay_observed, relay_dispatched) = (observed.clone(), dispatched.clone());
    let stream = fixture.stream.clone();
    let relay = AckDroppingRelay::start_filtering(move |payload| {
        if is_stream_publish_ack(payload, &stream)
            && relay_observed.load(Ordering::SeqCst) < capacity
        {
            if relay_observed.fetch_add(1, Ordering::SeqCst) + 1 == capacity {
                relay_dispatched.notify_one();
            }
            true
        } else {
            false
        }
    })
    .await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options_with_servers(&fixture, vec![relay.url.clone()], None, max_payload_bytes),
        deadline(),
        cancel.clone(),
    ))
    .await
    .unwrap();
    let prepared = registry(&fixture)
        .prepare(&event("event-window"), 1024)
        .unwrap();
    let mut publications = tokio::task::JoinSet::new();
    for _ in 0..capacity {
        let (producer, prepared, cancel) = (messaging.producer(), prepared.clone(), cancel.clone());
        publications.spawn(async move { producer.publish(&prepared, deadline(), &cancel).await });
    }
    timeout(Duration::from_secs(3), dispatched.notified())
        .await
        .expect("every admitted publication reaches the broker");
    cancel.cancel();
    while let Some(result) = publications.join_next().await {
        assert!(matches!(result.unwrap(), Err(PublishError::Ambiguous)));
    }
    let producer = messaging.producer();
    let healthy = CancellationToken::new();
    let refused = timeout(
        Duration::from_millis(250),
        producer.publish(&prepared, deadline(), &healthy),
    )
    .await
    .expect("a full shared window refuses without waiting");
    assert!(matches!(refused, Err(PublishError::Rejected)));
    assert_eq!(observed.load(Ordering::SeqCst), capacity);

    timeout(Duration::from_secs(7), async {
        let mut cadence = tokio::time::interval(Duration::from_millis(20));
        loop {
            cadence.tick().await;
            match producer.publish(&prepared, deadline(), &healthy).await {
                Ok(ack) => {
                    assert!(ack.duplicate);
                    break;
                }
                Err(PublishError::Rejected) => {}
                Err(PublishError::Ambiguous) => panic!("restored ACK path must confirm"),
            }
        }
    })
    .await
    .expect("native cleanup restores publication without replacing the resource");
    close(messaging).await;
    relay.join().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn post_dispatch_lost_ack_is_ambiguous_and_same_identity_retries_as_duplicate() {
    let fixture = Fixture::create(false).await;
    let relay = AckDroppingRelay::start(&fixture.stream).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options_with_servers(&fixture, vec![relay.url.clone()], None, 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted through the transparent relay");
    let prepared = registry(&fixture)
        .prepare(&event("event-ambiguous"), 1024)
        .expect("fixture event is prepared once before its ambiguous publication");

    let result = messaging
        .producer()
        .publish(
            &prepared,
            Instant::now() + Duration::from_millis(250),
            &cancel,
        )
        .await;
    assert!(matches!(result, Err(PublishError::Ambiguous)));

    let mut source = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .expect("fixture source stream remains observable after the dropped response");
    let source_info = source
        .info()
        .await
        .expect("fixture source message count is observable after dispatch");
    assert_eq!(source_info.state.messages, 1);
    let stored_sequence = source_info.state.last_sequence;
    let stored = source
        .get_raw_message(stored_sequence)
        .await
        .expect("broker retains the dispatched message despite its lost response");
    assert_eq!(
        stored.headers.get("Nats-Msg-Id").map(ToString::to_string),
        Some(prepared.publication_id().to_owned())
    );

    close(messaging).await;
    relay.join().await;

    let retry = Box::pin(Messaging::connect(
        options(&fixture, None, 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("direct retry producer is admitted after relay shutdown");
    let acknowledgment = retry
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("retrying the same prepared identity receives a broker acknowledgment");
    assert!(acknowledgment.duplicate);
    assert_eq!(acknowledgment.sequence, stored_sequence);
    let mut source = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .expect("fixture source stream remains observable after duplicate retry");
    assert_eq!(
        source
            .info()
            .await
            .expect("fixture source count is observable after duplicate retry")
            .state
            .messages,
        1
    );

    close(retry).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn typed_handler_success_is_followed_by_confirmed_source_ack() {
    let fixture = Fixture::create(false).await;
    let mut source = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .unwrap()
        .cached_info()
        .config
        .clone();
    source.retention = stream::RetentionPolicy::WorkQueue;
    fixture.replace_stream(source).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let (observed_tx, observed_rx) = oneshot::channel();
    let observed_tx = Arc::new(Mutex::new(Some(observed_tx)));
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |event, _| {
            if let Some(sender) = observed_tx
                .lock()
                .expect("test completion sender lock must not be poisoned")
                .take()
            {
                let _ = sender.send(event.id.clone());
            }
            async { Ok(()) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-consumer"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("fixture event publication is acknowledged");

    let observed = timeout(Duration::from_secs(3), observed_rx)
        .await
        .expect("typed handler must observe the broker delivery")
        .expect("handler completion signal must remain connected");
    assert_eq!(observed, "event-consumer");
    wait_for_source_ack(&fixture).await;
    assert!(
        matches!(
            fixture.jetstream.get_stream(&fixture.stream).await.unwrap().get_raw_message(1).await,
            Err(error) if matches!(error.kind(), stream::RawMessageErrorKind::NoMessageFound)
        ),
        "WorkQueue retention removes the acknowledged source record"
    );

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    close(messaging).await;
    fixture.cleanup().await;
}

/// Installs one process-wide OpenTelemetry layer; a delivery runs in its own
/// task. Returns the exporter that keeps every finished span of the process.
fn install_tracing() -> &'static opentelemetry_sdk::trace::InMemorySpanExporter {
    use opentelemetry::trace::TracerProvider as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    static EXPORTER: std::sync::OnceLock<opentelemetry_sdk::trace::InMemorySpanExporter> =
        std::sync::OnceLock::new();
    EXPORTER.get_or_init(|| {
        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );
        let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        tracing::subscriber::set_global_default(subscriber)
            .expect("no other test installs a subscriber");
        exporter
    })
}

/// The exported spans named `name`; a fixture's subject makes the name unique.
fn exported_spans(
    exporter: &opentelemetry_sdk::trace::InMemorySpanExporter,
    name: &str,
) -> Vec<opentelemetry_sdk::trace::SpanData> {
    exporter
        .get_finished_spans()
        .expect("in-memory exporter is readable")
        .into_iter()
        .filter(|span| span.name == name)
        .collect()
}

fn attribute(span: &opentelemetry_sdk::trace::SpanData, key: &str) -> Option<String> {
    span.attributes
        .iter()
        .find(|attribute| attribute.key.as_str() == key)
        .map(|attribute| attribute.value.as_str().into_owned())
}

fn trace_id(span: &tracing::Span) -> opentelemetry::trace::TraceId {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    span.context().span().span_context().trace_id()
}

#[tokio::test]
async fn handler_runs_in_the_trace_of_the_publication() {
    use tracing::Instrument as _;

    let exporter = install_tracing();
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let (observed_tx, observed_rx) = oneshot::channel();
    let observed_tx = Arc::new(Mutex::new(Some(observed_tx)));
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            if let Some(sender) = observed_tx
                .lock()
                .expect("test completion sender lock must not be poisoned")
                .take()
            {
                let _ = sender.send(trace_id(&tracing::Span::current()));
            }
            async { Ok(()) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-traced"),
        1024,
    )
    .expect("fixture event is prepared");
    let request = tracing::info_span!("request");
    messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .instrument(request.clone())
        .await
        .expect("fixture event publication is acknowledged");

    let observed = timeout(Duration::from_secs(3), observed_rx)
        .await
        .expect("typed handler must observe the broker delivery")
        .expect("handler completion signal must remain connected");
    assert_ne!(observed, opentelemetry::trace::TraceId::INVALID);
    assert_eq!(observed, trace_id(&request));
    wait_for_source_ack(&fixture).await;

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    // The exported names are low-cardinality: the subject of a publication
    // and the durable's filter for a delivery.
    for (name, kind, operation) in [
        (
            format!("publish {}", fixture.subject),
            opentelemetry::trace::SpanKind::Producer,
            "publish",
        ),
        (
            format!("process {}", fixture.subject),
            opentelemetry::trace::SpanKind::Consumer,
            "process",
        ),
    ] {
        let spans = exported_spans(exporter, &name);
        assert_eq!(spans.len(), 1, "{name}");
        let span = &spans[0];
        assert_eq!(span.span_context.trace_id(), observed, "{name}");
        assert_eq!(span.span_kind, kind, "{name}");
        assert_eq!(
            attribute(span, "messaging.operation.name").as_deref(),
            Some(operation)
        );
        assert_eq!(attribute(span, "error.type"), None, "{name}");
        assert_eq!(span.status, opentelemetry::trace::Status::Unset, "{name}");
    }
    let process = &exported_spans(exporter, &format!("process {}", fixture.subject))[0];
    assert_eq!(
        attribute(process, "messaging.destination.template").as_deref(),
        Some(fixture.subject.as_str())
    );
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn retryable_handler_is_redelivered_after_broker_nak_then_confirmed_acked() {
    let exporter = install_tracing();
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_handler = Arc::clone(&attempts);
    let (completed_tx, completed_rx) = oneshot::channel();
    let completed_tx = Arc::new(Mutex::new(Some(completed_tx)));
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            let attempt = attempts_for_handler.fetch_add(1, Ordering::SeqCst);
            let completed_tx = Arc::clone(&completed_tx);
            async move {
                if attempt == 0 {
                    Err(HandlerError::Retryable)
                } else {
                    if let Some(sender) = completed_tx
                        .lock()
                        .expect("test completion sender lock must not be poisoned")
                        .take()
                    {
                        let _ = sender.send(());
                    }
                    Ok(())
                }
            }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-retry"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("fixture event publication is acknowledged");

    timeout(Duration::from_secs(4), completed_rx)
        .await
        .expect("retryable delivery must be redelivered after the first NAK")
        .expect("handler completion signal must remain connected");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    wait_for_source_ack(&fixture).await;

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    // The failed attempt's span is an error named by its closed outcome.
    let deliveries = exported_spans(exporter, &format!("process {}", fixture.subject));
    let outcomes: Vec<_> = deliveries
        .iter()
        .map(|span| {
            (
                attribute(span, "outcome"),
                attribute(span, "error.type"),
                matches!(span.status, opentelemetry::trace::Status::Error { .. }),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            (
                Some("retryable".to_owned()),
                Some("retryable".to_owned()),
                true
            ),
            (Some("success".to_owned()), None, false),
        ]
    );
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn permanent_failure_transfers_original_record_then_redrive_keeps_logical_identity() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let (called_tx, called_rx) = oneshot::channel();
    let called_tx = Arc::new(Mutex::new(Some(called_tx)));
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            if let Some(sender) = called_tx
                .lock()
                .expect("test handler signal lock must not be poisoned")
                .take()
            {
                let _ = sender.send(());
            }
            async { Err(HandlerError::Permanent) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-permanent"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("fixture event publication is acknowledged");
    timeout(Duration::from_secs(3), called_rx)
        .await
        .expect("permanent handler must receive one delivery")
        .expect("handler signal must remain connected");

    let dead_letter = wait_for_dead_letter(&fixture).await;
    assert_dead_letter(
        &dead_letter,
        &fixture,
        "permanent",
        prepared.payload().as_ref(),
    );
    assert_eq!(
        dead_letter
            .headers
            .get("Message-Id")
            .map(ToString::to_string),
        Some("event-permanent".to_owned())
    );
    wait_for_source_ack(&fixture).await;
    handle
        .finish(deadline())
        .await
        .expect("terminal transfer worker can drain after source settlement");

    fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .unwrap()
        .get_raw_message(1)
        .await
        .expect("Limits retention keeps the source after confirmed consumer ACK");

    let record = infra_messaging::wire::DeadLetterRecord {
        subject: dead_letter.subject.to_string(),
        headers: dead_letter.headers.clone(),
        payload: dead_letter.payload.clone(),
        stream: fixture.dlq_stream.clone(),
        stream_sequence: dead_letter.sequence,
        stored_at: dead_letter.time,
    };
    let redrive = infra_messaging::wire::restore_dead_letter(record.clone())
        .expect("real DLQ record is restorable");
    let repeated = infra_messaging::wire::restore_dead_letter(record)
        .expect("the same DLQ record is repeatedly restorable");
    assert_eq!(redrive.message_id(), "event-permanent");
    assert!(redrive.publication_id().starts_with("redrive-"));
    assert_eq!(redrive.publication_id(), repeated.publication_id());
    let redrive_ack = messaging
        .producer()
        .publish(&redrive, deadline(), &cancel)
        .await
        .expect("redrive publication receives a JetStream acknowledgment");
    assert_eq!(redrive_ack.stream, fixture.stream);

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn malformed_and_unknown_envelopes_bypass_the_typed_handler_and_transfer_to_dlq() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = Arc::clone(&handler_calls);
    let mut handlers = registry(&fixture);
    handlers
        .register::<ExampleEvent, _, _>(move |_, _| {
            calls_for_handler.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(handlers)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let malformed = Bytes::from_static(b"not-a-go-compatible-envelope");
    fixture
        .jetstream
        .publish(fixture.subject.clone(), malformed.clone())
        .await
        .expect("malformed fixture message is accepted by the source stream")
        .await
        .expect("malformed fixture message receives a source publication acknowledgment");

    let malformed_dead_letter = wait_for_dead_letter(&fixture).await;
    assert_dead_letter(
        &malformed_dead_letter,
        &fixture,
        "malformed",
        malformed.as_ref(),
    );
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    wait_for_source_ack(&fixture).await;

    let unknown = registry(&fixture)
        .prepare(&event("event-unknown"), 1024)
        .expect("unknown-type fixture starts from a valid production envelope");
    let mut unknown_headers = infra_messaging::wire::encode_prepared(&unknown)
        .expect("unknown-type fixture has valid identity headers");
    unknown_headers.insert("Event-Type", "test.example.unknown");
    fixture
        .jetstream
        .publish_with_headers(
            fixture.subject.clone(),
            unknown_headers,
            unknown.payload().clone(),
        )
        .await
        .expect("unknown-type fixture message is accepted by the source stream")
        .await
        .expect("unknown-type fixture message receives a source publication acknowledgment");

    let dead_letter = wait_for_dead_letter_after(&fixture, malformed_dead_letter.sequence).await;
    assert_dead_letter(
        &dead_letter,
        &fixture,
        "permanent",
        unknown.payload().as_ref(),
    );
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    wait_for_source_ack_at_least(&fixture, 2).await;

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn unhandled_and_undecodable_deliveries_share_a_dead_letter_reason_and_differ_in_the_span() {
    let exporter = install_tracing();
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = Arc::clone(&handler_calls);
    let mut handlers = registry(&fixture);
    handlers
        .register::<ExampleEvent, _, _>(move |_, _| {
            calls_for_handler.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(handlers)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);

    // A schema version published before its consumer was deployed, then the
    // handled version with a payload that is not the handler's type.
    let newer = registry(&fixture)
        .prepare(&event("event-newer-schema"), 1024)
        .expect("newer-schema fixture starts from a valid production envelope");
    let mut newer_headers = infra_messaging::wire::encode_prepared(&newer)
        .expect("newer-schema fixture has valid identity headers");
    newer_headers.insert("Event-Schema", "v2");
    let undecodable = registry(&fixture)
        .prepare(&event("event-undecodable"), 1024)
        .expect("undecodable fixture starts from a valid production envelope");
    let undecodable_headers = infra_messaging::wire::encode_prepared(&undecodable)
        .expect("undecodable fixture has valid identity headers");
    let undecodable_payload = Bytes::from_static(br#"{"value":7}"#);

    let mut transferred = 0;
    for (headers, payload) in [
        (newer_headers, newer.payload().clone()),
        (undecodable_headers, undecodable_payload),
    ] {
        fixture
            .jetstream
            .publish_with_headers(fixture.subject.clone(), headers, payload.clone())
            .await
            .expect("fixture message is accepted by the source stream")
            .await
            .expect("fixture message receives a source publication acknowledgment");
        let dead_letter = wait_for_dead_letter_after(&fixture, transferred).await;
        assert_dead_letter(&dead_letter, &fixture, "permanent", payload.as_ref());
        transferred = dead_letter.sequence;
    }
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    wait_for_source_ack_at_least(&fixture, 2).await;

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    let causes: Vec<_> = exported_spans(exporter, &format!("process {}", fixture.subject))
        .iter()
        .map(|span| (attribute(span, "outcome"), attribute(span, "error.type")))
        .collect();
    assert_eq!(
        causes,
        [
            (Some("unhandled".to_owned()), Some("unhandled".to_owned())),
            (
                Some("undecodable".to_owned()),
                Some("undecodable".to_owned())
            ),
        ]
    );
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn lost_dlq_or_source_ack_reply_preserves_the_transfer_and_settles_without_another_dlq_record()
 {
    for lose_dlq_ack in [true, false] {
        let fixture = Fixture::create(false).await;
        let relay = if lose_dlq_ack {
            AckDroppingRelay::start(&fixture.dlq_stream).await
        } else {
            AckDroppingRelay::start_source_ack(&fixture.stream, &fixture.durable, false).await
        };
        let cancel = CancellationToken::new();
        let messaging = Box::pin(Messaging::connect(
            options_with_servers(
                &fixture,
                vec![relay.url.clone()],
                Some(consumer_options(&fixture)),
                1024,
            ),
            deadline(),
            cancel.clone(),
        ))
        .await
        .unwrap();
        let mut registered = registry(&fixture);
        registered
            .register::<ExampleEvent, _, _>(|_, _| async { Err(HandlerError::Permanent) })
            .unwrap();
        let prepared = registered
            .prepare(&event("lost-transfer-ack"), 1024)
            .unwrap();
        let mut handle = messaging.consumer(registered).await.unwrap().start(&cancel);
        messaging
            .producer()
            .publish(&prepared, deadline(), &cancel)
            .await
            .unwrap();
        let first = wait_for_dead_letter(&fixture).await;
        relay.wait_for_drop().await;
        assert_dead_letter(&first, &fixture, "permanent", prepared.payload().as_ref());
        if lose_dlq_ack {
            wait_for_source_unacked(&fixture).await;
        }
        // A lost DLQ PubAck spends the five-second request budget, then the
        // adapter requests the broker's actual 30-second delayed redelivery.
        wait_for_source_ack_within(&fixture, 1, Duration::from_secs(45)).await;
        handle
            .finish(Instant::now() + Duration::from_secs(10))
            .await
            .unwrap();
        let dlq = fixture
            .jetstream
            .get_stream(&fixture.dlq_stream)
            .await
            .unwrap();
        assert_eq!(
            dlq.cached_info().state.messages,
            1,
            "same source identity deduplicates the DLQ retry"
        );
        let retained = dlq.get_raw_message(first.sequence).await.unwrap();
        assert_eq!(retained.payload, first.payload);
        assert_eq!(
            retained.headers.get("Nats-Msg-Id"),
            first.headers.get("Nats-Msg-Id")
        );
        close(messaging).await;
        relay.join().await;
        fixture.cleanup().await;
    }
}

#[tokio::test]
async fn definite_dlq_refusal_keeps_the_source_for_redelivery_and_the_consumer_running() {
    let fixture = Fixture::create_with_limits(true, 10, 1).await;
    fixture
        .jetstream
        .publish(
            fixture.dlq_subject.clone(),
            Bytes::from_static(b"occupied-dlq"),
        )
        .await
        .expect("fixture fills its refusal-control DLQ stream")
        .await
        .expect("fixture DLQ fill receives an acknowledgment");
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(|_, _| async { Err(HandlerError::Permanent) })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-dlq-refused"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("fixture event publication is acknowledged");

    wait_for_source_unacked(&fixture).await;
    assert!(
        timeout(Duration::from_secs(2), handle.failed())
            .await
            .is_err(),
        "a refused dead-letter transfer must not stop the consumer"
    );
    let dlq = fixture
        .jetstream
        .get_stream(&fixture.dlq_stream)
        .await
        .expect("fixture DLQ stream remains available")
        .get_info()
        .await
        .expect("fixture DLQ state is observable");
    assert_eq!(
        dlq.state.messages, 1,
        "only the fixture fill reached the DLQ"
    );
    handle
        .finish(deadline())
        .await
        .expect("the consumer drains after a refused transfer");

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn sixth_delivery_bypasses_the_handler_and_transfers_as_exhausted() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-exhausted"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("fixture event publication is acknowledged");
    redeliver_until_attempt(&fixture, 6).await;

    let handler_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = Arc::clone(&handler_calls);
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            calls_for_handler.fetch_add(1, Ordering::SeqCst);
            async { Err(HandlerError::Retryable) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);

    let dead_letter = wait_for_dead_letter(&fixture).await;
    assert_dead_letter(
        &dead_letter,
        &fixture,
        "exhausted",
        prepared.payload().as_ref(),
    );
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    wait_for_source_ack(&fixture).await;

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn handler_panic_is_retried_like_a_retryable_failure() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_handler = Arc::clone(&attempts);
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            let attempt = attempts_for_handler.fetch_add(1, Ordering::SeqCst);
            async move {
                assert!(attempt > 0, "intentional first-delivery handler panic");
                Ok(())
            }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-panic"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("fixture event publication is acknowledged");

    wait_for_source_ack(&fixture).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    handle
        .finish(deadline())
        .await
        .expect("a handler panic must not stop the consumer");

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn oversized_source_message_is_dead_lettered_without_invoking_the_handler() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let producer = Box::pin(Messaging::connect(
        options(&fixture, None, 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture producer is admitted against the original source bound");
    let oversized = registry(&fixture)
        .prepare(&event_with_value("event-oversized", &"x".repeat(128)), 1024)
        .expect("fixture event is valid for the original producer bound");
    producer
        .producer()
        .publish(&oversized, deadline(), &cancel)
        .await
        .expect("source retains the event before the operator shrinks its bound");
    close(producer).await;

    let source = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .expect("fixture source is available for the operator update");
    let mut source_config = source.cached_info().config.clone();
    source_config.max_message_size = 32 + 8 * 1024;
    let updated = fixture
        .jetstream
        .update_stream(source_config)
        .await
        .expect("operator can shrink the stream limit without deleting retained records");
    assert_eq!(updated.state.messages, 1);

    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 32),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted for the smaller delivery bound");
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = Arc::clone(&handler_calls);
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            calls_for_handler.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);

    let dead_letter = wait_for_dead_letter(&fixture).await;
    assert_dead_letter(
        &dead_letter,
        &fixture,
        "malformed",
        oversized.payload().as_ref(),
    );
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    wait_for_source_ack(&fixture).await;
    handle
        .finish(deadline())
        .await
        .expect("an oversized delivery must not stop the consumer");

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn drain_forces_unfinished_handler_shutdown_at_the_shared_deadline() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let started = Arc::new(Notify::new());
    let started_for_handler = Arc::clone(&started);
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            let started = Arc::clone(&started_for_handler);
            async move {
                started.notify_one();
                std::future::pending::<Result<(), HandlerError>>().await
            }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-drain"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("fixture event publication is acknowledged");
    timeout(Duration::from_secs(3), started.notified())
        .await
        .expect("handler must begin before drain is requested");

    let outcome = handle.finish(Instant::now() + Duration::from_secs(1)).await;
    assert!(matches!(outcome, Err(ConsumerError::DrainTimedOut)));
    wait_for_source_unacked(&fixture).await;

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn consumer_reconciles_its_named_durable_without_stream_administration() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(|_, _| async { Ok(()) })
        .expect("fixture handler is registered");

    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("adapter creates or reconciles only its named durable consumer")
        .start(&cancel);
    let stream = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .expect("adapter must not remove the operator-owned source stream");
    let durable: consumer::PullConsumer = stream
        .get_consumer(&fixture.durable)
        .await
        .expect("adapter creates the configured durable consumer");
    let actual = durable
        .get_info()
        .await
        .expect("created durable consumer has observable configuration");
    assert_eq!(actual.config.filter_subject, fixture.subject);
    assert_eq!(actual.config.max_deliver, -1);
    assert_eq!(actual.config.ack_policy, consumer::AckPolicy::Explicit);
    assert_eq!(actual.config.ack_wait, Duration::from_secs(41));
    assert!(
        actual.config.max_ack_pending > 1,
        "the durable keeps the broker default instead of one replica's concurrency"
    );
    handle
        .finish(deadline())
        .await
        .expect("empty durable consumer drains within the shared deadline");
    close(messaging).await;
    fixture.cleanup().await;
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct PublishedElsewhere;

impl EventPayload for PublishedElsewhere {
    const EVENT_TYPE: &'static str = "test.example.published_elsewhere";
    const SCHEMA_VERSION: u16 = 1;
}

#[tokio::test]
async fn consumer_admission_checks_the_filter_for_handled_subjects_only() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let routes = || {
        Registry::new([
            Route::new::<ExampleEvent>(fixture.subject.clone()),
            Route::new::<PublishedElsewhere>("elsewhere.published"),
        ])
        .expect("fixture routes are valid")
    };

    // The process only publishes `PublishedElsewhere`; its durable need not
    // select that subject.
    let mut publishes_elsewhere = routes();
    publishes_elsewhere
        .register::<ExampleEvent, _, _>(|_, _| async { Ok(()) })
        .expect("fixture handler is registered");
    messaging
        .consumer(publishes_elsewhere)
        .await
        .expect("a publish-only route outside the filter does not refuse the consumer");

    let mut handles_elsewhere = routes();
    handles_elsewhere
        .register::<PublishedElsewhere, _, _>(|_, _| async { Ok(()) })
        .expect("fixture handler is registered");
    let refused = messaging
        .consumer(handles_elsewhere)
        .await
        .expect_err("a handler the filter can never reach refuses the consumer");
    assert!(
        matches!(refused, infra_messaging::MessagingError::Configuration(_)),
        "{refused:?}"
    );

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn durable_deleted_between_pulls_stops_the_consumer() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(|_, _| async { Ok(()) })
        .expect("fixture handler is registered");
    let consumer = messaging
        .consumer(registry)
        .await
        .expect("adapter creates its named durable consumer");
    // No pull is waiting yet, so the broker has no request to terminate with
    // `Consumer Deleted`; the next pull meets a subject nobody answers.
    fixture
        .jetstream
        .delete_consumer_from_stream(&fixture.durable, &fixture.stream)
        .await
        .expect("test administrator deletes the durable consumer");

    let handle = consumer.start(&cancel);
    let failure = timeout(Duration::from_secs(10), handle.failed())
        .await
        .expect("a durable the broker reports missing must stop the consumer");
    assert!(matches!(failure, ConsumerError::ConsumerLost));

    drop(handle);
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn deleting_a_durable_with_a_waiting_batch_stops_the_consumer() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("source stream is admitted");
    let mut registered = registry(&fixture);
    registered
        .register::<ExampleEvent, _, _>(|_, _| async { Ok(()) })
        .unwrap();
    let mut handle = messaging.consumer(registered).await.unwrap().start(&cancel);
    timeout(Duration::from_secs(3), async {
        let mut cadence = tokio::time::interval(Duration::from_millis(20));
        loop {
            let durable: consumer::PullConsumer = fixture
                .jetstream
                .get_consumer_from_stream(&fixture.durable, &fixture.stream)
                .await
                .unwrap();
            if durable.cached_info().num_waiting > 0 {
                break;
            }
            cadence.tick().await;
        }
    })
    .await
    .expect("the broker observes a waiting batch");
    fixture
        .jetstream
        .delete_consumer_from_stream(&fixture.durable, &fixture.stream)
        .await
        .unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(10), handle.failed())
            .await
            .unwrap(),
        ConsumerError::ConsumerLost
    ));
    assert!(matches!(
        handle.finish(deadline()).await,
        Err(ConsumerError::ConsumerLost)
    ));
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn a_durable_replaced_between_full_batches_stops_before_another_handler() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .unwrap();
    for id in ["before-replacement", "pending-after-replacement"] {
        let prepared = registry(&fixture).prepare(&event(id), 1024).unwrap();
        messaging
            .producer()
            .publish(&prepared, deadline(), &cancel)
            .await
            .unwrap();
    }
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registered = registry(&fixture);
    registered
        .register::<ExampleEvent, _, _>({
            let entered = Arc::clone(&entered);
            let calls = Arc::clone(&calls);
            let release = release.clone();
            move |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                entered.notify_one();
                let release = release.clone();
                async move {
                    release.cancelled().await;
                    Ok(())
                }
            }
        })
        .unwrap();
    let mut handle = messaging.consumer(registered).await.unwrap().start(&cancel);
    timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let source = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    fixture
        .jetstream
        .delete_consumer_from_stream(&fixture.durable, &fixture.stream)
        .await
        .unwrap();
    source
        .create_consumer(consumer::pull::Config {
            durable_name: Some(fixture.durable.clone()),
            filter_subject: fixture.subject.clone(),
            ack_policy: consumer::AckPolicy::Explicit,
            ack_wait: Duration::from_secs(41),
            max_deliver: -1,
            ..Default::default()
        })
        .await
        .unwrap();
    release.cancel();
    assert!(matches!(
        timeout(Duration::from_secs(10), handle.failed())
            .await
            .expect("a successful previous batch cannot hide replacement"),
        ConsumerError::ConsumerLost
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        handle.finish(deadline()).await,
        Err(ConsumerError::ConsumerLost)
    ));
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn an_unanswered_identity_check_blocks_pulls_without_losing_the_consumer() {
    let fixture = Fixture::create(false).await;
    let refuse_info = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let drop_info = Arc::clone(&refuse_info);
    let stream = fixture.stream.clone();
    let relay = AckDroppingRelay::start_filtering(move |payload| {
        drop_info.load(Ordering::SeqCst)
            && serde_json::from_slice::<serde_json::Value>(payload)
                .is_ok_and(|info| info.get("ack_floor").is_some() && info["stream_name"] == stream)
    })
    .await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options_with_servers(
            &fixture,
            vec![relay.url.clone()],
            Some(consumer_options(&fixture)),
            1024,
        ),
        deadline(),
        cancel.clone(),
    ))
    .await
    .unwrap();
    let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut registered = registry(&fixture);
    registered
        .register::<ExampleEvent, _, _>(move |event, _| {
            observed_tx.send(event.id).unwrap();
            async { Ok(()) }
        })
        .unwrap();
    let consumer = messaging.consumer(registered).await.unwrap();
    let prepared = registry(&fixture)
        .prepare(&event("waiting-for-identity"), 1024)
        .unwrap();
    messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .unwrap();
    refuse_info.store(true, Ordering::SeqCst);
    let mut handle = consumer.start(&cancel);
    timeout(Duration::from_secs(3), async {
        let mut cadence = tokio::time::interval(Duration::from_millis(20));
        while !relay.dropped_ack.load(Ordering::SeqCst) {
            cadence.tick().await;
        }
    })
    .await
    .expect("the broker answered the identity query but its reply was withheld");
    assert!(
        messaging.probe().check().await.is_ok(),
        "the NATS connection itself is healthy"
    );
    // Observe across the five-second request budget and its one-second retry
    // backoff; a fail-open lookup would admit the retained event here.
    assert!(
        timeout(Duration::from_secs(7), observed_rx.recv())
            .await
            .is_err()
    );
    let durable: consumer::PullConsumer = fixture
        .jetstream
        .get_consumer_from_stream(&fixture.durable, &fixture.stream)
        .await
        .unwrap();
    assert_eq!(durable.cached_info().num_ack_pending, 0);
    assert_eq!(durable.cached_info().num_pending, 1);
    refuse_info.store(false, Ordering::SeqCst);
    assert_eq!(
        timeout(Duration::from_secs(15), observed_rx.recv())
            .await
            .unwrap()
            .unwrap(),
        "waiting-for-identity"
    );
    wait_for_source_ack(&fixture).await;
    handle.finish(deadline()).await.unwrap();
    close(messaging).await;
    relay.join().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn a_consumer_recovers_after_its_broker_connection_is_interrupted() {
    let fixture = Fixture::create(false).await;
    let relay = OutageRelay::start().await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options_with_servers(
            &fixture,
            vec![relay.url.clone()],
            Some(consumer_options(&fixture)),
            1024,
        ),
        deadline(),
        cancel.clone(),
    ))
    .await
    .unwrap();
    let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut registered = registry(&fixture);
    registered
        .register::<ExampleEvent, _, _>(move |event, _| {
            observed_tx.send(event.id).unwrap();
            async { Ok(()) }
        })
        .unwrap();
    let mut handle = messaging.consumer(registered).await.unwrap().start(&cancel);
    let before = registry(&fixture)
        .prepare(&event("before-outage"), 1024)
        .unwrap();
    messaging
        .producer()
        .publish(&before, deadline(), &cancel)
        .await
        .unwrap();
    assert_eq!(
        timeout(Duration::from_secs(3), observed_rx.recv())
            .await
            .unwrap()
            .unwrap(),
        "before-outage"
    );
    wait_for_source_ack(&fixture).await;

    relay.pause.cancel();
    timeout(Duration::from_secs(3), async {
        let mut cadence = tokio::time::interval(Duration::from_millis(20));
        while messaging.probe().check().await.is_ok() {
            cadence.tick().await;
        }
    })
    .await
    .expect("the disconnected dependency becomes unready");
    let after = registry(&fixture)
        .prepare(&event("after-outage"), 1024)
        .unwrap();
    fixture
        .jetstream
        .publish_with_headers(
            fixture.subject.clone(),
            infra_messaging::wire::encode_prepared(&after).unwrap(),
            after.payload().clone(),
        )
        .await
        .unwrap()
        .await
        .unwrap();
    relay.resume.cancel();
    assert_eq!(
        timeout(Duration::from_secs(45), observed_rx.recv())
            .await
            .expect("the next bounded batch recovers the retained message")
            .unwrap(),
        "after-outage"
    );
    wait_for_source_ack_at_least(&fixture, 2).await;
    handle.finish(deadline()).await.unwrap();
    close(messaging).await;
    relay.finish().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one broker scenario proves admission, one-slot refill and drain together"
)]
async fn pulls_reserve_handler_slots_and_drain_leaves_unadmitted_messages_at_the_broker() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let consumer_options = ConsumerOptions {
        concurrency: 2,
        ..consumer_options(&fixture)
    };
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options), 1024),
        Instant::now() + Duration::from_secs(30),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let release_first = CancellationToken::new();
    let release_other = CancellationToken::new();
    let mut blocking = registry(&fixture);
    blocking
        .register::<ExampleEvent, _, _>({
            let release_first = release_first.clone();
            let release_other = release_other.clone();
            move |event, _| {
                let _ = started_tx.send(event.id.clone());
                let release = if event.id == "event-in-flight-1" {
                    release_first.clone()
                } else {
                    release_other.clone()
                };
                async move {
                    release.cancelled().await;
                    Ok(())
                }
            }
        })
        .expect("fixture handler is registered");
    for id in [
        "event-in-flight-1",
        "event-in-flight-2",
        "event-next",
        "event-unadmitted",
    ] {
        let prepared =
            infra_messaging::PreparedEvent::prepare(fixture.subject.clone(), &event(id), 1024)
                .expect("fixture event is prepared");
        messaging
            .producer()
            .publish(&prepared, deadline(), &cancel)
            .await
            .expect("fixture event publication is acknowledged");
    }
    let mut first = messaging
        .consumer(blocking)
        .await
        .expect("adapter creates its named durable consumer")
        .start(&cancel);
    timeout(Duration::from_secs(3), async {
        let first_id = started_rx.recv().await.expect("first handler starts");
        let second_id = started_rx.recv().await.expect("second handler starts");
        assert_ne!(first_id, second_id);
        assert!([first_id.as_str(), second_id.as_str()].contains(&"event-in-flight-1"));
        assert!([first_id.as_str(), second_id.as_str()].contains(&"event-in-flight-2"));
        // Observe broker ownership while the slots remain occupied, including
        // the interval in which the old client stream issued its next batch.
        let observed_until = Instant::now() + Duration::from_secs(1);
        let mut cadence = tokio::time::interval(Duration::from_millis(20));
        while Instant::now() < observed_until {
            cadence.tick().await;
            let durable: consumer::PullConsumer = fixture
                .jetstream
                .get_consumer_from_stream(&fixture.durable, &fixture.stream)
                .await
                .expect("the durable consumer exists");
            assert_eq!(durable.cached_info().num_ack_pending, 2);
            assert_eq!(durable.cached_info().num_pending, 2);
            assert_eq!(durable.cached_info().num_redelivered, 0);
        }
    })
    .await
    .expect("two occupied slots must leave both other events at the broker");

    release_first.cancel();
    let next = timeout(Duration::from_secs(3), started_rx.recv())
        .await
        .expect("one freed slot admits the next event")
        .expect("handler channel remains connected");
    assert_eq!(next, "event-next");
    let durable: consumer::PullConsumer = fixture
        .jetstream
        .get_consumer_from_stream(&fixture.durable, &fixture.stream)
        .await
        .expect("the durable remains observable");
    assert_eq!(durable.cached_info().num_ack_pending, 2);
    assert_eq!(durable.cached_info().num_pending, 1);

    first.drain();
    release_other.cancel();
    first
        .finish(deadline())
        .await
        .expect("the in-flight deliveries settle within the shared deadline");

    let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut recording = registry(&fixture);
    recording
        .register::<ExampleEvent, _, _>(move |event, _| {
            let _ = observed_tx.send(event.id);
            async { Ok(()) }
        })
        .expect("fixture handler is registered");
    let mut second = messaging
        .consumer(recording)
        .await
        .expect("the durable consumer is admitted again")
        .start(&cancel);
    let observed = timeout(Duration::from_secs(10), observed_rx.recv())
        .await
        .expect("an unadmitted event remains available without waiting for ack wait")
        .expect("handler completion signal must remain connected");
    assert_eq!(observed, "event-unadmitted");

    second
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    close(messaging).await;
    fixture.cleanup().await;
}

/// The fields of every event logged on this thread while the guard lives.
type Logged = Arc<Mutex<Vec<Vec<(&'static str, String)>>>>;

struct LoggedFields(Vec<(&'static str, String)>);

impl tracing::field::Visit for LoggedFields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name(), format!("{value:?}")));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push((field.name(), value.to_owned()));
    }
}

struct LogCapture(Logged);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LogCapture {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut fields = LoggedFields(Vec::new());
        event.record(&mut fields);
        self.0.lock().expect("log lock").push(fields.0);
    }
}

fn capture_logs() -> (tracing::subscriber::DefaultGuard, Logged) {
    use tracing_subscriber::layer::SubscriberExt as _;

    let logged = Logged::default();
    let subscriber = tracing_subscriber::registry().with(LogCapture(Arc::clone(&logged)));
    (tracing::subscriber::set_default(subscriber), logged)
}

/// The `messaging_admission_failed` events as (`error.type`, `required_bytes`, `limit_bytes`).
fn admission_refusals(logged: &Logged) -> Vec<(String, String, Option<String>)> {
    let field = |event: &[(&'static str, String)], name: &str| {
        event
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, value)| value.clone())
    };
    logged
        .lock()
        .expect("log lock")
        .iter()
        .filter(|event| field(event, "message").as_deref() == Some("messaging_admission_failed"))
        .map(|event| {
            (
                field(event, "error.type").expect("refusal names its error type"),
                field(event, "required_bytes").expect("refusal names the delivery size"),
                field(event, "limit_bytes"),
            )
        })
        .collect()
}

#[tokio::test]
async fn admission_refuses_unacknowledged_or_volatile_streams_for_the_publishing_role() {
    for (no_ack, storage, persist_mode, error_type) in [
        (true, stream::StorageType::File, None, "stream_no_ack"),
        (
            false,
            stream::StorageType::Memory,
            None,
            "stream_memory_storage",
        ),
        (
            false,
            stream::StorageType::File,
            Some(stream::PersistenceMode::Async),
            "stream_async_persistence",
        ),
    ] {
        for (consumer_mode, dead_letter) in [(false, false), (true, false), (true, true)] {
            let fixture = Fixture::create(false).await;
            let name = if dead_letter {
                &fixture.dlq_stream
            } else {
                &fixture.stream
            };
            let mut config = fixture
                .jetstream
                .get_stream(name)
                .await
                .unwrap()
                .cached_info()
                .config
                .clone();
            config.storage = storage;
            config.persist_mode = persist_mode;
            config.no_ack = no_ack;
            fixture.replace_stream(config).await;
            let (_guard, logged) = capture_logs();
            let refused = Box::pin(Messaging::connect(
                options(
                    &fixture,
                    consumer_mode.then(|| consumer_options(&fixture)),
                    1024,
                ),
                deadline(),
                CancellationToken::new(),
            ))
            .await
            .expect_err("each publishing role requires ACKs and durable storage");
            assert!(matches!(refused, infra_messaging::MessagingError::Topology));
            assert_eq!(
                admission_refusals(&logged),
                [(error_type.to_owned(), (1024 + 8192).to_string(), None)]
            );
            let role = if dead_letter {
                "dead_letter_stream"
            } else {
                "source_stream"
            };
            assert!(logged.lock().unwrap().iter().any(|event| {
                event
                    .iter()
                    .any(|(name, value)| *name == "operation" && value == role)
            }));
            assert_eq!(
                fixture
                    .jetstream
                    .get_stream(&fixture.stream)
                    .await
                    .unwrap()
                    .cached_info()
                    .state
                    .consumer_count,
                0
            );
            fixture.cleanup().await;
        }
    }
}

#[tokio::test]
async fn publisher_admits_explicit_default_persistence_without_a_dead_letter_stream() {
    let fixture = Fixture::create(false).await;
    fixture
        .jetstream
        .delete_stream(&fixture.dlq_stream)
        .await
        .unwrap();
    let mut config = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .unwrap()
        .cached_info()
        .config
        .clone();
    config.persist_mode = Some(stream::PersistenceMode::Default);
    fixture.replace_stream(config).await;
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, None, 1024),
        deadline(),
        CancellationToken::new(),
    ))
    .await
    .unwrap();
    messaging
        .producer()
        .publish(
            &registry(&fixture)
                .prepare(&event("publisher-no-dlq"), 1024)
                .unwrap(),
            deadline(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    close(messaging).await;
    fixture
        .jetstream
        .delete_stream(&fixture.stream)
        .await
        .unwrap();
}

/// Fixture byte count follows the NATS header framing, independently of adapter sizing.
fn encoded_headers(headers: &async_nats::HeaderMap) -> usize {
    let mut encoded = String::from("NATS/1.0\r\n");
    for (name, values) in headers.iter() {
        for value in values {
            use std::fmt::Write as _;
            write!(encoded, "{name}: {value}\r\n").unwrap();
        }
    }
    encoded.push_str("\r\n");
    encoded.len()
}

/// Independent native line encoding, including the replacement publication ID.
fn transfer_additions(subject: &str, stream: &str) -> usize {
    format!("Nats-Msg-Id: {}\r\nOriginal-Subject: {subject}\r\nDead-Letter-Reason: permanent\r\nNats-Expected-Stream: {stream}\r\n", "x".repeat(68)).len()
}

#[tokio::test]
async fn consumer_admits_the_transfer_bound_and_refuses_one_byte_less_before_durable_creation() {
    for short in [true, false] {
        let fixture = Fixture::create(false).await;
        let required = 1024 + 8192 + transfer_additions(&fixture.subject, &fixture.dlq_stream);
        let mut config = fixture
            .jetstream
            .get_stream(&fixture.dlq_stream)
            .await
            .unwrap()
            .cached_info()
            .config
            .clone();
        config.max_message_size = i32::try_from(required - usize::from(short)).unwrap();
        fixture.jetstream.update_stream(config).await.unwrap();
        let cancel = CancellationToken::new();
        let messaging = Box::pin(Messaging::connect(
            options(&fixture, Some(consumer_options(&fixture)), 1024),
            deadline(),
            cancel.clone(),
        ))
        .await
        .unwrap();
        let mut registered = registry(&fixture);
        registered
            .register::<ExampleEvent, _, _>(|_, _| async { Err(HandlerError::Permanent) })
            .unwrap();
        let (_guard, logged) = capture_logs();
        let consumer = messaging.consumer(registered).await;
        if short {
            assert!(matches!(
                consumer,
                Err(infra_messaging::MessagingError::Bounds)
            ));
            assert_eq!(
                admission_refusals(&logged),
                [(
                    "stream_max_message_size".to_owned(),
                    required.to_string(),
                    Some((required - 1).to_string())
                )]
            );
            assert_eq!(
                fixture
                    .jetstream
                    .get_stream(&fixture.stream)
                    .await
                    .unwrap()
                    .cached_info()
                    .state
                    .consumer_count,
                0
            );
        } else {
            let mut handle = consumer.unwrap().start(&cancel);
            let prepared = registry(&fixture)
                .prepare(&event_with_value("full-transfer", &"x".repeat(1012)), 1024)
                .unwrap();
            assert_eq!(prepared.payload().len(), 1024);
            let mut headers = infra_messaging::wire::encode_prepared(&prepared).unwrap();
            headers.insert("tracestate", "");
            let trace = "x".repeat(8192 - encoded_headers(&headers));
            headers.insert("tracestate", trace.as_str());
            assert_eq!(encoded_headers(&headers), 8192);
            fixture
                .jetstream
                .publish_with_headers(fixture.subject.clone(), headers, prepared.payload().clone())
                .await
                .unwrap()
                .await
                .unwrap();
            let transferred = wait_for_dead_letter(&fixture).await;
            assert_dead_letter(&transferred, &fixture, "permanent", prepared.payload());
            assert_eq!(
                transferred.headers.get("tracestate").unwrap().as_str(),
                trace
            );
            wait_for_source_ack(&fixture).await;
            handle.finish(deadline()).await.unwrap();
        }
        close(messaging).await;
        fixture.cleanup().await;
    }
}

#[tokio::test]
async fn unlimited_dlq_cannot_exceed_the_advertised_server_transfer_limit() {
    let fixture = Fixture::create(false).await;
    let server_limit = async_nats::connect(nats_url())
        .await
        .unwrap()
        .server_info()
        .max_payload;
    for name in [&fixture.stream, &fixture.dlq_stream] {
        let mut config = fixture
            .jetstream
            .get_stream(name)
            .await
            .unwrap()
            .cached_info()
            .config
            .clone();
        config.max_message_size = if name == &fixture.stream {
            i32::try_from(server_limit).unwrap()
        } else {
            -1
        };
        fixture.jetstream.update_stream(config).await.unwrap();
    }
    let messaging = Box::pin(Messaging::connect(
        options(
            &fixture,
            Some(consumer_options(&fixture)),
            server_limit - 8192,
        ),
        deadline(),
        CancellationToken::new(),
    ))
    .await
    .unwrap();
    let mut registered = registry(&fixture);
    registered
        .register::<ExampleEvent, _, _>(|_, _| async { Ok(()) })
        .unwrap();
    let (_guard, logged) = capture_logs();
    assert!(matches!(
        messaging.consumer(registered).await,
        Err(infra_messaging::MessagingError::Bounds)
    ));
    assert_eq!(
        admission_refusals(&logged),
        [(
            "server_max_payload".to_owned(),
            (server_limit + transfer_additions(&fixture.subject, &fixture.dlq_stream)).to_string(),
            Some(server_limit.to_string())
        )]
    );
    assert_eq!(
        fixture
            .jetstream
            .get_stream(&fixture.stream)
            .await
            .unwrap()
            .cached_info()
            .state
            .consumer_count,
        0
    );
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn selected_routes_without_handlers_obey_the_native_header_ceiling() {
    for (selected, excess) in [(true, 0), (true, 1), (false, 1)] {
        let fixture = Fixture::create(false).await;
        let mut config = fixture
            .jetstream
            .get_stream(&fixture.dlq_stream)
            .await
            .unwrap()
            .cached_info()
            .config
            .clone();
        config.max_message_size = -1;
        fixture.jetstream.update_stream(config).await.unwrap();
        let mut options = options(&fixture, Some(consumer_options(&fixture)), 1024);
        options.consumer.as_mut().unwrap().filter_subject = format!("{}.events.>", fixture.subject);
        // Both the handled route and selected publish-only route lie below this
        // filter; no long subject enters a broker control line during admission.
        let mut source = fixture
            .jetstream
            .get_stream(&fixture.stream)
            .await
            .unwrap()
            .cached_info()
            .config
            .clone();
        source.subjects = vec![format!("{}.events.>", fixture.subject)];
        fixture.jetstream.update_stream(source).await.unwrap();
        let handled_subject = format!("{}.events.handled", fixture.subject);
        let prefix = if selected {
            format!("{}.events.", fixture.subject)
        } else {
            "elsewhere.".to_owned()
        };
        let subject_bytes = 65_535 + excess - 8192 - transfer_additions("", &fixture.dlq_stream);
        let subject = format!("{prefix}{}", "x".repeat(subject_bytes - prefix.len()));
        let mut registered = Registry::new([
            Route::new::<ExampleEvent>(handled_subject),
            Route::new::<UnhandledEvent>(subject),
        ])
        .unwrap();
        registered
            .register::<ExampleEvent, _, _>(|_, _| async { Ok(()) })
            .unwrap();
        let messaging = Box::pin(Messaging::connect(
            options,
            deadline(),
            CancellationToken::new(),
        ))
        .await
        .unwrap();
        let (_guard, logged) = capture_logs();
        let admitted = messaging.consumer(registered).await;
        if selected && excess > 0 {
            assert!(matches!(
                admitted,
                Err(infra_messaging::MessagingError::Bounds)
            ));
            assert_eq!(
                admission_refusals(&logged),
                [(
                    "dead_letter_header_bytes".to_owned(),
                    "65536".to_owned(),
                    Some("65535".to_owned())
                )]
            );
            assert_eq!(
                fixture
                    .jetstream
                    .get_stream(&fixture.stream)
                    .await
                    .unwrap()
                    .cached_info()
                    .state
                    .consumer_count,
                0
            );
        } else {
            admitted.unwrap();
        }
        close(messaging).await;
        fixture.cleanup().await;
    }
}

#[tokio::test]
async fn admission_names_the_broker_limit_that_cannot_carry_one_delivery() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let server_max_payload = async_nats::connect(nats_url())
        .await
        .expect("fixture broker is reachable")
        .server_info()
        .max_payload;
    let (_guard, logged) = capture_logs();

    // One delivery is the payload limit plus 8 KiB of headers, and the
    // server bounds the two together.
    let refused = Box::pin(Messaging::connect(
        options(&fixture, None, server_max_payload),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect_err("a delivery the server cannot carry is refused");
    assert!(
        matches!(refused, infra_messaging::MessagingError::Bounds),
        "{refused:?}"
    );

    // A consumer's source stream must bound its own messages.
    let unbounded = format!("{}_UNBOUNDED", fixture.stream);
    fixture
        .jetstream
        .create_stream(stream::Config {
            name: unbounded.clone(),
            subjects: vec![format!("{}.unbounded", fixture.subject)],
            ..Default::default()
        })
        .await
        .expect("test stream without a message limit is created");
    let mut without_limit = options(&fixture, Some(consumer_options(&fixture)), 1024);
    without_limit.source_stream.clone_from(&unbounded);
    let refused = Box::pin(Messaging::connect(
        without_limit,
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect_err("a source stream without a message limit is refused");
    assert!(
        matches!(refused, infra_messaging::MessagingError::Bounds),
        "{refused:?}"
    );

    // A stream that admits more than one delivery is refused too.
    let refused = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 512),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect_err("a source stream that admits a larger message is refused");
    assert!(
        matches!(refused, infra_messaging::MessagingError::Bounds),
        "{refused:?}"
    );

    assert_eq!(
        admission_refusals(&logged),
        [
            (
                "server_max_payload".to_owned(),
                (server_max_payload + 8 * 1024).to_string(),
                Some(server_max_payload.to_string()),
            ),
            (
                "stream_max_message_size_unset".to_owned(),
                (1024 + 8 * 1024).to_string(),
                Some("-1".to_owned()),
            ),
            (
                "stream_max_message_size".to_owned(),
                (512 + 8 * 1024).to_string(),
                Some((1024 + 8 * 1024).to_string()),
            ),
        ]
    );

    fixture
        .jetstream
        .delete_stream(&unbounded)
        .await
        .expect("test stream is removable");
    fixture.cleanup().await;
}

/// A transparent relay that records the user JWT of every client `CONNECT`
/// and cuts the live connection on request.
struct ConnectRecordingRelay {
    url: String,
    jwts: Arc<Mutex<Vec<String>>>,
    cut: Arc<Notify>,
    task: JoinHandle<()>,
}

impl ConnectRecordingRelay {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test relay listener must bind an ephemeral loopback port");
        let address = listener
            .local_addr()
            .expect("test relay listener must report its loopback address");
        let target = relay_target(&nats_url());
        let jwts = Arc::new(Mutex::new(Vec::new()));
        let cut = Arc::new(Notify::new());
        let (recorded, cut_now) = (Arc::clone(&jwts), Arc::clone(&cut));
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let Ok(broker) = TcpStream::connect(&target).await else {
                    return;
                };
                tokio::select! {
                    _ = record_connect(client, broker, &recorded) => {}
                    () = cut_now.notified() => {}
                }
            }
        });
        Self {
            url: format!("nats://{address}"),
            jwts,
            cut,
            task,
        }
    }

    async fn jwts(&self, count: usize) -> Vec<String> {
        timeout(Duration::from_secs(10), async {
            loop {
                let jwts = self.jwts.lock().expect("relay lock").clone();
                if jwts.len() >= count {
                    return jwts;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the client connects through the relay")
    }
}

async fn record_connect(
    client: TcpStream,
    broker: TcpStream,
    jwts: &Mutex<Vec<String>>,
) -> Result<(), std::io::Error> {
    let (client_read, mut client_write) = client.into_split();
    let (mut broker_read, mut broker_write) = broker.into_split();
    let mut client_read = BufReader::new(client_read);
    let to_client = tokio::io::copy(&mut broker_read, &mut client_write);
    let to_broker = async {
        let mut line = Vec::new();
        client_read.read_until(b'\n', &mut line).await?;
        let connect: serde_json::Value = line
            .strip_prefix(b"CONNECT ")
            .and_then(|json| serde_json::from_slice(json).ok())
            .expect("the client's first protocol line is CONNECT");
        let jwt = connect["jwt"].as_str().unwrap_or_default().to_owned();
        jwts.lock().expect("relay lock").push(jwt);
        broker_write.write_all(&line).await?;
        tokio::io::copy(&mut client_read, &mut broker_write).await
    };
    tokio::try_join!(to_client, to_broker).map(|_| ())
}

/// A credentials file as `nsc generate creds` writes it. The broker of this
/// suite authenticates nobody, so only the client reads the JWT.
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn write_creds(path: &std::path::Path, jwt: &str) {
    let seed = nkeys::KeyPair::new_user()
        .seed()
        .expect("a generated user key has a seed");
    let creds = format!(
        "-----BEGIN NATS USER JWT-----\n{jwt}\n------END NATS USER JWT------\n\n\
         -----BEGIN USER NKEY SEED-----\n{seed}\n------END USER NKEY SEED------\n"
    );
    let replacement = path.with_extension("next");
    std::fs::write(&replacement, creds).expect("credentials file is written");
    std::fs::rename(replacement, path).expect("credentials file is replaced");
}

#[tokio::test]
async fn a_credentials_file_is_read_again_for_a_reconnect() {
    let fixture = Fixture::create(false).await;
    let relay = ConnectRecordingRelay::start().await;
    let cancel = CancellationToken::new();
    let dir = tempfile::tempdir().expect("temporary directory is created");
    let creds = dir.path().join("nats.creds");
    write_creds(&creds, "first.user.jwt");

    let mut with_file = options_with_servers(&fixture, vec![relay.url.clone()], None, 1024);
    with_file.credentials_file = Some(creds.clone());
    let messaging = Box::pin(Messaging::connect(with_file, deadline(), cancel.clone()))
        .await
        .expect("a connection that authenticates from a credentials file is admitted");
    assert_eq!(relay.jwts(1).await, ["first.user.jwt"]);

    // The platform rotates the file, then the broker ends the connection.
    write_creds(&creds, "rotated.user.jwt");
    relay.cut.notify_one();
    assert_eq!(relay.jwts(2).await, ["first.user.jwt", "rotated.user.jwt"]);

    let prepared = registry(&fixture)
        .prepare(&event("event-after-rotation"), 1024)
        .expect("fixture event is prepared");
    timeout(Duration::from_secs(10), async {
        while messaging
            .producer()
            .publish(&prepared, deadline(), &cancel)
            .await
            .is_err()
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the reconnected client publishes again");

    close(messaging).await;
    relay.task.abort();
    fixture.cleanup().await;
}

#[tokio::test]
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
async fn an_unusable_credentials_source_refuses_the_connection() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let dir = tempfile::tempdir().expect("temporary directory is created");

    let mut missing = options(&fixture, None, 1024);
    missing.credentials_file = Some(dir.path().join("absent.creds"));
    let refused = Box::pin(Messaging::connect(missing, deadline(), cancel.clone()))
        .await
        .expect_err("a missing credentials file refuses the connection");
    assert!(
        matches!(refused, infra_messaging::MessagingError::Configuration(_)),
        "{refused:?}"
    );

    let malformed_path = dir.path().join("malformed.creds");
    std::fs::write(&malformed_path, "not credentials").expect("fixture file is written");
    let mut malformed = options(&fixture, None, 1024);
    malformed.credentials_file = Some(malformed_path.clone());
    let refused = Box::pin(Messaging::connect(malformed, deadline(), cancel.clone()))
        .await
        .expect_err("a malformed credentials file refuses the connection");
    assert!(
        matches!(refused, infra_messaging::MessagingError::Authentication),
        "{refused:?}"
    );

    let mut both = options(&fixture, None, 1024);
    both.credentials = Some("inline".to_owned().into());
    both.credentials_file = Some(malformed_path);
    let refused = Box::pin(Messaging::connect(both, deadline(), cancel.clone()))
        .await
        .expect_err("two credential sources refuse the connection");
    assert!(
        matches!(refused, infra_messaging::MessagingError::Configuration(_)),
        "{refused:?}"
    );

    fixture.cleanup().await;
}
