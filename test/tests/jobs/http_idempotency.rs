use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use infra_idempotency_store::{
    Attempted, Digest, Record, ScopeKey, Store, Tx, WorkOutput, connection,
};
use infra_jobs::{EnqueueOptions, Enqueued, JobKind, enqueue};
use infra_postgres::{Dsn, PgPool};
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
        let store = Store::new(pool.clone(), Duration::from_secs(3_600));
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

fn success(fingerprint: Digest, body: &str) -> Record {
    let mut headers = vec![1, 0, 16];
    headers.extend_from_slice(b"application/json");
    Record {
        fingerprint,
        format: 1,
        status: 201,
        headers,
        body: body.as_bytes().to_vec(),
    }
}

async fn replicas(pool: &PgPool) -> (Replica, Replica) {
    let dsn = dsn_for(pool).await;
    (Replica::open(&dsn).await, Replica::open(&dsn).await)
}

async fn enqueue_widget(tx: &mut Tx<'_>, widget: u64) -> Enqueued {
    let key = widget.to_string();
    enqueue(
        connection(tx),
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

fn committed(outcome: Attempted<()>) -> Record {
    match outcome {
        Attempted::Committed(record) => record,
        other => panic!("expected a commit, got {other:?}"),
    }
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e1_e3_a_committed_attempt_enqueues_once(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let record = success(INPUT, r#"{"widget":7}"#);
    let seen = Mutex::new(None);
    let outcome = first
        .store
        .attempt(
            &ScopeKey::from_digest(SCOPE),
            &[INPUT],
            async |tx: &mut Tx<'_>| -> WorkOutput<()> {
                let enqueued = enqueue_widget(tx, WIDGET).await;
                *seen.lock().expect("seen") = Some(enqueued);
                WorkOutput::Commit(record.clone())
            },
        )
        .await;
    assert_eq!(committed(outcome), record);
    let id = created_id(seen.lock().expect("seen").expect("the work enqueued"));
    assert_eq!(super::job_count(&pool).await, 1);
    assert!(row_is(&pool, &id, WIDGET).await);
    super::close(&[&first.pool, &second.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e3_a_replayed_attempt_enqueues_nothing(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let record = success(INPUT, r#"{"widget":7}"#);
    let runs = AtomicUsize::new(0);
    let first_outcome = first
        .store
        .attempt(
            &ScopeKey::from_digest(SCOPE),
            &[INPUT],
            async |tx: &mut Tx<'_>| -> WorkOutput<()> {
                runs.fetch_add(1, Ordering::SeqCst);
                let enqueued = enqueue_widget(tx, WIDGET).await;
                assert!(matches!(enqueued, Enqueued::Created(_)));
                WorkOutput::Commit(record.clone())
            },
        )
        .await;
    assert_eq!(committed(first_outcome), record);

    let replay = second
        .store
        .attempt(
            &ScopeKey::from_digest(SCOPE),
            &[INPUT],
            async |tx: &mut Tx<'_>| -> WorkOutput<()> {
                runs.fetch_add(1, Ordering::SeqCst);
                let _enqueued = enqueue_widget(tx, WIDGET).await;
                WorkOutput::Commit(success(INPUT, r#"{"widget":8}"#))
            },
        )
        .await;
    match replay {
        Attempted::Live {
            matched,
            record: stored,
        } => {
            assert!(matched);
            assert_eq!(stored, record);
        }
        other => panic!("expected a replay, got {other:?}"),
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(super::job_count(&pool).await, 1);
    super::close(&[&first.pool, &second.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e3_a_rolled_back_attempt_enqueues_nothing(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let runs = AtomicUsize::new(0);
    let outcome = first
        .store
        .attempt(
            &ScopeKey::from_digest(SCOPE),
            &[INPUT],
            async |tx: &mut Tx<'_>| -> WorkOutput<()> {
                runs.fetch_add(1, Ordering::SeqCst);
                let enqueued = enqueue_widget(tx, WIDGET).await;
                assert!(matches!(enqueued, Enqueued::Created(_)), "{enqueued:?}");
                WorkOutput::Rollback(())
            },
        )
        .await;
    assert!(matches!(outcome, Attempted::RolledBack(())));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(super::job_count(&pool).await, 0);
    super::close(&[&first.pool, &second.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e3_an_in_progress_attempt_enqueues_nothing(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let record = success(INPUT, r#"{"widget":7}"#);
    let runs = AtomicUsize::new(0);
    let seen = Mutex::new(None);
    let hold = Hold::new();
    let scope = ScopeKey::from_digest(SCOPE);
    hold.arm();
    let (held, ()) = tokio::join!(
        first.store.attempt(
            &scope,
            &[INPUT],
            async |tx: &mut Tx<'_>| -> WorkOutput<()> {
                runs.fetch_add(1, Ordering::SeqCst);
                let enqueued = enqueue_widget(tx, WIDGET).await;
                *seen.lock().expect("seen") = Some(enqueued);
                hold.pass().await;
                WorkOutput::Commit(record.clone())
            },
        ),
        async {
            hold.entered().await;
            let refused = second
                .store
                .attempt(
                    &scope,
                    &[INPUT],
                    async |tx: &mut Tx<'_>| -> WorkOutput<()> {
                        runs.fetch_add(1, Ordering::SeqCst);
                        let _enqueued = enqueue_widget(tx, WIDGET).await;
                        WorkOutput::Commit(record.clone())
                    },
                )
                .await;
            assert!(matches!(refused, Attempted::InProgress), "{refused:?}");
            assert_eq!(runs.load(Ordering::SeqCst), 1);
            assert_eq!(super::job_count(&pool).await, 0);
            hold.release();
        },
    );
    assert_eq!(committed(held), record);
    let id = created_id(seen.lock().expect("seen").expect("the held work enqueued"));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(super::job_count(&pool).await, 1);
    assert!(row_is(&pool, &id, WIDGET).await);
    super::close(&[&first.pool, &second.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_another_scope_gets_duplicate_and_still_commits(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let record = success(INPUT, r#"{"widget":7}"#);
    let other = success(INPUT, r#"{"widget":7,"scope":"other"}"#);
    let first_outcome = first
        .store
        .attempt(
            &ScopeKey::from_digest(SCOPE),
            &[INPUT],
            async |tx: &mut Tx<'_>| -> WorkOutput<()> {
                let enqueued = enqueue_widget(tx, WIDGET).await;
                assert!(matches!(enqueued, Enqueued::Created(_)));
                WorkOutput::Commit(record.clone())
            },
        )
        .await;
    assert_eq!(committed(first_outcome), record);

    let seen = Mutex::new(None);
    let second_outcome = second
        .store
        .attempt(
            &ScopeKey::from_digest(OTHER),
            &[INPUT],
            async |tx: &mut Tx<'_>| -> WorkOutput<()> {
                let enqueued = enqueue_widget(tx, WIDGET).await;
                *seen.lock().expect("seen") = Some(enqueued);
                WorkOutput::Commit(other.clone())
            },
        )
        .await;
    assert_eq!(committed(second_outcome), other);
    assert_eq!(
        seen.lock()
            .expect("seen")
            .expect("the second work enqueued"),
        Enqueued::Duplicate
    );
    assert_eq!(super::job_count(&pool).await, 1);
    super::close(&[&first.pool, &second.pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_a_concurrent_enqueue_waits_then_duplicate_and_both_commit(pool: PgPool) {
    let (first, second) = replicas(&pool).await;
    let first_pool = first.pool.clone();
    let second_pool = second.pool.clone();
    let record = success(INPUT, r#"{"widget":7}"#);
    let other = success(INPUT, r#"{"widget":7,"scope":"other"}"#);
    let hold = Arc::new(Hold::new());
    let seen_first = Arc::new(Mutex::new(None));
    let seen_second = Arc::new(Mutex::new(None));
    hold.arm();

    let first_hold = Arc::clone(&hold);
    let first_seen = Arc::clone(&seen_first);
    let first_record = record.clone();
    let first_task = tokio::spawn(async move {
        first
            .store
            .attempt(
                &ScopeKey::from_digest(SCOPE),
                &[INPUT],
                async |tx: &mut Tx<'_>| -> WorkOutput<()> {
                    let enqueued = enqueue_widget(tx, WIDGET).await;
                    *first_seen.lock().expect("seen") = Some(enqueued);
                    first_hold.pass().await;
                    WorkOutput::Commit(first_record)
                },
            )
            .await
    });
    hold.entered().await;

    let second_seen = Arc::clone(&seen_second);
    let second_record = other.clone();
    let second_task = tokio::spawn(async move {
        second
            .store
            .attempt(
                &ScopeKey::from_digest(OTHER),
                &[INPUT],
                async |tx: &mut Tx<'_>| -> WorkOutput<()> {
                    let enqueued = enqueue_widget(tx, WIDGET).await;
                    *second_seen.lock().expect("seen") = Some(enqueued);
                    WorkOutput::Commit(second_record)
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
    assert_eq!(committed(first_outcome), record);
    assert_eq!(committed(second_outcome), other);
    let id = created_id(
        seen_first
            .lock()
            .expect("seen")
            .expect("the first work enqueued"),
    );
    assert_eq!(
        seen_second
            .lock()
            .expect("seen")
            .expect("the second work enqueued"),
        Enqueued::Duplicate
    );
    assert_eq!(super::job_count(&pool).await, 1);
    assert!(row_is(&pool, &id, WIDGET).await);
    super::close(&[&first_pool, &second_pool]).await;
}
