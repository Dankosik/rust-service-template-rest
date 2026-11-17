use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use bytes::Bytes;
use infra_idempotency_store::{
    AttemptError, Attempted, CallerIdentity, CallerKind, Digest, Record, ScopeKey, Store,
};
use infra_jobs::{EnqueueOptions, JobKind, enqueue};
use infra_postgres::{Dsn, PgPool, Tx};
use integration_tests::dsn_for;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

#[derive(Debug, Serialize, Deserialize)]
struct WidgetWelcome {
    widget: u64,
}

impl JobKind for WidgetWelcome {
    const NAME: &'static str = "widgets.welcome";
}

const KEY: &[u8] = b"widget";
const OTHER_KEY: &[u8] = b"other-widget";
const INPUT: Digest = [0xa1; 32];
const WIDGET: u64 = 7;
const DUPLICATE_WIDGET: u64 = 8;

struct Replica {
    pool: PgPool,
    store: Store,
}

impl Replica {
    async fn open(dsn: &Dsn) -> Self {
        let pool = super::template_pool(dsn, 2).await;
        let store =
            Store::new(pool.clone(), Duration::from_hours(1)).expect("retention fits PostgreSQL");
        Self { pool, store }
    }
}

fn scope(key: &[u8]) -> ScopeKey {
    ScopeKey::new(
        CallerIdentity {
            issuer: "https://issuer.example".to_owned(),
            kind: CallerKind::Subject,
            value: "fixture-subject".to_owned(),
        },
        key,
    )
}

fn success(body: &str) -> Record {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Record::capture(
        StatusCode::CREATED,
        &headers,
        Bytes::copy_from_slice(body.as_bytes()),
    )
    .expect("a storable response")
}

async fn replicas(pool: &PgPool) -> (Replica, Replica) {
    let dsn = dsn_for(pool).await;
    (Replica::open(&dsn).await, Replica::open(&dsn).await)
}

async fn enqueue_widget(tx: &mut Tx<'_>, widget: u64) {
    let key = WIDGET.to_string();
    let _enqueued = enqueue(
        tx,
        &WidgetWelcome { widget },
        EnqueueOptions {
            delay: Duration::ZERO,
            unique_key: Some(&key),
        },
    )
    .await
    .expect("the enqueue attempt succeeds");
}

async fn committed_job_id(pool: &PgPool) -> String {
    assert_eq!(super::job_count(pool).await, 1);
    let payload = serde_json::to_string(&WidgetWelcome { widget: WIDGET }).expect("payload text");
    sqlx::query_scalar(
        "SELECT id::text FROM background_jobs \
         WHERE kind = $1 AND payload_text = $2 AND unique_key = $3 AND state = 'pending'",
    )
    .bind(WidgetWelcome::NAME)
    .bind(payload)
    .bind(WIDGET.to_string())
    .fetch_one(pool)
    .await
    .expect("the job row")
}

