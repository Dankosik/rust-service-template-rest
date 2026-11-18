#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "a broken fixture or setup fails the test"
)]

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
// template:begin inbound-webhooks:outbox-test-messaging-outbox-inbound-imports
use std::time::SystemTime;
// template:end inbound-webhooks:outbox-test-messaging-outbox-inbound-imports

use async_nats::jetstream::{self, consumer, stream};
use domain_events::{Event, EventPayload};
use infra_jobs::{Engine, EnqueueError, Kinds};
use infra_messaging::outbox::{OutboxEnqueueError, OutboxEnqueued};
use infra_messaging::wire::{DeadLetterRecord, restore_dead_letter};
use infra_messaging::{
    ConsumerOptions, EnvelopeError, HandlerError, Messaging, MessagingError, MessagingOptions,
    Registry, Route,
};
use infra_postgres::{Isolation, PgPool, TxError, in_tx};
use operation_context::{Deadline, OperationContext};
// template:begin inbound-webhooks:outbox-test-messaging-outbox-inbound-imports-2
use infra_jobs::{JobError, KindsBuilder};
use infra_webhooks::inbound::{
    Consumer as WebhookConsumer, Consumers, Incoming, Processor, Receiver, Verifier,
};
use infra_webhooks::protocol::KeyRing;
// template:end inbound-webhooks:outbox-test-messaging-outbox-inbound-imports-2
use integration_tests::{close_pools, dsn_for, poll_until};
use nats_test_support::{AckDroppingRelay, DroppedAck};
use sqlx::Row;
use tokio::sync::Notify;
use tokio::time::{Instant, timeout};
use tokio_util::sync::{CancellationToken, DropGuard};
use tokio_util::task::TaskTracker;

#[path = "messaging_outbox/upgrade.rs"]
mod upgrade;

const WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);
const CLOSE_BUDGET: Duration = Duration::from_secs(5);
const APP: &str = "integration-tests-outbox";
const PUBLISHER_ID: uuid::NonNilUuid = uuid::NonNilUuid::new(uuid::Uuid::from_u128(
    0x18ec64bc_9693_465e_a5dc_8f61177b89e2,
))
.unwrap();
const OUTBOX_KIND: &str = "publish_domain_event";
// template:begin inbound-webhooks:outbox-test-messaging-outbox-inbound-constants
const WEBHOOK_ENDPOINT: &str = "partner/a?#";
const WEBHOOK_KEY: &str = "whsec_Q0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0M=";
// template:end inbound-webhooks:outbox-test-messaging-outbox-inbound-constants

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
    route: Route<Created>,
    publication: infra_messaging::PublicationRoute<Created>,
    registry: Registry,
    durable: String,
    dlq_stream: String,
    dlq_subject: String,
    duplicate_window: Duration,
}

