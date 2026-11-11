//! Integration tests against a live `JetStream` broker.

#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration tests make failures and broker setup explicit"
)]

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_nats::jetstream::{self, consumer, stream};
use bytes::Bytes;
use domain_events::{Event, EventPayload};
use futures_util::StreamExt;
use health::Probe as _;
use infra_messaging::{
    ConsumerError, ConsumerOptions, Credentials, HandlerError, Messaging, MessagingOptions,
    PublishError, Registry, Route,
};
use tokio::sync::{Notify, oneshot};
use tokio::time::{Instant, timeout};
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[path = "../support/relay.rs"]
mod relay;

mod admission;
mod connection;
mod credentials;
mod dead_letter;
mod delivery;
mod durable;
mod probe;
mod publish;
use relay::{AckDroppingRelay, AuthObservingRelay, is_stream_publish_ack, relay_target};

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
        let client = async_nats::connect(nats_url())
            .await
            .expect("NATS_URL must point to the JetStream broker selected for this suite");
        Self::create_with_client(client, with_consumer, source_max_messages, dlq_max_messages).await
    }

    async fn create_with_authenticated_admin(
        url: &str,
        credentials: &std::path::Path,
    ) -> (Self, async_nats::Client) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let options = tokio::time::timeout_at(
            deadline,
            async_nats::ConnectOptions::with_credentials_file(credentials),
        )
        .await
        .expect("authenticated admin credentials admission stays bounded")
        .expect("runner-provided admin credentials parse");
        let client = tokio::time::timeout_at(deadline, options.connect(url))
            .await
            .expect("authenticated admin connection stays bounded")
            .expect("runner-owned authenticated broker accepts admin credentials");
        let fixture = tokio::time::timeout_at(
            deadline,
            Self::create_with_client(client.clone(), false, 10, 10),
        )
        .await
        .expect("authenticated fixture topology admission stays bounded");
        (fixture, client)
    }

    async fn create_with_client(
        client: async_nats::Client,
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
        root_ca_path: None,
        allow_plaintext: true,
        tls_first: false,
        source_stream: fixture.stream.clone(),
        max_payload_bytes,
        consumer,
    }
}

fn consumer_options(fixture: &Fixture) -> ConsumerOptions {
    ConsumerOptions {
        durable_name: fixture.durable.clone(),
        filter_subject: fixture.subject.clone(),
        dlq_subject: fixture.dlq_subject.clone(),
        dlq_stream: Some(fixture.dlq_stream.clone()),
        concurrency: std::num::NonZeroUsize::MIN,
    }
}

fn registry(fixture: &Fixture) -> Registry {
    Registry::try_new([Route::new::<ExampleEvent>(fixture.subject.clone())])
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