fn committed(outcome: Result<Attempted<()>, AttemptError>) -> Record {
    match outcome {
        Ok(Attempted::Committed(value)) => value,
        other => panic!("expected a commit, got {other:?}"),
    }
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e1_e3_a_committed_attempt_enqueues_once(pool: PgPool) {
    let first = Replica::open(&dsn_for(&pool).await).await;
    let record = success(r#"{"widget":7}"#);
    let outcome = first
        .store
        .attempt(&scope(KEY), &INPUT, async |tx: &mut Tx<'_>| {
            enqueue_widget(tx, WIDGET).await;
            Ok(record.clone())
        })
        .await;
    assert_eq!(committed(outcome), record);
    let _id = committed_job_id(&pool).await;
    super::close_pools(&[&first.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e3_a_replayed_attempt_enqueues_nothing(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let record = success(r#"{"widget":7}"#);
    let runs = AtomicUsize::new(0);
    let first_outcome = first
        .store
        .attempt(&scope(KEY), &INPUT, async |tx: &mut Tx<'_>| {
            runs.fetch_add(1, Ordering::SeqCst);
            enqueue_widget(tx, WIDGET).await;
            Ok(record.clone())
        })
        .await;
    assert_eq!(committed(first_outcome), record);

    let replay = second
        .store
        .attempt(
            &scope(KEY),
            &INPUT,
            async |tx: &mut Tx<'_>| -> Result<Record, ()> {
                runs.fetch_add(1, Ordering::SeqCst);
                enqueue_widget(tx, DUPLICATE_WIDGET).await;
                Ok(success(r#"{"widget":8}"#))
            },
        )
        .await;
    match replay {
        Ok(Attempted::Replay(stored)) => assert_eq!(stored, record),
        other => panic!("expected a replay, got {other:?}"),
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(super::job_count(&pool).await, 1);
    super::close_pools(&[&first.pool, &second.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e3_a_rolled_back_attempt_enqueues_nothing(pool: PgPool) {
    let first = Replica::open(&dsn_for(&pool).await).await;
    let runs = AtomicUsize::new(0);
    let outcome = first
        .store
        .attempt(&scope(KEY), &INPUT, async |tx: &mut Tx<'_>| {
            runs.fetch_add(1, Ordering::SeqCst);
            enqueue_widget(tx, WIDGET).await;
            Err(())
        })
        .await;
    std::assert_matches!(outcome, Ok(Attempted::RolledBack(())));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(super::job_count(&pool).await, 0);
    super::close_pools(&[&first.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e3_an_in_progress_attempt_enqueues_nothing(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let record = success(r#"{"widget":7}"#);
    let runs = AtomicUsize::new(0);
    let (entered_tx, entered_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let scope = scope(KEY);
    let (held, ()) = tokio::join!(
        first
            .store
            .attempt(&scope, &INPUT, async |tx: &mut Tx<'_>| {
                runs.fetch_add(1, Ordering::SeqCst);
                enqueue_widget(tx, WIDGET).await;
                entered_tx.send(()).expect("the entered receiver waits");
                release_rx.await.expect("the release sender sends");
                Ok(record.clone())
            }),
        async {
            super::bounded("the held work to enter", entered_rx)
                .await
                .expect("the held work enters");
            let refused = second
                .store
                .attempt(
                    &scope,
                    &INPUT,
                    async |tx: &mut Tx<'_>| -> Result<Record, ()> {
                        runs.fetch_add(1, Ordering::SeqCst);
                        enqueue_widget(tx, DUPLICATE_WIDGET).await;
                        Ok(record.clone())
                    },
                )
                .await;
            std::assert_matches!(refused, Ok(Attempted::InProgress), "{refused:?}");
            assert_eq!(runs.load(Ordering::SeqCst), 1);
            assert_eq!(super::job_count(&pool).await, 0);
            release_tx.send(()).expect("the held work waits");
        },
    );
    assert_eq!(committed(held), record);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let _id = committed_job_id(&pool).await;
    super::close_pools(&[&first.pool, &second.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_another_scope_commits_without_replacing_the_job(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let record = success(r#"{"widget":7}"#);
    let other = success(r#"{"widget":7,"scope":"other"}"#);
    let first_outcome = first
        .store
        .attempt(&scope(KEY), &INPUT, async |tx: &mut Tx<'_>| {
            enqueue_widget(tx, WIDGET).await;
            Ok(record.clone())
        })
        .await;
    assert_eq!(committed(first_outcome), record);
    let first_job_id = committed_job_id(&pool).await;

    let second_outcome = second
        .store
        .attempt(&scope(OTHER_KEY), &INPUT, async |tx: &mut Tx<'_>| {
            enqueue_widget(tx, DUPLICATE_WIDGET).await;
            Ok(other.clone())
        })
        .await;
    assert_eq!(committed(second_outcome), other);
    assert_eq!(committed_job_id(&pool).await, first_job_id);
    super::close_pools(&[&first.pool, &second.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_a_concurrent_enqueue_waits_and_both_scopes_commit_with_one_job(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let first_pool = first.pool.clone();
    let second_pool = second.pool.clone();
    let record = success(r#"{"widget":7}"#);
    let other = success(r#"{"widget":7,"scope":"other"}"#);
    let (entered_tx, entered_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();

    let first_record = record.clone();
    let first_task = tokio::spawn(async move {
        first
            .store
            .attempt(&scope(KEY), &INPUT, async move |tx: &mut Tx<'_>| {
                enqueue_widget(tx, WIDGET).await;
                entered_tx.send(()).expect("the entered receiver waits");
                release_rx.await.expect("the release sender sends");
                Ok(first_record)
            })
            .await
    });
    super::bounded("the held work to enter", entered_rx)
        .await
        .expect("the held work enters");

    let second_record = other.clone();
    let second_task = tokio::spawn(async move {
        second
            .store
            .attempt(&scope(OTHER_KEY), &INPUT, async |tx: &mut Tx<'_>| {
                enqueue_widget(tx, DUPLICATE_WIDGET).await;
                Ok(second_record)
            })
            .await
    });

    let second_outcome = super::join_after_lock_wait(
        &pool,
        async {
            release_tx.send(()).expect("the held work waits");
        },
        second_task,
    )
    .await;
    let first_outcome = super::bounded("the first attempt", first_task)
        .await
        .expect("the first attempt joins");
    assert_eq!(committed(first_outcome), record);
    assert_eq!(committed(second_outcome), other);
    let _id = committed_job_id(&pool).await;
    super::close_pools(&[&first_pool, &second_pool]).await;
}
