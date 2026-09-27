//! Real PostgreSQL and `JetStream` proof for transactional outbox publication.
//!
//! Each case provisions its own `JetStream` source stream and `SQLx` database. The
//! carrier drives the public outbox enqueue and registry APIs through the
//! production jobs engine; it does not replace broker acknowledgement, queue
//! transitions, or the publisher with a test double.

#![cfg(feature = "integration")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
// template:begin inbound-webhooks:outbox-test-messaging-outbox-inbound-imports
use std::time::{SystemTime, UNIX_EPOCH};
// template:end inbound-webhooks:outbox-test-messaging-outbox-inbound-imports

use async_nats::jetstream::{self, consumer, stream};
use domain_events::{Event, EventPayload};
use infra_jobs::{Engine, EnqueueError, Registry as JobsRegistry};
use infra_messaging::outbox::{OutboxEnqueueError, OutboxEnqueued};
use infra_messaging::{
    ConsumerOptions, HandlerError, Messaging, MessagingOptions, Registry, Route,
};
use infra_postgres::{Closed, Isolation, PgPool, PoolOptions, TxError, connection, in_tx};
// template:begin inbound-webhooks:outbox-test-messaging-outbox-inbound-imports-2
use infra_jobs::{JobError, Kinds, Policy};
use infra_webhooks::inbound::{
    Consumer as WebhookConsumer, Consumers, Incoming, Processor, Receiver,
};
use infra_webhooks::protocol::KeyRing;
// template:end inbound-webhooks:outbox-test-messaging-outbox-inbound-imports-2
use integration_tests::dsn_for;
use sqlx::Row;
use tokio::sync::Notify;
use tokio::time::{Instant, timeout};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

const WAIT: Duration = Duration::from_secs(10);
const CLOSE_BUDGET: Duration = Duration::from_secs(5);
const APP: &str = "integration-tests-outbox";
const OUTBOX_KIND: &str = "publish_domain_event";
const WEBHOOK_ENDPOINT: &str = "partner/a?#";
const WEBHOOK_KEY: &str = "whsec_d2ViaG9va19zZWNyZXQ=";

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct Created {
    value: String,
}

impl EventPayload for Created {
    const EVENT_TYPE: &'static str = "test.outbox.created";
    const SCHEMA_VERSION: u16 = 1;
}

struct Fixture {
    jetstream: jetstream::Context,
    stream: String,
    subject: String,
    durable: String,
    dlq_stream: String,
    dlq_subject: String,
    duplicate_window: Duration,
}

impl Fixture {
    async fn create() -> Self {
        Self::create_with_window(false, Duration::from_secs(60)).await
    }

    async fn create_with_consumer(with_consumer: bool) -> Self {
        Self::create_with_window(with_consumer, Duration::from_millis(100)).await
    }

