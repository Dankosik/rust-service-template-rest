use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use infra_idempotency_store::{
    AttemptError, Attempted, CallerIdentity, CallerKind, Digest, HeaderPair, Record, ScopeKey,
    Store,
};
use infra_jobs::{EnqueueOptions, Enqueued, JobKind, enqueue};
use infra_postgres::{Dsn, PgPool, Tx};
use integration_tests::dsn_for;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

#[derive(Debug, Serialize, Deserialize)]
struct WidgetWelcome {
    widget: u64,
}

impl JobKind for WidgetWelcome {
    const NAME: &'static str = "widgets.welcome";
}

type Committing<C = Record> = Result<(Record, C), ()>;

/// A record to commit that the attempt also hands back as its value.
fn returned(record: Record) -> (Record, Record) {
    (record.clone(), record)
}

fn with_enqueued(record: Record, enqueued: Enqueued) -> (Record, (Record, Enqueued)) {
    (record.clone(), (record, enqueued))
}

const SCOPE: Digest = [0x11; 32];
const OTHER: Digest = [0x22; 32];
const INPUT: Digest = [0xa1; 32];
const WIDGET: u64 = 7;

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

struct Hold {
    armed: std::sync::atomic::AtomicBool,
    entered: Notify,
    release: Notify,
}