impl Fixture {
    async fn create() -> Self {
        Self::create_with_window(false, Duration::from_mins(1)).await
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
        jetstream
            .create_stream(stream::Config {
                name: dlq_stream.clone(),
                subjects: vec![dlq_subject.clone()],
                max_message_size: 16 * 1024,
                duplicate_window,
                ..Default::default()
            })
            .await
            .expect("test administrator creates the transfer-sized DLQ");
        let route = Route::<Created>::new(subject.clone());
        let mut registry = Registry::default();
        let publication = registry
            .declare_publication(&route, 1024)
            .expect("fixture publication route is valid");
        if with_consumer {
            let mut handled = registry.clone();
            handled.register(&route, |_, _| async { Ok(()) }).unwrap();
            jetstream
                .get_stream(&stream)
                .await
                .expect("source stream is available for durable setup")
                .create_consumer(consumer::pull::Config {
                    name: Some(durable.clone()),
                    durable_name: Some(durable.clone()),
                    filter_subject: subject.clone(),
                    metadata: handled.durable_metadata(&dlq_stream, &dlq_subject),
                    ack_wait: Duration::from_secs(46),
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
            route,
            publication,
            registry,
            durable,
            dlq_stream,
            dlq_subject,
            duplicate_window,
        }
    }

    fn connect_producer(&self) -> impl Future<Output = Result<Messaging, MessagingError>> {
        Box::pin(Messaging::connect(
            messaging_options(self),
            Instant::now() + WAIT,
            CancellationToken::new(),
        ))
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
    Event {
        id: id.to_owned(),
        occurred_at: time::UtcDateTime::from_unix_timestamp(1_700_000_000)
            .expect("fixed occurrence is valid"),
        payload: Created {
            value: value.to_owned(),
        },
    }
}

fn messaging_options(fixture: &Fixture) -> MessagingOptions {
    messaging_options_with_servers(fixture, vec![nats_url()])
}

fn messaging_options_with_servers(fixture: &Fixture, servers: Vec<String>) -> MessagingOptions {
    MessagingOptions {
        connection_name: "integration-tests".to_owned(),
        servers,
        credentials: None,
        root_ca_path: None,
        allow_plaintext: true,
        tls_first: false,
        source_stream: fixture.stream.clone(),
        max_payload_bytes: 1024,
        consumer: None,
    }
}

fn consumer_options(fixture: &Fixture, durable: &str) -> MessagingOptions {
    let mut options = messaging_options(fixture);
    options.consumer = Some(ConsumerOptions {
        durable_name: durable.to_owned(),
        filter_subject: fixture.subject.clone(),
        dlq_subject: fixture.dlq_subject.clone(),
        dlq_stream: Some(fixture.dlq_stream.clone()),
        concurrency: std::num::NonZeroUsize::MIN,
    });
    options
}

async fn template_pool(pool: &PgPool, max_connections: u32) -> PgPool {
    integration_tests::template_pool(
        &dsn_for(pool).await,
        APP,
        max_connections,
        Isolation::ReadCommitted,
    )
    .await
}

struct EngineRun {
    started: infra_jobs::Started,
    tracker: TaskTracker,
    cancel: CancellationToken,
    _cancel_guard: DropGuard,
}

async fn start(pool: &PgPool, registry: Kinds) -> EngineRun {
    start_engine(Engine::new(pool.clone(), registry, NonZeroU32::MIN)).await
}

async fn start_engine(engine: Engine) -> EngineRun {
    engine
        .check_startup()
        .await
        .expect("outbox engine admits the shared jobs store");
    let tracker = TaskTracker::new();
    let cancel = CancellationToken::new();
    let started = engine.start(&tracker, &cancel);
    tracker.close();
    let cancel_guard = cancel.clone().drop_guard();
    EngineRun {
        started,
        tracker,
        cancel,
        _cancel_guard: cancel_guard,
    }
}

async fn finish(run: EngineRun, pools: &[&PgPool]) {
    run.started.stop_claiming();
    timeout(WAIT, run.started.drained())
        .await
        .expect("the engine drains");
    run.cancel.cancel();
    timeout(WAIT, run.tracker.wait())
        .await
        .expect("engine background tasks join");
    close_pools(pools).await;
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

async fn job(pool: &PgPool, key: &str) -> JobRow {
    let row = sqlx::query(
        "SELECT id::text AS id, state, attempts, failure_reason, error_summary \
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
        failure_reason: row.try_get("failure_reason").expect("failure reason"),
        error_summary: row.try_get("error_summary").expect("error summary"),
    }
}

struct JobRow {
    id: String,
    state: String,
    attempts: i16,
    failure_reason: Option<String>,
    error_summary: Option<String>,
}

async fn outbox_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM background_jobs WHERE kind = $1")
        .bind(OUTBOX_KIND)
        .fetch_one(pool)
        .await
        .expect("outbox row count")
}

fn prepared(fixture: &Fixture, id: &str, value: &str) -> infra_messaging::PreparedEvent {
    fixture
        .publication
        .clone()
        .for_outbox(PUBLISHER_ID)
        .prepare(&event(id, value))
        .expect("fixture event prepares once")
}

#[derive(Debug, derive_more::From, derive_more::Display)]
enum Step {
    #[from(skip)]
    #[display("caller refused the transaction")]
    Refused,
    #[display("envelope: {_0}")]
    Envelope(EnvelopeError),
    #[display("outbox: {_0}")]
    Outbox(OutboxEnqueueError),
    #[display("jobs: {_0}")]
    Jobs(EnqueueError),
    #[display("query: {_0}")]
    Query(sqlx::Error),
    #[display("transaction: {_0}")]
    Transaction(TxError),
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn typed_refusal_leaves_no_outbox_intent_and_allows_corrected_same_id(pool: PgPool) {
    #[derive(serde::Serialize)]
    #[serde(untagged)]
    enum Payload {
        Number(Option<f64>),
        Json(Box<serde_json::value::RawValue>),
    }

    impl EventPayload for Payload {
        const EVENT_TYPE: &'static str = "test.outbox.checked";
        const SCHEMA_VERSION: u16 = 1;
    }

    let fixture = Fixture::create().await;
    let jobs = template_pool(&pool, 3).await;
    let route = Route::<Payload>::new(fixture.subject.clone());
    let mut registry = Registry::default();
    let publication = registry
        .declare_publication(&route, 1024)
        .expect("typed publication route")
        .for_outbox(PUBLISHER_ID);
    let leaf = serde_json::to_string("[{}]\\\"\\").expect("escaped string");
    let raw127 = format!("{}{leaf}{}", "[".repeat(127), "]".repeat(127));
    let raw128 = format!("[{raw127}]");
    assert!(serde_json::from_str::<serde_json::Value>(&raw127).is_ok());
    assert!(serde_json::from_str::<serde_json::Value>(&raw128).is_err());
    let cases = [
        (
            "event-float-retry",
            Payload::Number(Some(f64::NAN)),
            Payload::Number(None),
            EnvelopeError::Serialization,
            b"null".to_vec(),
        ),
        (
            "event-depth-retry",
            Payload::Json(serde_json::value::RawValue::from_string(raw128).expect("raw JSON")),
            Payload::Json(
                serde_json::value::RawValue::from_string(raw127.clone()).expect("raw JSON"),
            ),
            EnvelopeError::PayloadStructure,
            raw127.into_bytes(),
        ),
    ];
    for (id, invalid, corrected, expected_error, expected_bytes) in cases {
        let before = outbox_count(&jobs).await;
        let mut event = Event {
            id: id.to_owned(),
            occurred_at: time::UtcDateTime::from_unix_timestamp(1_700_000_000)
                .expect("fixed occurrence"),
            payload: invalid,
        };
        let rejected = in_tx(&jobs, async |tx| -> Result<_, Step> {
            match publication.prepare(&event) {
                Ok(prepared) => Ok(Ok(prepared.enqueue(tx).await?)),
                Err(error) => Ok(Err(error)),
            }
        })
        .await
        .expect("refusal leaves the caller transaction usable");
        assert_eq!(rejected, Err(expected_error));
        assert_eq!(outbox_count(&jobs).await, before);

        event.payload = corrected;
        let prepared = publication.prepare(&event).expect("corrected payload");
        assert_eq!(prepared.payload().as_ref(), expected_bytes);
        assert_eq!(
            in_tx(&jobs, async |tx| -> Result<_, Step> {
                prepared.enqueue(tx).await.map_err(Step::from)
            })
            .await
            .expect("the refused identity remains available"),
            OutboxEnqueued::Created
        );
        assert_eq!(outbox_count(&jobs).await, before + 1);
    }
    close_pools(&[&jobs]).await;
    fixture.cleanup().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn business_rollback_hides_intent_commit_publishes_exact_prepared_event(pool: PgPool) {
    #[derive(serde::Serialize)]
    #[serde(untagged)]
    enum AccountUpdated {
        Sorted(BTreeMap<&'static str, &'static str>),
        Reordered(Box<serde_json::value::RawValue>),
    }

    impl EventPayload for AccountUpdated {
        const EVENT_TYPE: &'static str = "test.outbox.account_updated";
        const SCHEMA_VERSION: u16 = 1;
    }

    const ORIGINAL_JSON: &str = r#"{"account_id":"42","status":"active"}"#;
    const REORDERED_JSON: &str = r#"{"status":"active","account_id":"42"}"#;

    let fixture = Fixture::create_with_window(false, Duration::from_secs(1)).await;
    let messaging = fixture
        .connect_producer()
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
            .execute(&mut *tx)
            .await?;
        assert_eq!(rolled_back.enqueue(tx).await?, OutboxEnqueued::Created);
        Err(Step::Refused)
    })
    .await;
    std::assert_matches!(rollback, Err(Step::Refused));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM business_effects")
            .fetch_one(&business)
            .await
            .expect("business effects after rollback"),
        0
    );
    assert_eq!(outbox_count(&business).await, 0);

    let route = Route::<AccountUpdated>::new(fixture.subject.clone());
    let publication = Registry::default()
        .declare_publication(&route, 1024)
        .expect("account publication route")
        .for_outbox(PUBLISHER_ID);
    let mut event = Event {
        id: "event-commit".into(),
        occurred_at: time::UtcDateTime::from_unix_timestamp(1_700_000_000)
            .expect("fixed occurrence"),
        payload: AccountUpdated::Sorted(BTreeMap::from([
            ("account_id", "42"),
            ("status", "active"),
        ])),
    };
    let replay_route = publication.clone();
    let committed = publication
        .prepare(&event)
        .expect("original account update");
    let committed_outcome = in_tx(&business, async |tx| -> Result<_, Step> {
        sqlx::query("INSERT INTO business_effects (id) VALUES ('commit')")
            .execute(&mut *tx)
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
        infra_messaging::outbox::kinds(messaging.outbox_producer().unwrap())
            .expect("outbox publisher registry is valid"),
    )
    .await;
    poll_until(
        "committed outbox event is synchronously acknowledged by JetStream",
        WAIT,
        POLL,
        async || (fixture.published().await == 1).then_some(()),
    )
    .await;
    let row = poll_until(
        "acknowledged publication is fenced complete",
        WAIT,
        POLL,
        async || {
            let row = job(&business, &event_key(committed.message_id())).await;
            (row.state == "completed").then_some(row)
        },
    )
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
    assert_eq!(stored.payload.as_ref(), ORIGINAL_JSON.as_bytes());
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

    event.payload = AccountUpdated::Reordered(
        serde_json::value::RawValue::from_string(REORDERED_JSON.to_owned())
            .expect("reordered account update JSON"),
    );
    let reordered = replay_route
        .prepare(&event)
        .expect("reordered account update");
    assert_eq!(reordered.payload().as_ref(), REORDERED_JSON.as_bytes());

    event.payload =
        AccountUpdated::Sorted(BTreeMap::from([("status", "active"), ("account_id", "42")]));
    let same = replay_route
        .prepare(&event)
        .expect("rebuilt account update");

    verify_completed_identity(&fixture, &messaging, &business, &reordered, &same, &row.id).await;
    finish(run, &[&publisher, &business]).await;
    assert_eq!(fixture.published().await, 1);
    close(messaging).await;
    fixture.cleanup().await;
}

async fn verify_completed_identity(
    fixture: &Fixture,
    messaging: &Messaging,
    business: &PgPool,
    reordered: &infra_messaging::PreparedEvent,
    same: &infra_messaging::PreparedEvent,
    job_id: &str,
) {
    let cleanup = Engine::new(
        business.clone(),
        infra_messaging::outbox::kinds(messaging.outbox_producer().unwrap()).unwrap(),
        NonZeroU32::MIN,
    );
    for retained in [true, false] {
        if !retained {
            sqlx::query("UPDATE background_jobs SET finished_at = now() - interval '2 days' WHERE id::text = $1")
                .bind(job_id)
                .execute(business)
                .await
                .unwrap();
            assert_eq!(cleanup.remove_expired().await.unwrap(), 1);
            tokio::time::sleep(fixture.duplicate_window + Duration::from_millis(20)).await;
        }
        let conflicting = in_tx(business, async |tx| -> Result<_, Step> {
            sqlx::query("INSERT INTO business_effects (id) VALUES ('conflicting')")
                .execute(&mut *tx)
                .await?;
            reordered.enqueue(tx).await.map_err(Step::from)
        })
        .await;
        std::assert_matches!(
            conflicting,
            Err(Step::Outbox(OutboxEnqueueError::EventIdConflict))
        );
        let duplicate = in_tx(business, async |tx| -> Result<_, Step> {
            same.enqueue(tx).await.map_err(Step::from)
        })
        .await
        .unwrap();
        assert_eq!(duplicate, OutboxEnqueued::Duplicate);
        assert_eq!(outbox_count(business).await, i64::from(retained));
    }
    let business_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM business_effects")
        .fetch_one(business)
        .await
        .unwrap();
    assert_eq!(business_rows, 1);
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn independent_publishers_deliver_their_own_event_for_the_same_local_id(pool: PgPool) {
    let fixture = Fixture::create_with_window(false, Duration::from_mins(5)).await;
    let local_id = "order:17";
    let jobs = template_pool(&pool, 4).await;
    let unscoped = fixture
        .publication
        .prepare(&event(local_id, "unbound"))
        .unwrap();
    let reconcile = async || {
        in_tx(&jobs, async |tx| -> Result<_, Step> {
            unscoped.enqueue(tx).await.map_err(Step::from)
        })
        .await
    };
    let (a, b) = tokio::join!(reconcile(), reconcile());
    for outcome in [a, b] {
        std::assert_matches!(
            outcome,
            Err(Step::Outbox(OutboxEnqueueError::PublisherRequired))
        );
    }
    assert_eq!(outbox_count(&jobs).await, 0);
    let stream = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    let start_time = Instant::now();
    for (sequence, publisher, value) in [
        (1, PUBLISHER_ID, "first order"),
        (
            2,
            uuid::NonNilUuid::new(uuid::Uuid::from_u128(
                0x3df13a07_b452_4cf0_ad2a_81acb263b11f,
            ))
            .unwrap(),
            "second order",
        ),
    ] {
        let messaging = fixture.connect_producer().await.unwrap();
        let workers = template_pool(&pool, 3).await;
        let run = start(
            &workers,
            infra_messaging::outbox::kinds(messaging.outbox_producer().unwrap()).unwrap(),
        )
        .await;
        let authored = event(local_id, value);
        let expected_id = format!(
            "outbox-v1-{}-4df9dbb3462374b8510500956968320d2725d77ae107b87e4e41ef80d3dbd1bf",
            publisher.get().simple()
        );
        for outcome in [OutboxEnqueued::Created, OutboxEnqueued::Duplicate] {
            let route = fixture.publication.clone().for_outbox(publisher);
            let prepared = route.prepare(&authored).unwrap();
            assert_eq!(prepared.message_id(), expected_id);
            let observed = in_tx(&jobs, async |tx| -> Result<_, Step> {
                prepared.enqueue(tx).await.map_err(Step::from)
            })
            .await
            .unwrap();
            assert_eq!(observed, outcome);
            poll_until("publisher receives a broker ACK", WAIT, POLL, async || {
                (job(&jobs, &event_key(prepared.message_id())).await.state == "completed")
                    .then_some(())
            })
            .await;
        }
        let message = stream.get_raw_message(sequence).await.unwrap();
        for header in ["Message-Id", "Nats-Msg-Id"] {
            assert_eq!(message.headers.get(header).unwrap().as_str(), expected_id);
        }
        assert_eq!(message.subject.as_ref(), fixture.subject.as_str());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&message.payload).unwrap(),
            serde_json::json!({"value": value})
        );
        assert_eq!(fixture.published().await, sequence);
        finish(run, &[&workers]).await;
        close(messaging).await;
    }
    assert!(start_time.elapsed() < fixture.duplicate_window);
    assert_eq!(outbox_count(&jobs).await, 2);
    close_pools(&[&jobs]).await;
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
    let same_id = race_ordered_enqueues(
        &fixture,
        &jobs,
        &competing,
        "by-id",
        "event-conflict",
        "event-conflict",
    )
    .await;
    std::assert_matches!(
        same_id,
        Err(Step::Outbox(OutboxEnqueueError::EventIdConflict))
    );
    let same_version = race_ordered_enqueues(
        &fixture,
        &jobs,
        &competing,
        "by-version",
        "event-one",
        "event-two",
    )
    .await;
    std::assert_matches!(
        same_version,
        Err(Step::Outbox(OutboxEnqueueError::OrderConflict))
    );
    assert_eq!(outbox_count(&jobs).await, 3);
    close_pools(&[&competing, &jobs]).await;
    fixture.cleanup().await;
}

async fn race_ordered_enqueues(
    fixture: &Fixture,
    jobs: &PgPool,
    competing: &PgPool,
    aggregate: &'static str,
    first_id: &str,
    second_id: &str,
) -> Result<OutboxEnqueued, Step> {
    let held = prepared(fixture, first_id, "first");
    let conflicting = prepared(fixture, second_id, "second");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let first_pool = jobs.clone();
    let first_entered = Arc::clone(&entered);
    let first_release = Arc::clone(&release);
    let first_task = tokio::spawn(async move {
        in_tx(&first_pool, async |tx| -> Result<_, Step> {
            let result = held.enqueue_ordered(tx, aggregate, 1).await?;
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
            conflicting
                .enqueue_ordered(tx, aggregate, 1)
                .await
                .map_err(Step::from)
        })
        .await
    });
    poll_until(
        "the conflicting insert waits for the live unique key",
        WAIT,
        POLL,
        async || {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity \
             WHERE datname = current_database() AND application_name = $1 \
               AND wait_event_type = 'Lock' AND pid <> pg_backend_pid()",
            )
            .bind(APP)
            .fetch_one(jobs)
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
    timeout(WAIT, second_task)
        .await
        .expect("conflicting transaction joins")
        .expect("conflicting task does not panic")
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn lost_broker_ack_retries_then_republishes_the_same_immutable_identity(pool: PgPool) {
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
    let intent = prepared(&fixture, "event-lost-ack", "ambiguous");
    let key = event_key(intent.message_id());
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
        infra_messaging::outbox::kinds(first_messaging.outbox_producer().unwrap())
            .expect("outbox publisher registry is valid"),
    )
    .await;
    let retrying = poll_until(
        "lost broker ACK spends an attempt and retains a pending outbox intent",
        WAIT,
        POLL,
        async || {
            let row = job(&jobs, &key).await;
            (relay.dropped_ack.is_cancelled()
                && row.state == "pending"
                && row.attempts >= 1
                && row.error_summary.is_some())
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
        Some(intent.publication_id().to_owned())
    );
    finish(first_run, &[&first_pool]).await;
    close(first_messaging).await;
    relay.join().await;

    sqlx::query("UPDATE background_jobs SET not_before = statement_timestamp() - interval '1 second' WHERE id::text = $1")
        .bind(&retrying.id)
        .execute(&jobs)
        .await
        .expect("ambiguous intent is made due for its same-ID retry");
    let retry_messaging = fixture
        .connect_producer()
        .await
        .expect("direct producer reconnects after the relay closes");
    let retry_pool = template_pool(&pool, 3).await;
    let retry_run = start(
        &retry_pool,
        infra_messaging::outbox::kinds(retry_messaging.outbox_producer().unwrap())
            .expect("retry publisher registry is valid"),
    )
    .await;
    let completed = poll_until(
        "same immutable identity receives duplicate broker ACK and completes",
        WAIT,
        POLL,
        async || {
            let row = job(&jobs, &key).await;
            (row.state == "completed").then_some(row)
        },
    )
    .await;
    assert!(completed.attempts >= 2);
    assert_eq!(fixture.published().await, 1);

    finish(retry_run, &[&retry_pool, &jobs]).await;
    close(retry_messaging).await;
    fixture.cleanup().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn outbox_retries_after_a_route_changing_stream_update(pool: PgPool) {
    let fixture = Fixture::create().await;
    let messaging = fixture.connect_producer().await.unwrap();
    let kinds = infra_messaging::outbox::kinds(messaging.outbox_producer().unwrap()).unwrap();
    let source = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    let original = source.cached_info().config.clone();
    let mut rewritten = original.clone();
    rewritten.subject_transform = Some(stream::SubjectTransform {
        source: fixture.subject.clone(),
        destination: format!("{}.rewritten", fixture.subject),
    });
    fixture.jetstream.update_stream(rewritten).await.unwrap();

    let jobs = template_pool(&pool, 3).await;
    let intent = prepared(&fixture, "live-transform", "preserve route");
    let key = event_key(intent.message_id());
    in_tx(&jobs, async |tx| -> Result<_, Step> {
        intent.enqueue(tx).await.map_err(Step::from)
    })
    .await
    .unwrap();
    let run = start(&jobs, kinds).await;
    let attempt = poll_until("relay attempts publication", WAIT, POLL, async || {
        let row = job(&jobs, &key).await;
        (row.attempts > 0 && row.state != "running").then_some(row)
    })
    .await;
    assert_eq!(attempt.state, "pending");
    assert_eq!(attempt.error_summary.as_deref(), Some("broker_refused"));
    assert_eq!(fixture.published().await, 0);

    fixture.jetstream.update_stream(original).await.unwrap();
    poll_until(
        "same relay completes after route repair",
        WAIT,
        POLL,
        async || (job(&jobs, &key).await.state == "completed").then_some(()),
    )
    .await;
    assert_eq!(fixture.published().await, 1);
    let stored = source.get_raw_message(1).await.unwrap();
    assert_eq!(stored.subject.as_str(), fixture.subject);
    assert_eq!(&stored.payload, intent.payload());
    finish(run, &[&jobs]).await;
    close(messaging).await;
    fixture.cleanup().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn route_limit_refuses_before_commit_and_a_later_reduction_refuses_old_intent(pool: PgPool) {
    let fixture = Fixture::create().await;
    let messaging = Box::pin(Messaging::connect(
        MessagingOptions {
            max_payload_bytes: 64,
            ..messaging_options(&fixture)
        },
        Instant::now() + WAIT,
        CancellationToken::new(),
    ))
    .await
    .expect("producer-only messaging admits before the controlled outage");
    let jobs = template_pool(&pool, 3).await;
    let mut declarations = Registry::default();
    let publication = declarations
        .declare_publication(&fixture.route, 64)
        .unwrap()
        .for_outbox(PUBLISHER_ID);
    let too_large = event("oversized-new-intent", &"x".repeat(53));
    let before = outbox_count(&jobs).await;
    let refused = in_tx(&jobs, async |tx| -> Result<(), Step> {
        publication.prepare(&too_large)?.enqueue(tx).await?;
        Ok(())
    })
    .await;
    std::assert_matches!(refused, Err(Step::Envelope(EnvelopeError::PayloadTooLarge)));
    assert_eq!(outbox_count(&jobs).await, before);
    let intent = publication
        .prepare(&event("event-exhausted", &"x".repeat(52)))
        .unwrap();
    assert_eq!(intent.payload().len(), 64);
    let oversized = prepared(&fixture, "event-oversized", &"x".repeat(64));
    in_tx(&jobs, async |tx| -> Result<_, Step> {
        assert_eq!(intent.enqueue(tx).await?, OutboxEnqueued::Created);
        assert_eq!(oversized.enqueue(tx).await?, OutboxEnqueued::Created);
        Ok(())
    })
    .await
    .expect("both the boundary event and an intent from the previous larger route commit");
    let key = event_key(intent.message_id());
    let id = job(&jobs, &key).await.id;
    fixture.remove_source().await;

    let publisher = template_pool(&pool, 3).await;
    let run = start(
        &publisher,
        infra_messaging::outbox::kinds(messaging.outbox_producer().unwrap())
            .expect("outbox publisher registry is valid"),
    )
    .await;
    let oversized_key = event_key(oversized.message_id());
    let make_due = async |generation: i64| {
        let updated = sqlx::query(
            "UPDATE background_jobs SET not_before = statement_timestamp()
             WHERE id::text = $1 AND claim_generation = $2 AND state = 'pending'",
        )
        .bind(&id)
        .bind(generation)
        .execute(&jobs)
        .await
        .expect("make a scheduled retry due");
        assert_eq!(updated.rows_affected(), 1);
    };
    let mut generation = 0;
    for (attempt, oldest, delay_seconds) in [
        (1_i16, 1_i16, 1.8..=2.2),
        (2, 1, 3.6..=4.4),
        (24, 1, 48.6..=59.4),
        (25, 1, 48.6..=59.4),
        (25, 2, 48.6..=59.4),
    ] {
        if attempt == 24 {
            let seeded = sqlx::query(
                "UPDATE background_jobs SET attempts = 23, errors = errors || (
                     SELECT jsonb_agg(jsonb_build_object('attempt', attempt, 'at', statement_timestamp(),
                                                        'error', 'broker_refused') ORDER BY attempt)
                     FROM generate_series(3, 23) AS history(attempt)
                 ) WHERE id::text = $1 AND claim_generation = $2 AND state = 'pending'",
            )
            .bind(&id)
            .bind(generation)
            .execute(&jobs)
            .await
            .expect("stage the intervening retry history");
            assert_eq!(seeded.rows_affected(), 1);
        }
        if generation != 0 {
            make_due(generation).await;
        }
        let (next_generation, history_len, oldest_attempt, delay) =
            poll_until("broker refusal durably schedules a retry", WAIT, POLL, async || {
                sqlx::query_as::<_, (i64, i32, i16, f64)>(
                    "SELECT claim_generation, jsonb_array_length(errors),
                            (errors -> 0 ->> 'attempt')::smallint,
                            EXTRACT(EPOCH FROM not_before - (errors -> -1 ->> 'at')::timestamptz)::double precision
                     FROM background_jobs
                     WHERE id::text = $1 AND claim_generation > $2 AND attempts = $3
                           AND state = 'pending' AND failure_reason IS NULL",
                )
                .bind(&id)
                .bind(generation)
                .bind(attempt)
                .fetch_optional(&jobs)
                .await
                .expect("read the stored delay from the transition's PostgreSQL clock")
            })
            .await;
        assert!(
            delay_seconds.contains(&delay),
            "attempt {attempt} scheduled {delay}s outside {delay_seconds:?}"
        );
        assert_eq!(history_len, i32::from(attempt));
        assert_eq!(oldest_attempt, oldest);
        generation = next_generation;
    }
    let permanent = poll_until(
        "local payload refusal is terminal",
        WAIT,
        POLL,
        async || {
            let row = job(&jobs, &oversized_key).await;
            (row.state == "failed").then_some(row)
        },
    )
    .await;
    assert_eq!(permanent.attempts, 1);
    assert_eq!(permanent.failure_reason.as_deref(), Some("permanent"));
    fixture.restore_source().await;
    let interrupted = prepared(&fixture, "final-lease", "interrupted");
    in_tx(&jobs, async |tx| -> Result<(), Step> {
        interrupted.enqueue(tx).await?;
        sqlx::query(
            "UPDATE background_jobs SET state='running', attempts=25,
             claim_generation=nextval('background_jobs_claim_generation'),
             claim_token=gen_random_uuid(),
             claim_expires_at=now() - interval '1 second' WHERE unique_key=$1",
        )
        .bind(event_key(interrupted.message_id()))
        .execute(&mut *tx)
        .await?;
        Ok(())
    })
    .await
    .unwrap();
    let completed = poll_until(
        "restoring the stream resumes the outstanding publication without redrive",
        Duration::from_secs(60) + WAIT,
        POLL,
        async || {
            let row = job(&jobs, &key).await;
            (row.state == "completed"
                && job(&jobs, &event_key(interrupted.message_id())).await.state == "completed")
                .then_some(row)
        },
    )
    .await;
    assert_eq!(completed.id, id);
    assert_eq!(completed.attempts, 25);
    let stream = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    let mut ids = Vec::new();
    for seq in 1..=2 {
        let message = stream.get_raw_message(seq).await.unwrap();
        let id = message.headers.get("Message-Id").unwrap().as_str();
        let expected = if id == intent.message_id() {
            &intent
        } else {
            &interrupted
        };
        assert_eq!(id, expected.message_id());
        assert_eq!(message.payload, *expected.payload());
        ids.push(id.to_owned());
    }
    assert_ne!(ids[0], ids[1]);
    assert_eq!(fixture.published().await, 2);
    assert_eq!(job(&jobs, &oversized_key).await.state, "failed");
    finish(run, &[&publisher, &jobs]).await;
    close(messaging).await;
    fixture.cleanup().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn ordered_redrive_keeps_successors_behind_their_failed_predecessor(pool: PgPool) {
    use infra_jobs::operator::{self, OperatorError, RecoveryTarget};
    let fixture = Fixture::create().await;
    let jobs = template_pool(&pool, 3).await;
    let first = prepared(&fixture, "A1", &"a".repeat(100));
    let second = prepared(&fixture, "A2", "second");
    let independent = prepared(&fixture, "B1", "independent");
    in_tx(&jobs, async |tx| -> Result<_, Step> {
        std::assert_matches!(
            second.enqueue_ordered(tx, "A", 2).await,
            Err(OutboxEnqueueError::OrderConflict)
        );
        Ok(())
    })
    .await
    .unwrap();
    in_tx(&jobs, async |tx| -> Result<_, Step> {
        assert_eq!(
            first.enqueue_ordered(tx, "A", 1).await?,
            OutboxEnqueued::Created
        );
        Ok(())
    })
    .await
    .unwrap();
    in_tx(&jobs, async |tx| -> Result<_, Step> {
        std::assert_matches!(
            independent.enqueue_ordered(tx, "A", 1).await,
            Err(OutboxEnqueueError::OrderConflict)
        );
        assert_eq!(
            second.enqueue_ordered(tx, "A", 2).await?,
            OutboxEnqueued::Created
        );
        assert_eq!(
            independent.enqueue_ordered(tx, "B", 1).await?,
            OutboxEnqueued::Created
        );
        assert_eq!(
            first.enqueue_ordered(tx, "A", 1).await?,
            OutboxEnqueued::Duplicate
        );
        std::assert_matches!(
            first.enqueue(tx).await,
            Err(OutboxEnqueueError::EventIdConflict)
        );
        Ok(())
    })
    .await
    .unwrap();
    let connect = async |limit| {
        let mut options = messaging_options(&fixture);
        options.max_payload_bytes = limit;
        Messaging::connect(options, Instant::now() + WAIT, CancellationToken::new())
            .await
            .unwrap()
    };
    let publisher_a = template_pool(&pool, 3).await;
    let publisher_b = template_pool(&pool, 3).await;
    let start_publisher = async |pool: &PgPool, messaging: &Messaging| {
        let kinds = infra_messaging::outbox::kinds(messaging.outbox_producer().unwrap()).unwrap();
        start(pool, kinds).await
    };
    let restricted = connect(64).await;
    let a = start_publisher(&publisher_a, &restricted).await;
    let b = start_publisher(&publisher_b, &restricted).await;
    poll_until(
        "independent entity publishes despite the failed predecessor",
        WAIT,
        POLL,
        async || {
            (job(&jobs, &event_key(independent.message_id())).await.state == "completed"
                && job(&jobs, &event_key(first.message_id())).await.state == "failed")
                .then_some(())
        },
    )
    .await;
    finish(a, &[]).await;
    finish(b, &[]).await;
    close(restricted).await;
    assert_eq!(fixture.published().await, 1);
    assert_eq!(
        job(&jobs, &event_key(second.message_id())).await.attempts,
        0
    );
    let (id, generation): (String, String) = sqlx::query_as(
        "SELECT id::text, claim_generation::text FROM background_jobs WHERE kind=$1 AND unique_key=$2"
    ).bind(OUTBOX_KIND).bind(event_key(first.message_id())).fetch_one(&jobs).await.unwrap();
    let target = RecoveryTarget::try_new(&id, OUTBOX_KIND, &generation).unwrap();
    let discard = in_tx(&jobs, async |tx| operator::discard(tx, &target).await).await;
    std::assert_matches!(discard, Err(OperatorError::Conflict));
    in_tx(&jobs, async |tx| operator::redrive(tx, &target).await)
        .await
        .unwrap();
    let recovered = connect(1024).await;
    let a = start_publisher(&publisher_a, &recovered).await;
    let b = start_publisher(&publisher_b, &recovered).await;
    poll_until(
        "successor completes after redrive",
        WAIT,
        POLL,
        async || {
            (job(&jobs, &event_key(second.message_id())).await.state == "completed").then_some(())
        },
    )
    .await;
    let stream = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    for (seq, expected) in [(1, &independent), (2, &first), (3, &second)] {
        let message = stream.get_raw_message(seq).await.unwrap();
        assert_eq!(
            message.headers.get("Message-Id").unwrap().as_str(),
            expected.message_id()
        );
        assert_eq!(message.payload, *expected.payload());
    }
    finish(a, &[&publisher_a]).await;
    finish(b, &[&publisher_b, &jobs]).await;
    close(recovered).await;
    fixture.cleanup().await;
}

#[sqlx::test(migrations = false)]
async fn stored_intent_rejects_corruption_and_publishes_valid_json_bytes_unchanged(pool: PgPool) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    migrate::MIGRATOR
        .run_to(upgrade::HISTORICAL_SCHEMA, &pool)
        .await
        .unwrap();
    let fixture = Fixture::create().await;
    let messaging = fixture
        .connect_producer()
        .await
        .expect("producer-only messaging admits the fixture stream");
    let jobs = template_pool(&pool, 3).await;
    let nested = |depth| format!("{}1.00 {}", "[".repeat(depth), "]".repeat(depth));
    let valid = nested(127);
    let corrupt = [
        (
            "bad-base64",
            "not-base64".to_owned(),
            "outbox_base64_invalid",
        ),
        (
            "bad-utf8",
            STANDARD.encode([b'"', 0xff, b'"']),
            "outbox_json_invalid",
        ),
        ("bad-json", STANDARD.encode(b"[1,]"), "outbox_json_invalid"),
        (
            "too-deep",
            STANDARD.encode(nested(128)),
            "outbox_payload_too_deep",
        ),
    ];
    let valid_body = STANDARD.encode(&valid);
    for (id, encoded) in corrupt
        .iter()
        .map(|(id, body, _)| (*id, body))
        .chain([("valid", &valid_body)])
    {
        upgrade::stage_encoded(&pool, &fixture.subject, id, encoded).await;
    }
    migrate::MIGRATOR.run(&pool).await.unwrap();
    let mut corrupt_keys: Vec<_> = corrupt
        .iter()
        .map(|(id, _, reason)| (event_key(id), *reason))
        .collect();
    for id in ["substituted", "malformed-outer"] {
        let intent = prepared(&fixture, id, "original");
        in_tx(&jobs, async |tx| -> Result<_, Step> {
            intent.enqueue(tx).await.map_err(Step::from)
        })
        .await
        .unwrap();
        let changed = sqlx::query(
            "UPDATE background_jobs SET payload_text = CASE WHEN $2 THEN '{' ELSE
             jsonb_set(payload_text::jsonb, '{payload_base64}', to_jsonb($3::text), false)::text END
             WHERE unique_key = $1",
        )
        .bind(event_key(intent.message_id()))
        .bind(id == "malformed-outer")
        .bind(&valid_body)
        .execute(&jobs)
        .await
        .unwrap();
        assert_eq!(changed.rows_affected(), 1);
        corrupt_keys.push((event_key(intent.message_id()), "outbox_receipt_mismatch"));
    }

    let publisher = template_pool(&pool, 3).await;
    let run = start(
        &publisher,
        infra_messaging::outbox::kinds(messaging.outbox_producer().unwrap())
            .expect("outbox publisher registry is valid"),
    )
    .await;
    for (key, reason) in corrupt_keys {
        let failed = poll_until("corrupt intent is terminal", WAIT, POLL, async || {
            let row = job(&jobs, &key).await;
            (row.state == "failed").then_some(row)
        })
        .await;
        assert_eq!(failed.attempts, 1, "{key}");
        assert_eq!(failed.failure_reason.as_deref(), Some("permanent"), "{key}");
        assert_eq!(failed.error_summary.as_deref(), Some(reason), "{key}");
    }
    let key = event_key("valid");
    poll_until("valid intent completes", WAIT, POLL, async || {
        (job(&jobs, &key).await.state == "completed").then_some(())
    })
    .await;
    assert_eq!(fixture.published().await, 1);
    let stored = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .unwrap()
        .get_raw_message(1)
        .await
        .unwrap();
    assert_eq!(stored.payload.as_ref(), valid.as_bytes());
    assert_eq!(stored.headers.get("Nats-Msg-Id").unwrap().as_str(), "valid");

    finish(run, &[&publisher, &jobs]).await;
    close(messaging).await;
    fixture.cleanup().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn durable_consumer_effect_dedupes_same_logical_id_after_broker_window(pool: PgPool) {
    let fixture = Fixture::create_with_consumer(true).await;
    let messaging = Box::pin(Messaging::connect(
        consumer_options(&fixture, &fixture.durable),
        Instant::now() + WAIT,
        CancellationToken::new(),
    ))
    .await
    .expect("messaging consumer topology admits");
    let effects = template_pool(&pool, 3).await;
    create_effect_store(&effects).await;
    let invoked = Arc::new(AtomicUsize::new(0));
    let registry = effect_registry(&fixture, &effects, &invoked);
    let cancel = CancellationToken::new();
    let mut handle = messaging
        .consumer(registry, Instant::now() + WAIT, &cancel)
        .await
        .expect("consumer admits")
        .start(&cancel);
    let event = prepared(&fixture, "event-durable-effect", "same");
    messaging
        .producer()
        .publish(
            &event,
            &OperationContext::new(
                Some(Deadline::at(Instant::now() + WAIT)),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("first publication ACKs");
    poll_until("first durable effect commits", WAIT, POLL, async || {
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
        .publish(
            &event,
            &OperationContext::new(
                Some(Deadline::at(Instant::now() + WAIT)),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("same-ID publication after broker window ACKs");
    poll_until(
        "second broker delivery reaches the real consumer",
        WAIT,
        POLL,
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
    assert_eq!(business_effects(&effects).await, 1);
    handle.drain();
    handle
        .finish(Instant::now() + CLOSE_BUDGET)
        .await
        .expect("consumer drains");
    close(messaging).await;
    close_pools(&[&effects]).await;
    fixture.cleanup().await;
}

async fn create_effect_store(pool: &PgPool) {
    sqlx::query("CREATE TABLE messaging_effects (logical_id text PRIMARY KEY)")
        .execute(pool)
        .await
        .expect("logical identity table");
    sqlx::query("CREATE TABLE messaging_business (applied bigint NOT NULL)")
        .execute(pool)
        .await
        .expect("business effect table");
    sqlx::query("INSERT INTO messaging_business VALUES (0)")
        .execute(pool)
        .await
        .expect("initial business value");
}

fn effect_registry(fixture: &Fixture, pool: &PgPool, invoked: &Arc<AtomicUsize>) -> Registry {
    let effect_pool = pool.clone();
    let invoked = Arc::clone(invoked);
    let mut registry = fixture.registry.clone();
    registry
        .register(&fixture.route, move |event, _| {
            let pool = effect_pool.clone();
            let invoked = Arc::clone(&invoked);
            async move {
                in_tx(&pool, async |tx| -> Result<(), Step> {
                    let inserted = sqlx::query(
                    "INSERT INTO messaging_effects (logical_id) VALUES ($1) ON CONFLICT DO NOTHING",
                ).bind(&event.id).execute(&mut *tx).await?.rows_affected();
                    if inserted == 1 {
                        sqlx::query("UPDATE messaging_business SET applied = applied + 1")
                            .execute(&mut *tx)
                            .await?;
                    }
                    Ok(())
                })
                .await
                .map_err(|_| HandlerError::Retryable)?;
                invoked.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .expect("durably idempotent handler registers");
    registry
}

async fn business_effects(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT applied FROM messaging_business")
        .fetch_one(pool)
        .await
        .expect("committed business effect count")
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn dlq_redrive_reaches_both_durables_and_applies_the_first_subscribers_effect_once(
    pool: PgPool,
) {
    let fixture = Fixture::create().await;
    create_effect_store(&pool).await;
    let invocations = Arc::new(AtomicUsize::new(0));
    let first_registry = effect_registry(&fixture, &pool, &invocations);
    let mut second_registry = fixture.registry.clone();
    let reject_once = AtomicBool::new(true);
    let (accepted, mut recovered) = tokio::sync::mpsc::channel(1);
    second_registry
        .register(&fixture.route, move |event, _| {
            std::future::ready(if reject_once.swap(false, Ordering::Relaxed) {
                Err(HandlerError::Permanent)
            } else {
                accepted.try_send(event.id).unwrap();
                Ok(())
            })
        })
        .unwrap();

    let cancel = CancellationToken::new();
    let subscribe = async |durable: &str, registry: Registry| {
        let messaging = Box::pin(Messaging::connect(
            consumer_options(&fixture, durable),
            Instant::now() + WAIT,
            cancel.clone(),
        ))
        .await
        .unwrap();
        let worker = messaging
            .consumer(registry, Instant::now() + WAIT, &cancel)
            .await
            .unwrap()
            .start(&cancel);
        (messaging, worker)
    };
    let second_name = format!("{}_recovery", fixture.durable);
    let first = subscribe(&fixture.durable, first_registry).await;
    let second = subscribe(&second_name, second_registry).await;
    let source = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    let durables: [consumer::PullConsumer; 2] = [
        source.get_consumer(&fixture.durable).await.unwrap(),
        source.get_consumer(&second_name).await.unwrap(),
    ];
    let wait_for_both_acks = async |sequence: u64| {
        for durable in &durables {
            poll_until(
                "subscriber acknowledged the source record",
                WAIT,
                POLL,
                async || {
                    let info = durable.get_info().await.unwrap();
                    (info.ack_floor.stream_sequence == sequence).then_some(())
                },
            )
            .await;
        }
    };

    let original = prepared(&fixture, "fanout-event", "original");
    let publisher = first.0.producer();
    let ack = publisher
        .publish(&original, &OperationContext::with_timeout(WAIT))
        .await
        .unwrap();
    wait_for_both_acks(ack.sequence).await;
    assert_eq!(invocations.load(Ordering::SeqCst), 1);
    assert_eq!(business_effects(&pool).await, 1);

    let dlq = fixture
        .jetstream
        .get_stream(&fixture.dlq_stream)
        .await
        .unwrap();
    let stored = dlq
        .get_last_raw_message_by_subject(&fixture.dlq_subject)
        .await
        .unwrap();
    let redrive = restore_dead_letter(DeadLetterRecord {
        headers: stored.headers,
        payload: stored.payload,
        stream: fixture.dlq_stream.clone(),
        stream_sequence: stored.sequence,
        stored_at: stored.time,
    })
    .unwrap();
    let replay = publisher
        .publish(&redrive, &OperationContext::with_timeout(WAIT))
        .await
        .unwrap();
    assert!(replay.sequence > ack.sequence);
    wait_for_both_acks(replay.sequence).await;
    assert_eq!(recovered.try_recv().unwrap(), original.message_id());
    assert_eq!(invocations.load(Ordering::SeqCst), 2);
    assert_eq!(business_effects(&pool).await, 1);

    for (messaging, mut worker) in [first, second] {
        worker.finish(Instant::now() + CLOSE_BUDGET).await.unwrap();
        close(messaging).await;
    }
    fixture.cleanup().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn lost_source_ack_request_redelivers_a_committed_effect_without_applying_it_again(
    pool: PgPool,
) {
    Box::pin(committed_effect_after_lost_settlement(
        pool,
        DroppedAck::Request,
    ))
    .await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn lost_source_ack_reply_can_hide_a_settled_committed_effect(pool: PgPool) {
    Box::pin(committed_effect_after_lost_settlement(
        pool,
        DroppedAck::Reply,
    ))
    .await;
}

async fn committed_effect_after_lost_settlement(pool: PgPool, dropped: DroppedAck) {
    let fixture = Fixture::create_with_consumer(false).await;
    let relay =
        AckDroppingRelay::start_source_ack(&fixture.stream, &fixture.durable, dropped).await;
    let mut options = consumer_options(&fixture, &fixture.durable);
    options.servers = vec![relay.url.clone()];
    let messaging = Box::pin(Messaging::connect(
        options,
        Instant::now() + WAIT,
        CancellationToken::new(),
    ))
    .await
    .expect("consumer connects through the settlement relay");
    let effects = template_pool(&pool, 3).await;
    create_effect_store(&effects).await;
    let invoked = Arc::new(AtomicUsize::new(0));
    let cancel = CancellationToken::new();
    let consumer = messaging
        .consumer(
            effect_registry(&fixture, &effects, &invoked),
            Instant::now() + WAIT,
            &cancel,
        )
        .await
        .expect("real durable consumer admits");
    let stream = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    let durable: consumer::PullConsumer = stream.get_consumer(&fixture.durable).await.unwrap();
    let ack_wait = durable.cached_info().config.ack_wait;
    let mut handle = consumer.start(&cancel);
    let event = prepared(&fixture, "event-lost-settlement", "committed");
    let ack = messaging
        .producer()
        .publish(
            &event,
            &OperationContext::new(Some(Deadline::at(Instant::now() + WAIT)), cancel.clone()),
        )
        .await
        .unwrap();
    relay.wait_for_drop().await;
    assert_eq!(
        business_effects(&effects).await,
        1,
        "the effect committed before settlement"
    );
    let state: consumer::PullConsumer = stream.get_consumer(&fixture.durable).await.unwrap();
    let state = state.cached_info();
    if dropped == DroppedAck::Request {
        assert!(state.ack_floor.stream_sequence < ack.sequence);
        poll_until(
            "lost source ACK redelivers the committed effect",
            ack_wait + WAIT,
            POLL,
            async || (invoked.load(Ordering::SeqCst) >= 2).then_some(()),
        )
        .await;
    } else {
        assert_eq!(state.ack_floor.stream_sequence, ack.sequence);
        assert_eq!(
            state.num_ack_pending, 0,
            "the broker already processed the lost ACK reply"
        );
    }
    poll_until("the durable settles", WAIT, POLL, async || {
        let durable: consumer::PullConsumer = stream.get_consumer(&fixture.durable).await.unwrap();
        let info = durable.cached_info();
        (info.ack_floor.stream_sequence >= ack.sequence && info.num_ack_pending == 0).then_some(())
    })
    .await;
    handle
        .finish(Instant::now() + WAIT)
        .await
        .expect("settlement work drains");
    assert_eq!(business_effects(&effects).await, 1);
    assert_eq!(
        invoked.load(Ordering::SeqCst),
        match dropped {
            DroppedAck::Request => 2,
            DroppedAck::Reply => 1,
        }
    );
    close(messaging).await;
    relay.join().await;
    close_pools(&[&effects]).await;
    fixture.cleanup().await;
}

// template:begin inbound-webhooks:outbox-test-messaging-outbox-inbound-fixture
fn webhook_receiver(pool: PgPool) -> Receiver {
    Receiver::new(
        pool,
        [(
            WEBHOOK_ENDPOINT.to_owned(),
            Arc::new(KeyRing::from_encoded(WEBHOOK_KEY, None).expect("fixture key"))
                as Arc<dyn Verifier>,
        )],
    )
}

fn admission_deadline() -> Instant {
    Instant::now() + infra_postgres::ACQUIRE_TIMEOUT
}

fn webhook_headers(message_id: &str, body: &[u8]) -> http::HeaderMap {
    let timestamp = time::OffsetDateTime::now_utc().unix_timestamp();
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

#[infra_webhooks::inbound::async_trait]
impl WebhookConsumer for HeldWebhookConsumer {
    async fn process(
        &self,
        tx: &mut infra_postgres::Tx<'_>,
        incoming: &Incoming,
    ) -> Result<(), JobError> {
        sqlx::query("INSERT INTO webhook_effects (message_id) VALUES ($1)")
            .bind(incoming.message_id())
            .execute(&mut *tx)
            .await
            .map_err(|_| JobError::retryable("webhook_effect_write_failed"))?;
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}
// template:end inbound-webhooks:outbox-test-messaging-outbox-inbound-fixture

// template:begin inbound-webhooks:outbox-test-messaging-outbox-webhook-capacity
#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn dedicated_publisher_progresses_while_real_inbound_webhook_processing_slot_is_occupied(
    pool: PgPool,
) {
    let fixture = Fixture::create().await;
    let messaging = fixture
        .connect_producer()
        .await
        .expect("producer-only messaging admits the fixture stream");
    let shared_pool = template_pool(&pool, 6).await;
    sqlx::query("CREATE TABLE webhook_effects (message_id bytea NOT NULL)")
        .execute(&shared_pool)
        .await
        .expect("webhook effect table");
    let held = Arc::new(HeldWebhookConsumer::default());
    let mut consumers = Consumers::new();
    consumers
        .bind(
            WEBHOOK_ENDPOINT,
            Arc::clone(&held) as Arc<dyn WebhookConsumer>,
        )
        .expect("first binding");
    let mut webhook_kinds = KindsBuilder::new();
    Processor::new(consumers).register(&mut webhook_kinds);
    let mut engine = Engine::new(
        shared_pool.clone(),
        webhook_kinds.validate().expect("webhook registry is valid"),
        NonZeroU32::MIN,
    );
    engine
        .add_group(
            infra_messaging::outbox::kinds(messaging.outbox_producer().unwrap())
                .expect("dedicated publisher registry is valid"),
            NonZeroU32::MIN,
        )
        .expect("publisher and webhook kinds are disjoint");
    let worker = start_engine(engine).await;
    let body = b"{\"process\":true}";
    assert!(
        webhook_receiver(shared_pool.clone())
            .receive(
                WEBHOOK_ENDPOINT,
                &webhook_headers("outbox-capacity", body),
                body,
                SystemTime::now(),
                admission_deadline(),
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
        .expect("outbox intent commits while real webhook work occupies its slot"),
        OutboxEnqueued::Created
    );
    poll_until(
        "dedicated publisher receives the broker ACK despite ordinary saturation",
        WAIT,
        POLL,
        async || (fixture.published().await == 1).then_some(()),
    )
    .await;

    worker.started.stop_claiming();
    held.release.notify_one();
    finish(worker, &[&shared_pool]).await;
    close(messaging).await;
    fixture.cleanup().await;
}
// template:end inbound-webhooks:outbox-test-messaging-outbox-webhook-capacity

fn event_key(id: &str) -> String {
    use sha2::{Digest as _, Sha256};

    infra_messaging::wire::prefixed_digest_hex("event-", &Sha256::digest(id.as_bytes()).into())
}