    async fn create_with_window(with_consumer: bool, duplicate_window: Duration) -> Self {
        let suffix = format!(
            "{}_{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        );
        let stream = format!("TEST_OUTBOX_{suffix}");
        let subject = format!("test.outbox.{suffix}");
        let durable = format!("test_outbox_{suffix}");
        let dlq_stream = format!("TEST_OUTBOX_DLQ_{suffix}");
        let dlq_subject = format!("test.outbox.{suffix}.dlq");
        let client = async_nats::connect(nats_url())
            .await
            .expect("NATS_URL must select the integration JetStream broker");
        let jetstream = jetstream::new(client);
        create_source(&jetstream, &stream, &subject, duplicate_window).await;
        create_source(&jetstream, &dlq_stream, &dlq_subject, duplicate_window).await;
        if with_consumer {
            jetstream
                .get_stream(&stream)
                .await
                .expect("source stream is available for durable setup")
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
                .expect("test administrator provisions durable consumer");
        }
        Self {
            jetstream,
            stream,
            subject,
            durable,
            dlq_stream,
            dlq_subject,
            duplicate_window,
        }
    }

    async fn remove_source(&self) {
        self.jetstream
            .delete_stream(&self.stream)
            .await
            .expect("test administrator removes the source stream");
    }

    async fn restore_source(&self) {
        create_source(
            &self.jetstream,
            &self.stream,
            &self.subject,
            self.duplicate_window,
        )
        .await;
    }

    async fn published(&self) -> u64 {
        self.jetstream
            .get_stream(&self.stream)
            .await
            .expect("source stream remains observable")
            .info()
            .await
            .expect("source stream info is observable")
            .state
            .messages
    }

    async fn cleanup(self) {
        self.jetstream
            .delete_stream(&self.stream)
            .await
            .expect("test fixture source stream is removable");
        self.jetstream
            .delete_stream(&self.dlq_stream)
            .await
            .expect("test fixture DLQ stream is removable");
    }
}

async fn create_source(
    jetstream: &jetstream::Context,
    stream: &str,
    subject: &str,
    duplicate_window: Duration,
) {
    jetstream
        .create_stream(stream::Config {
            name: stream.to_owned(),
            subjects: vec![subject.to_owned()],
            max_message_size: 1024 + 8 * 1024,
            duplicate_window,
            ..Default::default()
        })
        .await
        .expect("test administrator creates a source stream");
}

fn nats_url() -> String {
    std::env::var("NATS_URL")
        .expect("NATS_URL is required; run this carrier through the integration runner")
}

fn event(id: &str, value: &str) -> Event<Created> {
    Event::new(
        id,
        time::OffsetDateTime::from_unix_timestamp(1_700_000_000)
            .expect("fixed occurrence is valid"),
        Created {
            value: value.to_owned(),
        },
    )
    .expect("fixture event is valid")
}

fn messaging_options(fixture: &Fixture) -> MessagingOptions {
    messaging_options_with_servers(fixture, vec![nats_url()])
}

fn messaging_options_with_servers(fixture: &Fixture, servers: Vec<String>) -> MessagingOptions {
    MessagingOptions {
        servers,
        credentials: None,
        root_ca_path: None,
        allow_plaintext: true,
        allow_unauthenticated: true,
        source_stream: fixture.stream.clone(),
        dlq_stream: None,
        max_payload_bytes: 1024,
        consumer: None,
    }
}

fn consumer_options(fixture: &Fixture) -> MessagingOptions {
    let mut options = messaging_options(fixture);
    options.dlq_stream = Some(fixture.dlq_stream.clone());
    options.consumer = Some(ConsumerOptions {
        durable_name: fixture.durable.clone(),
        filter_subject: fixture.subject.clone(),
        dlq_subject: fixture.dlq_subject.clone(),
        concurrency: 1,
    });
    options
}

struct AckDroppingRelay {
    url: String,
    dropped_ack: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl AckDroppingRelay {
    async fn start(stream: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay binds an ephemeral loopback port");
        let address: SocketAddr = listener
            .local_addr()
            .expect("relay reports its loopback address");
        let stream = stream.to_owned();
        let dropped_ack = Arc::new(AtomicBool::new(false));
        let dropped_for_task = Arc::clone(&dropped_ack);
        let task = tokio::spawn(async move {
            let Ok((client, _)) = listener.accept().await else {
                return;
            };
            let Ok(broker) = TcpStream::connect(relay_target(&nats_url())).await else {
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
            .expect("relay finishes after its client closes")
            .expect("relay does not panic");
        assert!(
            self.dropped_ack.load(Ordering::SeqCst),
            "relay drops exactly the broker publication acknowledgement"
        );
    }
}

fn relay_target(url: &str) -> String {
    let authority = url
        .strip_prefix("nats://")
        .expect("integration NATS URL uses nats scheme")
        .rsplit('@')
        .next()
        .expect("integration NATS URL has authority")
        .trim_end_matches('/');
    assert!(
        !authority.is_empty() && !authority.contains('/'),
        "integration NATS URL identifies one broker address"
    );
    authority.to_owned()
}

async fn relay_connection(
    client: TcpStream,
    broker: TcpStream,
    stream: String,
    dropped_ack: Arc<AtomicBool>,
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

fn routes(fixture: &Fixture) -> Registry {
    Registry::new([Route::new::<Created>(fixture.subject.clone())])
        .expect("fixture publication route is valid")
}

async fn template_pool(pool: &PgPool, max_connections: u32) -> PgPool {
    infra_postgres::connect(
        &dsn_for(pool).await,
        &PoolOptions {
            max_connections: NonZeroU32::new(max_connections).expect("pool has capacity"),
            application_name: APP,
            default_isolation: Isolation::ReadCommitted,
        },
    )
    .await
    .expect("template engine pool connects")
}

struct EngineRun {
    started: infra_jobs::Started,
    tracker: TaskTracker,
    cancel: CancellationToken,
}

async fn start(pool: &PgPool, registry: JobsRegistry) -> EngineRun {
    let engine = Engine::new(
        pool.clone(),
        registry,
        NonZeroU32::new(1).expect("one publisher slot"),
    );
    engine
        .check_startup()
        .await
        .expect("outbox engine admits the shared jobs store");
    let tracker = TaskTracker::new();
    let cancel = CancellationToken::new();
    let started = engine.start(&tracker, &cancel);
    EngineRun {
        started,
        tracker,
        cancel,
    }
}

async fn finish(run: EngineRun, pools: &[&PgPool]) {
    run.started.stop_claiming();
    timeout(WAIT, run.started.drained())
        .await
        .expect("the engine drains");
    run.cancel.cancel();
    run.tracker.close();
    timeout(WAIT, run.tracker.wait())
        .await
        .expect("engine background tasks join");
    for pool in pools {
        assert_eq!(
            infra_postgres::close(pool, CLOSE_BUDGET).await,
            Closed::Complete
        );
    }
}

async fn close(messaging: Messaging) {
    let cancel = CancellationToken::new();
    assert_eq!(
        messaging
            .close(Instant::now() + CLOSE_BUDGET, &cancel)
            .await,
        infra_messaging::CloseOutcome::Complete
    );
}

async fn until<T>(what: &str, mut check: impl AsyncFnMut() -> Option<T>) -> T {
    timeout(WAIT, async {
        loop {
            if let Some(value) = check().await {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} did not happen within {WAIT:?}"))
}

async fn job(pool: &PgPool, key: &str) -> JobRow {
    let row = sqlx::query(
        "SELECT id::text AS id, state, attempts, claim_generation, failure_reason, \
         EXTRACT(EPOCH FROM (not_before - clock_timestamp()))::double precision AS snooze_delay_seconds \
         FROM background_jobs WHERE kind = $1 AND unique_key = $2",
    )
    .bind(OUTBOX_KIND)
    .bind(key)
    .fetch_one(pool)
    .await
    .expect("outbox job is stored");
    JobRow {
        id: row.try_get("id").expect("job id"),
        state: row.try_get("state").expect("job state"),
        attempts: row.try_get("attempts").expect("job attempts"),
        claim_generation: row.try_get("claim_generation").expect("claim generation"),
        failure_reason: row.try_get("failure_reason").expect("failure reason"),
        snooze_delay_seconds: row.try_get("snooze_delay_seconds").expect("snooze delay"),
    }
}

struct JobRow {
    id: String,
    state: String,
    attempts: i16,
    claim_generation: i64,
    failure_reason: Option<String>,
    snooze_delay_seconds: f64,
}

async fn outbox_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM background_jobs WHERE kind = $1")
        .bind(OUTBOX_KIND)
        .fetch_one(pool)
        .await
        .expect("outbox row count")
}

fn prepared(fixture: &Fixture, id: &str, value: &str) -> infra_messaging::PreparedEvent {
    routes(fixture)
        .prepare(&event(id, value), 1024)
        .expect("fixture event prepares once")
}

#[derive(Debug)]
enum Step {
    Refused,
    Outbox(OutboxEnqueueError),
    Jobs(EnqueueError),
    Query(sqlx::Error),
    Transaction(TxError),
}

impl From<OutboxEnqueueError> for Step {
    fn from(error: OutboxEnqueueError) -> Self {
        Self::Outbox(error)
    }
}

impl From<sqlx::Error> for Step {
    fn from(error: sqlx::Error) -> Self {
        Self::Query(error)
    }
}

impl From<EnqueueError> for Step {
    fn from(error: EnqueueError) -> Self {
        Self::Jobs(error)
    }
}

impl From<TxError> for Step {
    fn from(error: TxError) -> Self {
        Self::Transaction(error)
    }
}

impl std::fmt::Display for Step {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused => formatter.write_str("caller refused the transaction"),
            Self::Outbox(error) => write!(formatter, "outbox: {error}"),
            Self::Jobs(error) => write!(formatter, "jobs: {error}"),
            Self::Query(error) => write!(formatter, "query: {error}"),
            Self::Transaction(error) => write!(formatter, "transaction: {error}"),
        }
    }
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn business_rollback_hides_intent_commit_publishes_exact_prepared_event(pool: PgPool) {
    let fixture = Fixture::create().await;
    let messaging = Box::pin(Messaging::connect(
        messaging_options(&fixture),
        Instant::now() + WAIT,
        CancellationToken::new(),
    ))
    .await
    .expect("producer-only messaging admits the provisioned stream");
    let business = template_pool(&pool, 3).await;
    sqlx::query("CREATE TABLE business_effects (id text PRIMARY KEY)")
        .execute(&business)
        .await
        .expect("business table");

    let rolled_back = prepared(&fixture, "event-rollback", "rollback");
    let rollback = in_tx(&business, async |tx| -> Result<(), Step> {
        sqlx::query("INSERT INTO business_effects (id) VALUES ('rollback')")
            .execute(connection(tx))
            .await?;
        assert_eq!(rolled_back.enqueue(tx).await?, OutboxEnqueued::Created);
        Err(Step::Refused)
    })
    .await;
    assert!(matches!(rollback, Err(Step::Refused)));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM business_effects")
            .fetch_one(&business)
            .await
            .expect("business effects after rollback"),
        0
    );
    assert_eq!(outbox_count(&business).await, 0);

    let committed = prepared(&fixture, "event-commit", "committed");
    let committed_outcome = in_tx(&business, async |tx| -> Result<_, Step> {
        sqlx::query("INSERT INTO business_effects (id) VALUES ('commit')")
            .execute(connection(tx))
            .await?;
        committed.enqueue(tx).await.map_err(Step::from)
    })
    .await
    .expect("business transaction commits alongside publication intent");
    assert_eq!(committed_outcome, OutboxEnqueued::Created);
    assert_eq!(outbox_count(&business).await, 1);

    let publisher = template_pool(&pool, 3).await;
    let run = start(
        &publisher,
        infra_messaging::outbox::registry(messaging.producer())
            .expect("outbox publisher registry is valid"),
    )
    .await;
    until(
        "committed outbox event is synchronously acknowledged by JetStream",
        async || (fixture.published().await == 1).then_some(()),
    )
    .await;
    let row = until("acknowledged publication is fenced complete", async || {
        let row = job(&business, &event_key("event-commit")).await;
        (row.state == "completed").then_some(row)
    })
    .await;
    assert_eq!(row.attempts, 1);
    assert!(row.failure_reason.is_none());
    let stored = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .expect("source stream remains observable")
        .get_raw_message(1)
        .await
        .expect("acknowledged publication is retained by JetStream");
    assert_eq!(stored.subject.as_ref(), committed.subject());
    assert_eq!(stored.payload.as_ref(), committed.payload().as_ref());
    assert_eq!(
        stored.headers.get("Message-Id").map(ToString::to_string),
        Some(committed.message_id().to_owned())
    );
    assert_eq!(
        stored.headers.get("Nats-Msg-Id").map(ToString::to_string),
        Some(committed.publication_id().to_owned())
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM business_effects WHERE id = 'commit'")
            .fetch_one(&business)
            .await
            .expect("committed business effect"),
        1
    );

    finish(run, &[&publisher, &business]).await;
    close(messaging).await;
    fixture.cleanup().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn live_event_identity_is_idempotent_only_for_equal_immutable_intent(pool: PgPool) {
    let fixture = Fixture::create().await;
    let jobs = template_pool(&pool, 3).await;
    let competing = template_pool(&pool, 3).await;
    let first = prepared(&fixture, "event-identity", "same");
    let equal = prepared(&fixture, "event-identity", "same");

    assert_eq!(
        in_tx(&jobs, async |tx| -> Result<_, Step> {
            first.enqueue(tx).await.map_err(Step::from)
        })
        .await
        .expect("first immutable intent commits"),
        OutboxEnqueued::Created
    );
    assert_eq!(
        in_tx(&jobs, async |tx| -> Result<_, Step> {
            equal.enqueue(tx).await.map_err(Step::from)
        })
        .await
        .expect("same immutable intent is accepted while its key is live"),
        OutboxEnqueued::Duplicate
    );
    let held = prepared(&fixture, "event-concurrent-conflict", "first");
    let conflicting = prepared(&fixture, "event-concurrent-conflict", "second");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let first_pool = jobs.clone();
    let first_entered = Arc::clone(&entered);
    let first_release = Arc::clone(&release);
    let first_task = tokio::spawn(async move {
        in_tx(&first_pool, async |tx| -> Result<_, Step> {
            let result = held.enqueue(tx).await?;
            first_entered.notify_one();
            first_release.notified().await;
            Ok(result)
        })
        .await
    });
    timeout(WAIT, entered.notified())
        .await
        .expect("the first concurrent immutable intent enters its transaction");
    let second_pool = competing.clone();
    let second_task = tokio::spawn(async move {
        in_tx(&second_pool, async |tx| -> Result<_, Step> {
            conflicting.enqueue(tx).await.map_err(Step::from)
        })
        .await
    });
    until(
        "the conflicting insert waits for the live unique key",
        async || {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity \
             WHERE datname = current_database() AND application_name = $1 \
               AND wait_event_type = 'Lock' AND pid <> pg_backend_pid()",
            )
            .bind(APP)
            .fetch_one(&jobs)
            .await
            .expect("concurrent lock observation");
            (waiting > 0).then_some(())
        },
    )
    .await;
    release.notify_one();
    assert_eq!(
        timeout(WAIT, first_task)
            .await
            .expect("first concurrent transaction joins")
            .expect("first concurrent task does not panic")
            .expect("first immutable intent commits"),
        OutboxEnqueued::Created
    );
    assert!(matches!(
        timeout(WAIT, second_task)
            .await
            .expect("conflicting transaction joins")
            .expect("conflicting task does not panic"),
        Err(Step::Outbox(OutboxEnqueueError::EventIdConflict))
    ));
    assert_eq!(outbox_count(&jobs).await, 2);

    close_pool(&competing).await;
    close_pool(&jobs).await;
    fixture.cleanup().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn lost_broker_ack_snoozes_then_republishes_the_same_immutable_identity(pool: PgPool) {
    let fixture = Fixture::create().await;
    let relay = AckDroppingRelay::start(&fixture.stream).await;
    let first_messaging = Box::pin(Messaging::connect(
        messaging_options_with_servers(&fixture, vec![relay.url.clone()]),
        Instant::now() + WAIT,
        CancellationToken::new(),
    ))
    .await
    .expect("producer-only messaging admits through the transparent relay");
    let jobs = template_pool(&pool, 3).await;
    let key = event_key("event-lost-ack");
    let intent = prepared(&fixture, "event-lost-ack", "ambiguous");
    assert_eq!(
        in_tx(&jobs, async |tx| -> Result<_, Step> {
            intent.enqueue(tx).await.map_err(Step::from)
        })
        .await
        .expect("outbox intent commits before the ambiguous acknowledgement"),
        OutboxEnqueued::Created
    );
    let first_pool = template_pool(&pool, 3).await;
    let first_run = start(
        &first_pool,
        infra_messaging::outbox::registry(first_messaging.producer())
            .expect("outbox publisher registry is valid"),
    )
    .await;
    let snoozed = until(
        "lost broker ACK retains a pending outbox intent",
        async || {
            let row = job(&jobs, &key).await;
            (relay.dropped_ack.load(Ordering::SeqCst)
                && row.state == "pending"
                && row.attempts == 0
                && row.claim_generation > 0
                && row.snooze_delay_seconds > 20.0)
                .then_some(row)
        },
    )
    .await;
    assert_eq!(fixture.published().await, 1);
    let stored = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .expect("source stream remains observable")
        .get_raw_message(1)
        .await
        .expect("ambiguous dispatch remains stored");
    assert_eq!(
        stored.headers.get("Nats-Msg-Id").map(ToString::to_string),
        Some("event-lost-ack".to_owned())
    );
    finish(first_run, &[&first_pool]).await;
    close(first_messaging).await;
    relay.join().await;

    sqlx::query("UPDATE background_jobs SET not_before = statement_timestamp() - interval '1 second' WHERE id::text = $1")
        .bind(&snoozed.id)
        .execute(&jobs)
        .await
        .expect("ambiguous intent is made due for its same-ID retry");
    let retry_messaging = Box::pin(Messaging::connect(
        messaging_options(&fixture),
        Instant::now() + WAIT,
        CancellationToken::new(),
    ))
    .await
    .expect("direct producer reconnects after the relay closes");
    let retry_pool = template_pool(&pool, 3).await;
    let retry_run = start(
        &retry_pool,
        infra_messaging::outbox::registry(retry_messaging.producer())
            .expect("retry publisher registry is valid"),
    )
    .await;
    let completed = until(
        "same immutable identity receives duplicate broker ACK and completes",
        async || {
            let row = job(&jobs, &key).await;
            (row.state == "completed").then_some(row)
        },
    )
    .await;
    assert_eq!(completed.attempts, 1);
    assert_eq!(fixture.published().await, 1);

    finish(retry_run, &[&retry_pool, &jobs]).await;
    close(retry_messaging).await;
    fixture.cleanup().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn final_attempt_topology_refusal_snoozes_then_recovery_keeps_publication_identity(
    pool: PgPool,
) {
    let fixture = Fixture::create().await;
    let messaging = Box::pin(Messaging::connect(
        messaging_options(&fixture),
        Instant::now() + WAIT,
        CancellationToken::new(),
    ))
    .await
    .expect("producer-only messaging admits before the controlled outage");
    let jobs = template_pool(&pool, 3).await;
    let intent = prepared(&fixture, "event-recovery", "recoverable");
    assert_eq!(
        in_tx(&jobs, async |tx| -> Result<_, Step> {
            intent.enqueue(tx).await.map_err(Step::from)
        })
        .await
        .expect("outbox intent commits"),
        OutboxEnqueued::Created
    );
    let key = event_key("event-recovery");
    sqlx::query("UPDATE background_jobs SET attempts = 24 WHERE kind = $1 AND unique_key = $2")
        .bind(OUTBOX_KIND)
        .bind(&key)
        .execute(&jobs)
        .await
        .expect("the controlled final-attempt setup applies");
    fixture.remove_source().await;

    let publisher = template_pool(&pool, 3).await;
    let run = start(
        &publisher,
        infra_messaging::outbox::registry(messaging.producer())
            .expect("outbox publisher registry is valid"),
    )
    .await;
    let snoozed = until(
        "recoverable topology refusal snoozes and refunds the final attempt",
        async || {
            let row = job(&jobs, &key).await;
            (row.state == "pending"
                && row.attempts == 24
                && row.claim_generation > 0
                && row.snooze_delay_seconds > 20.0)
                .then_some(row)
        },
    )
    .await;
    assert!(snoozed.failure_reason.is_none());
    sqlx::query("UPDATE background_jobs SET not_before = statement_timestamp() - interval '1 second' WHERE id::text = $1")
        .bind(&snoozed.id).execute(&jobs).await.expect("second outage attempt becomes due");
    let repeated = until(
        "repeated outage snoozes and refunds the final attempt again",
        async || {
            let row = job(&jobs, &key).await;
            (row.state == "pending"
                && row.attempts == 24
                && row.claim_generation > snoozed.claim_generation
                && row.snooze_delay_seconds > 20.0)
                .then_some(row)
        },
    )
    .await;
    fixture.restore_source().await;
    sqlx::query("UPDATE background_jobs SET not_before = statement_timestamp() - interval '1 second' WHERE id::text = $1")
        .bind(&repeated.id)
        .execute(&jobs)
        .await
        .expect("recovered intent is made due without changing its identity");
    until("recovered immutable intent is published", async || {
        (fixture.published().await == 1).then_some(())
    })
    .await;
    let completed = until(
        "recovered outbox job completes after broker ACK",
        async || {
            let row = job(&jobs, &key).await;
            (row.state == "completed").then_some(row)
        },
    )
    .await;
    assert_eq!(completed.attempts, 25);

    finish(run, &[&publisher, &jobs]).await;
    close(messaging).await;
    fixture.cleanup().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn malformed_stored_intent_is_terminal_and_never_false_completion(pool: PgPool) {
    let fixture = Fixture::create().await;
    let messaging = Box::pin(Messaging::connect(
        messaging_options(&fixture),
        Instant::now() + WAIT,
        CancellationToken::new(),
    ))
    .await
    .expect("producer-only messaging admits the fixture stream");
    let jobs = template_pool(&pool, 3).await;
    let logical_id = "event-malformed";
    let key = event_key(logical_id);
    let intent = prepared(&fixture, logical_id, "stored before corruption");
    assert_eq!(
        in_tx(&jobs, async |tx| -> Result<_, Step> {
            intent.enqueue(tx).await.map_err(Step::from)
        })
        .await
        .expect("canonical outbox intent commits"),
        OutboxEnqueued::Created
    );
    let corrupted = sqlx::query(
        "UPDATE background_jobs \
         SET payload = jsonb_set(payload, '{payload_base64}', to_jsonb('not-base64'::text), false) \
         WHERE kind = $1 AND unique_key = $2",
    )
    .bind(OUTBOX_KIND)
    .bind(&key)
    .execute(&jobs)
    .await
    .expect("only the stored payload encoding is corrupted");
    assert_eq!(corrupted.rows_affected(), 1);

    let publisher = template_pool(&pool, 3).await;
    let run = start(
        &publisher,
        infra_messaging::outbox::registry(messaging.producer())
            .expect("outbox publisher registry is valid"),
    )
    .await;
    let failed = until(
        "malformed intent becomes visible terminal history",
        async || {
            let row = job(&jobs, &key).await;
            (row.state == "failed").then_some(row)
        },
    )
    .await;
    assert_eq!(failed.failure_reason.as_deref(), Some("permanent"));
    assert_eq!(fixture.published().await, 0);

    finish(run, &[&publisher, &jobs]).await;
    close(messaging).await;
    fixture.cleanup().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn durable_consumer_effect_dedupes_same_logical_id_after_broker_window(pool: PgPool) {
    let fixture = Fixture::create_with_consumer(true).await;
    let messaging = Box::pin(Messaging::connect(
        consumer_options(&fixture),
        Instant::now() + WAIT,
        CancellationToken::new(),
    ))
    .await
    .expect("messaging consumer topology admits");
    let effects = template_pool(&pool, 3).await;
    sqlx::query("CREATE TABLE messaging_effects (logical_id text PRIMARY KEY)")
        .execute(&effects)
        .await
        .expect("durable effect table");
    let invoked = Arc::new(AtomicUsize::new(0));
    let invoked_handler = Arc::clone(&invoked);
    let effect_pool = effects.clone();
    let mut registry = routes(&fixture);
    registry
        .register::<Created, _, _>(move |event, _| {
            let pool = effect_pool.clone();
            let invoked = Arc::clone(&invoked_handler);
            let logical_id = event.id().to_owned();
            async move {
                sqlx::query(
                    "INSERT INTO messaging_effects (logical_id) VALUES ($1) ON CONFLICT DO NOTHING",
                )
                .bind(logical_id)
                .execute(&pool)
                .await
                .map_err(|_| HandlerError::Retryable)?;
                invoked.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .expect("consumer handler registers");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("consumer admits")
        .start(&CancellationToken::new());
    let event = prepared(&fixture, "event-durable-effect", "same");
    messaging
        .producer()
        .publish(&event, Instant::now() + WAIT, &CancellationToken::new())
        .await
        .expect("first publication ACKs");
    until("first durable effect commits", async || {
        let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM messaging_effects")
            .fetch_one(&effects)
            .await
            .expect("effect count");
        (effects == 1).then_some(())
    })
    .await;
    tokio::time::sleep(fixture.duplicate_window + Duration::from_millis(20)).await;
    messaging
        .producer()
        .publish(&event, Instant::now() + WAIT, &CancellationToken::new())
        .await
        .expect("same-ID publication after broker window ACKs");
    until(
        "second broker delivery reaches the real consumer",
        async || (invoked.load(Ordering::SeqCst) >= 2).then_some(()),
    )
    .await;
    assert_eq!(fixture.published().await, 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM messaging_effects")
            .fetch_one(&effects)
            .await
            .expect("deduped effect count"),
        1
    );
    handle.drain();
    handle
        .finish(Instant::now() + CLOSE_BUDGET)
        .await
        .expect("consumer drains");
    close(messaging).await;
    close_pool(&effects).await;
    fixture.cleanup().await;
}

// template:begin inbound-webhooks:outbox-test-messaging-outbox-inbound-fixture
fn webhook_receiver(pool: PgPool) -> Receiver {
    Receiver::new(
        pool,
        [(
            WEBHOOK_ENDPOINT.to_owned(),
            KeyRing::from_encoded(WEBHOOK_KEY, None).expect("fixture key"),
        )],
    )
}

fn webhook_headers(message_id: &str, body: &[u8]) -> http::HeaderMap {
    let timestamp: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
        .try_into()
        .expect("timestamp fits i64");
    let keys = KeyRing::from_encoded(WEBHOOK_KEY, None).expect("fixture key");
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "webhook-id",
        http::HeaderValue::from_str(message_id).expect("message id"),
    );
    headers.insert(
        "webhook-timestamp",
        http::HeaderValue::from_str(&timestamp.to_string()).expect("timestamp"),
    );
    headers.insert(
        "webhook-signature",
        http::HeaderValue::from_str(
            &keys
                .signatures(message_id.as_bytes(), timestamp, body)
                .expect("signature"),
        )
        .expect("signature header"),
    );
    headers
}

#[derive(Default)]
struct HeldWebhookConsumer {
    entered: Notify,
    release: Notify,
}

impl WebhookConsumer for HeldWebhookConsumer {
    fn process<'a>(
        &'a self,
        tx: &'a mut infra_postgres::Tx<'_>,
        incoming: &'a Incoming,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), JobError>> + Send + 'a>>
    {
        Box::pin(async move {
            sqlx::query("INSERT INTO webhook_effects (message_id) VALUES ($1)")
                .bind(incoming.message_id())
                .execute(connection(tx))
                .await
                .map_err(JobError::from)?;
            self.entered.notify_one();
            self.release.notified().await;
            Ok(())
        })
    }
}
// template:end inbound-webhooks:outbox-test-messaging-outbox-inbound-fixture

// template:begin inbound-webhooks:outbox-test-messaging-outbox-webhook-capacity
#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn dedicated_publisher_progresses_while_real_inbound_webhook_processing_slot_is_occupied(
    pool: PgPool,
) {
    let fixture = Fixture::create().await;
    let messaging = Box::pin(Messaging::connect(
        messaging_options(&fixture),
        Instant::now() + WAIT,
        CancellationToken::new(),
    ))
    .await
    .expect("producer-only messaging admits the fixture stream");
    let shared_pool = template_pool(&pool, 6).await;
    sqlx::query("CREATE TABLE webhook_effects (message_id bytea NOT NULL)")
        .execute(&shared_pool)
        .await
        .expect("webhook effect table");
    let held = Arc::new(HeldWebhookConsumer::default());
    let mut consumers = Consumers::new();
    assert!(
        consumers
            .insert(
                WEBHOOK_ENDPOINT,
                Arc::clone(&held) as Arc<dyn WebhookConsumer>
            )
            .is_none()
    );
    let mut webhook_kinds = Kinds::new();
    webhook_kinds.register(Policy::default(), Processor::new(consumers));
    let webhook = start(
        &shared_pool,
        webhook_kinds.validate().expect("webhook registry is valid"),
    )
    .await;
    let body = b"{\"process\":true}";
    assert!(
        webhook_receiver(shared_pool.clone())
            .receive(
                WEBHOOK_ENDPOINT,
                &webhook_headers("outbox-capacity", body),
                body,
                SystemTime::now(),
            )
            .await
            .is_ok()
    );
    timeout(WAIT, held.entered.notified())
        .await
        .expect("real webhook processor occupies its slot");

    let intent = prepared(&fixture, "event-independent-capacity", "published");
    assert_eq!(
        in_tx(&shared_pool, async |tx| -> Result<_, Step> {
            intent.enqueue(tx).await.map_err(Step::from)
        })
        .await
        .expect("outbox intent commits while real webhook work occupies its engine"),
        OutboxEnqueued::Created
    );
    let publisher = start(
        &shared_pool,
        infra_messaging::outbox::registry(messaging.producer())
            .expect("dedicated publisher registry is valid"),
    )
    .await;
    until(
        "dedicated publisher receives the broker ACK despite ordinary saturation",
        async || (fixture.published().await == 1).then_some(()),
    )
    .await;

    publisher.started.stop_claiming();
    finish(publisher, &[]).await;
    held.release.notify_one();
    finish(webhook, &[&shared_pool]).await;
    close(messaging).await;
    fixture.cleanup().await;
}
// template:end inbound-webhooks:outbox-test-messaging-outbox-webhook-capacity

async fn close_pool(pool: &PgPool) {
    assert_eq!(
        infra_postgres::close(pool, CLOSE_BUDGET).await,
        Closed::Complete
    );
}

fn event_key(id: &str) -> String {
    use sha2::{Digest as _, Sha256};

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut key = String::with_capacity(70);
    key.push_str("event-");
    for byte in Sha256::digest(id.as_bytes()) {
        key.push(char::from(HEX[usize::from(byte >> 4)]));
        key.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    key
}