impl Hold {
    fn new() -> Self {
        Self {
            armed: std::sync::atomic::AtomicBool::new(false),
            entered: Notify::new(),
            release: Notify::new(),
        }
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    async fn pass(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }

    async fn entered(&self) {
        super::bounded("the held work to enter", self.entered.notified()).await;
    }

    fn release(&self) {
        self.release.notify_one();
    }
}

fn caller() -> CallerIdentity {
    CallerIdentity {
        issuer: "https://issuer.example".to_owned(),
        kind: CallerKind::Subject,
        value: "fixture-subject".to_owned(),
    }
}

fn success(body: &str) -> Record {
    Record {
        status: 201,
        headers: vec![HeaderPair {
            name: "content-type".to_owned(),
            value: b"application/json".to_vec(),
        }],
        body: body.as_bytes().to_vec().into(),
    }
}

async fn replicas(pool: &PgPool) -> (Replica, Replica) {
    let dsn = dsn_for(pool).await;
    (Replica::open(&dsn).await, Replica::open(&dsn).await)
}

async fn enqueue_widget(tx: &mut Tx<'_>, widget: u64) -> Enqueued {
    let key = widget.to_string();
    enqueue(
        tx,
        &WidgetWelcome { widget },
        EnqueueOptions {
            delay: Duration::ZERO,
            unique_key: Some(&key),
        },
    )
    .await
    .expect("the work enqueues")
}

fn created_id(enqueued: Enqueued) -> String {
    match enqueued {
        Enqueued::Created(id) => id.to_string(),
        Enqueued::Duplicate => panic!("expected Created, got Duplicate"),
    }
}

async fn row_is(pool: &PgPool, id: &str, widget: u64) -> bool {
    let payload = serde_json::to_string(&WidgetWelcome { widget }).expect("payload text");
    sqlx::query_scalar(
        "SELECT id::text = $1 \
         AND kind = $2 \
         AND payload = $3::jsonb \
         AND unique_key = $4 \
         AND state = 'pending' \
         FROM background_jobs",
    )
    .bind(id)
    .bind(WidgetWelcome::NAME)
    .bind(payload)
    .bind(widget.to_string())
    .fetch_one(pool)
    .await
    .expect("the job row")
}

fn committed<C: std::fmt::Debug>(outcome: Result<Attempted<C, ()>, AttemptError>) -> C {
    match outcome {
        Ok(Attempted::Committed(value)) => value,
        other => panic!("expected a commit, got {other:?}"),
    }
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e1_e3_a_committed_attempt_enqueues_once(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let record = success(r#"{"widget":7}"#);
    let outcome = first
        .store
        .attempt(
            &ScopeKey::from_digest(SCOPE),
            &caller(),
            &INPUT,
            async |tx: &mut Tx<'_>| -> Committing<_> {
                let enqueued = enqueue_widget(tx, WIDGET).await;
                Ok(with_enqueued(record.clone(), enqueued))
            },
        )
        .await;
    let (committed_record, enqueued) = committed(outcome);
    assert_eq!(committed_record, record);
    let id = created_id(enqueued);
    assert_eq!(super::job_count(&pool).await, 1);
    assert!(row_is(&pool, &id, WIDGET).await);
    super::close_pools(&[&first.pool, &second.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e3_a_replayed_attempt_enqueues_nothing(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let record = success(r#"{"widget":7}"#);
    let runs = AtomicUsize::new(0);
    let first_outcome = first
        .store
        .attempt(
            &ScopeKey::from_digest(SCOPE),
            &caller(),
            &INPUT,
            async |tx: &mut Tx<'_>| -> Committing {
                runs.fetch_add(1, Ordering::SeqCst);
                let enqueued = enqueue_widget(tx, WIDGET).await;
                std::assert_matches!(enqueued, Enqueued::Created(_));
                Ok(returned(record.clone()))
            },
        )
        .await;
    assert_eq!(committed(first_outcome), record);

    let replay = second
        .store
        .attempt(
            &ScopeKey::from_digest(SCOPE),
            &caller(),
            &INPUT,
            async |tx: &mut Tx<'_>| -> Committing {
                runs.fetch_add(1, Ordering::SeqCst);
                let _enqueued = enqueue_widget(tx, WIDGET).await;
                Ok(returned(success(r#"{"widget":8}"#)))
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
    let (first, second) = replicas(&pool).await;
    let runs = AtomicUsize::new(0);
    let outcome = first
        .store
        .attempt(
            &ScopeKey::from_digest(SCOPE),
            &caller(),
            &INPUT,
            async |tx: &mut Tx<'_>| -> Committing {
                runs.fetch_add(1, Ordering::SeqCst);
                let enqueued = enqueue_widget(tx, WIDGET).await;
                std::assert_matches!(enqueued, Enqueued::Created(_), "{enqueued:?}");
                Err(())
            },
        )
        .await;
    std::assert_matches!(outcome, Ok(Attempted::RolledBack(())));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(super::job_count(&pool).await, 0);
    super::close_pools(&[&first.pool, &second.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e3_an_in_progress_attempt_enqueues_nothing(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let record = success(r#"{"widget":7}"#);
    let runs = AtomicUsize::new(0);
    let hold = Hold::new();
    let scope = ScopeKey::from_digest(SCOPE);
    let first_caller = caller();
    let second_caller = caller();
    hold.arm();
    let (held, ()) = tokio::join!(
        first.store.attempt(
            &scope,
            &first_caller,
            &INPUT,
            async |tx: &mut Tx<'_>| -> Committing<_> {
                runs.fetch_add(1, Ordering::SeqCst);
                let enqueued = enqueue_widget(tx, WIDGET).await;
                hold.pass().await;
                Ok(with_enqueued(record.clone(), enqueued))
            },
        ),
        async {
            hold.entered().await;
            let refused = second
                .store
                .attempt(
                    &scope,
                    &second_caller,
                    &INPUT,
                    async |tx: &mut Tx<'_>| -> Committing {
                        runs.fetch_add(1, Ordering::SeqCst);
                        let _enqueued = enqueue_widget(tx, WIDGET).await;
                        Ok(returned(record.clone()))
                    },
                )
                .await;
            std::assert_matches!(refused, Ok(Attempted::InProgress), "{refused:?}");
            assert_eq!(runs.load(Ordering::SeqCst), 1);
            assert_eq!(super::job_count(&pool).await, 0);
            hold.release();
        },
    );
    let (committed_record, enqueued) = committed(held);
    assert_eq!(committed_record, record);
    let id = created_id(enqueued);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(super::job_count(&pool).await, 1);
    assert!(row_is(&pool, &id, WIDGET).await);
    super::close_pools(&[&first.pool, &second.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_another_scope_gets_duplicate_and_still_commits(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let record = success(r#"{"widget":7}"#);
    let other = success(r#"{"widget":7,"scope":"other"}"#);
    let first_outcome = first
        .store
        .attempt(
            &ScopeKey::from_digest(SCOPE),
            &caller(),
            &INPUT,
            async |tx: &mut Tx<'_>| -> Committing {
                let enqueued = enqueue_widget(tx, WIDGET).await;
                std::assert_matches!(enqueued, Enqueued::Created(_));
                Ok(returned(record.clone()))
            },
        )
        .await;
    assert_eq!(committed(first_outcome), record);

    let second_outcome = second
        .store
        .attempt(
            &ScopeKey::from_digest(OTHER),
            &caller(),
            &INPUT,
            async |tx: &mut Tx<'_>| -> Committing<_> {
                let enqueued = enqueue_widget(tx, WIDGET).await;
                Ok(with_enqueued(other.clone(), enqueued))
            },
        )
        .await;
    let (committed_record, enqueued) = committed(second_outcome);
    assert_eq!(committed_record, other);
    assert_eq!(enqueued, Enqueued::Duplicate);
    assert_eq!(super::job_count(&pool).await, 1);
    super::close_pools(&[&first.pool, &second.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_a_concurrent_enqueue_waits_then_duplicate_and_both_commit(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let first_pool = first.pool.clone();
    let second_pool = second.pool.clone();
    let record = success(r#"{"widget":7}"#);
    let other = success(r#"{"widget":7,"scope":"other"}"#);
    let hold = Arc::new(Hold::new());
    hold.arm();

    let first_hold = Arc::clone(&hold);
    let first_record = record.clone();
    let first_task = tokio::spawn(async move {
        first
            .store
            .attempt(
                &ScopeKey::from_digest(SCOPE),
                &caller(),
                &INPUT,
                async |tx: &mut Tx<'_>| -> Committing<_> {
                    let enqueued = enqueue_widget(tx, WIDGET).await;
                    first_hold.pass().await;
                    Ok(with_enqueued(first_record, enqueued))
                },
            )
            .await
    });
    hold.entered().await;

    let second_record = other.clone();
    let second_task = tokio::spawn(async move {
        second
            .store
            .attempt(
                &ScopeKey::from_digest(OTHER),
                &caller(),
                &INPUT,
                async |tx: &mut Tx<'_>| -> Committing<_> {
                    let enqueued = enqueue_widget(tx, WIDGET).await;
                    Ok(with_enqueued(second_record, enqueued))
                },
            )
            .await
    });

    let second_outcome = super::join_after_lock_wait(
        &pool,
        async {
            hold.release();
        },
        second_task,
    )
    .await;
    let first_outcome = super::bounded("the first attempt", first_task)
        .await
        .expect("the first attempt joins");
    let (first_record, first_enqueued) = committed(first_outcome);
    let (second_record, second_enqueued) = committed(second_outcome);
    assert_eq!(first_record, record);
    assert_eq!(second_record, other);
    let id = created_id(first_enqueued);
    assert_eq!(second_enqueued, Enqueued::Duplicate);
    assert_eq!(super::job_count(&pool).await, 1);
    assert!(row_is(&pool, &id, WIDGET).await);
    super::close_pools(&[&first_pool, &second_pool]).await;
}
