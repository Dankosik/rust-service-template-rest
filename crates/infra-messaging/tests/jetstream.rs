#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration tests make failures and broker setup explicit"
)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_nats::jetstream::{self, consumer, stream};
use bytes::Bytes;
use domain_events::{Event, EventPayload};
use futures_util::StreamExt;
use infra_messaging::{
    ConsumerError, ConsumerOptions, HandlerError, Messaging, MessagingError, MessagingOptions,
    PublishError, Registry, Route,
};
use tokio::sync::{Notify, oneshot};
use tokio::time::{Instant, timeout};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct ExampleEvent {
    value: String,
}

impl EventPayload for ExampleEvent {
    const EVENT_TYPE: &'static str = "test.example.created";
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
                max_message_size: 1024 + 8 * 1024,
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
}

fn nats_url() -> String {
    std::env::var("NATS_URL")
        .expect("NATS_URL is required; run this suite through test-integration-messaging.sh")
}

struct AckDroppingRelay {
    url: String,
    dropped_ack: Arc<std::sync::atomic::AtomicBool>,
    task: JoinHandle<()>,
}

impl AckDroppingRelay {
    async fn start(stream: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test relay listener must bind an ephemeral loopback port");
        let address: SocketAddr = listener
            .local_addr()
            .expect("test relay listener must report its loopback address");
        let target = relay_target(&nats_url());
        let stream = stream.to_owned();
        let dropped_ack = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dropped_for_task = Arc::clone(&dropped_ack);
        let task = tokio::spawn(async move {
            let Ok((client, _)) = listener.accept().await else {
                return;
            };
            let Ok(broker) = TcpStream::connect(target).await else {
                return;
            };
            let _ = relay_connection(client, broker, stream, dropped_for_task).await;
        });
        Self {
            url: format!("nats://{address}"),
            dropped_ack,
            task,
        }
    }

    async fn join(self) {
        timeout(Duration::from_secs(3), self.task)
            .await
            .expect("relay task must finish after its client closes")
            .expect("relay task must not panic");
        assert!(
            self.dropped_ack.load(Ordering::SeqCst),
            "relay must drop a broker publication acknowledgement after dispatch"
        );
    }
}

fn relay_target(url: &str) -> String {
    let authority = url
        .strip_prefix("nats://")
        .expect("integration NATS_URL must use the nats scheme")
        .rsplit('@')
        .next()
        .expect("integration NATS_URL has an authority")
        .trim_end_matches('/');
    assert!(
        !authority.is_empty() && !authority.contains('/'),
        "integration NATS_URL must name one TCP broker address"
    );
    authority.to_owned()
}

async fn relay_connection(
    client: TcpStream,
    broker: TcpStream,
    stream: String,
    dropped_ack: Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), std::io::Error> {
    let (client_read, mut client_write) = client.into_split();
    let (broker_read, mut broker_write) = broker.into_split();
    let client_to_broker = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut BufReader::new(client_read), &mut broker_write).await;
    });
    let mut broker_read = BufReader::new(broker_read);
    let mut dropped = false;
    loop {
        let mut line = Vec::new();
        if broker_read.read_until(b'\n', &mut line).await? == 0 {
            break;
        }
        let Some(payload_length) = nats_payload_length(&line) else {
            client_write.write_all(&line).await?;
            continue;
        };
        let mut payload = vec![0; payload_length];
        broker_read.read_exact(&mut payload).await?;
        let mut ending = [0; 2];
        broker_read.read_exact(&mut ending).await?;
        if !dropped && is_stream_publish_ack(&payload, &stream) {
            dropped = true;
            dropped_ack.store(true, Ordering::SeqCst);
            continue;
        }
        client_write.write_all(&line).await?;
        client_write.write_all(&payload).await?;
        client_write.write_all(&ending).await?;
    }
    client_to_broker.abort();
    let _ = client_to_broker.await;
    Ok(())
}

fn nats_payload_length(line: &[u8]) -> Option<usize> {
    let frame = std::str::from_utf8(line).ok()?.trim_end();
    let mut fields = frame.split_ascii_whitespace();
    match fields.next()? {
        "MSG" | "HMSG" => fields.last()?.parse().ok(),
        _ => None,
    }
}

fn is_stream_publish_ack(payload: &[u8], stream: &str) -> bool {
    std::str::from_utf8(payload)
        .is_ok_and(|body| body.contains(&format!("\"stream\":\"{stream}\"")))
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn event(id: &str) -> Event<ExampleEvent> {
    event_with_value(id, "payload")
}

fn event_with_value(id: &str, value: &str) -> Event<ExampleEvent> {
    Event::new(
        id,
        time::OffsetDateTime::from_unix_timestamp(1_700_000_000)
            .expect("fixed event timestamp is valid"),
        ExampleEvent {
            value: value.to_owned(),
        },
    )
    .expect("fixed event is valid")
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
        servers,
        credentials: None,
        root_ca_path: None,
        allow_plaintext: true,
        allow_unauthenticated: true,
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
        .register::<ExampleEvent, _, _>(move |event, _| {
            if let Some(sender) = observed_tx
                .lock()
                .expect("test completion sender lock must not be poisoned")
                .take()
            {
                let _ = sender.send(event.id().to_owned());
            }
            async { Ok(()) }
        })
        .expect("fixture handler is registered");
    let handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = messaging
        .producer()
        .prepare(fixture.subject.clone(), &event("event-consumer"))
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

    handle
        .join(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn retryable_handler_is_redelivered_after_broker_nak_then_confirmed_acked() {
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
    let handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = messaging
        .producer()
        .prepare(fixture.subject.clone(), &event("event-retry"))
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
        .join(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
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
    let handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = messaging
        .producer()
        .prepare(fixture.subject.clone(), &event("event-permanent"))
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
        .join(deadline())
        .await
        .expect("terminal transfer worker can drain after source settlement");

    let source = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .expect("fixture source stream remains available")
        .get_raw_message(1)
        .await
        .expect("original source record remains observable for redrive");
    let record = infra_messaging::wire::DeadLetterRecord {
        subject: dead_letter.subject.to_string(),
        headers: dead_letter.headers.clone(),
        payload: dead_letter.payload.clone(),
        stream: fixture.stream.clone(),
        stream_sequence: source.sequence,
        stored_at: source.time,
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
    let handle = messaging
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
        .join(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn definite_dlq_refusal_leaves_the_permanent_source_delivery_unacknowledged() {
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
    let handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = messaging
        .producer()
        .prepare(fixture.subject.clone(), &event("event-dlq-refused"))
        .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("fixture event publication is acknowledged");

    let failure = timeout(Duration::from_secs(3), handle.failed())
        .await
        .expect("definite DLQ refusal must surface through the failure latch");
    assert!(matches!(failure, ConsumerError::DeadLetterRejected));
    wait_for_source_unacked(&fixture).await;
    assert!(matches!(
        handle.join(deadline()).await,
        Err(ConsumerError::DeadLetterRejected)
    ));

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
    let prepared = messaging
        .producer()
        .prepare(fixture.subject.clone(), &event("event-exhausted"))
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
    let handle = messaging
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
        .join(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn handler_panic_is_terminal_and_leaves_its_source_unacknowledged() {
    let fixture = Fixture::create(true).await;
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
        .register::<ExampleEvent, _, _>(|_, _| async { panic!("intentional handler panic") })
        .expect("fixture handler is registered");
    let handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = messaging
        .producer()
        .prepare(fixture.subject.clone(), &event("event-panic"))
        .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("fixture event publication is acknowledged");

    let failure = timeout(Duration::from_secs(3), handle.failed())
        .await
        .expect("handler panic must surface through the consumer failure latch");
    assert!(matches!(failure, ConsumerError::HandlerPanicked));
    wait_for_source_unacked(&fixture).await;
    assert!(matches!(
        handle.join(deadline()).await,
        Err(ConsumerError::HandlerPanicked)
    ));

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn oversized_source_message_is_terminal_without_invoking_the_handler() {
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
    let handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);

    let failure = timeout(Duration::from_secs(3), handle.failed())
        .await
        .expect("oversized source delivery must surface through the failure latch");
    assert!(matches!(failure, ConsumerError::SourceOversized));
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    wait_for_source_unacked(&fixture).await;
    assert!(matches!(
        handle.join(deadline()).await,
        Err(ConsumerError::SourceOversized)
    ));

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
    let handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = messaging
        .producer()
        .prepare(fixture.subject.clone(), &event("event-drain"))
        .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, deadline(), &cancel)
        .await
        .expect("fixture event publication is acknowledged");
    timeout(Duration::from_secs(3), started.notified())
        .await
        .expect("handler must begin before drain is requested");

    let outcome = handle.join(Instant::now() + Duration::from_secs(1)).await;
    assert!(matches!(outcome, Err(ConsumerError::DrainTimedOut)));
    wait_for_source_unacked(&fixture).await;

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn consumer_resident_delivery_bound_is_refused() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let invalid = ConsumerOptions {
        concurrency: 2,
        ..consumer_options(&fixture)
    };

    let result = Box::pin(Messaging::connect(
        options(&fixture, Some(invalid), 32 * 1024 * 1024),
        deadline(),
        cancel,
    ))
    .await;

    assert!(matches!(result, Err(MessagingError::Bounds)));
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

    let handle = messaging
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
    assert_eq!(actual.config.max_ack_pending, 1);
    handle
        .join(deadline())
        .await
        .expect("empty durable consumer drains within the shared deadline");
    close(messaging).await;
    fixture.cleanup().await;
}
