//! Real-PostgreSQL proof for the HTTP idempotency record store (P1-P8).
//!
//! Every test gets its own database from `#[sqlx::test]`, migrated with the
//! embedded set where the profile table is needed. Two independent template
//! pools on it stand for two replicas. Scopes and fingerprints are fixed raw
//! digests; the caller is persisted as separately inspectable metadata. The
//! work's effect is a row in a test-owned table written through the provider
//! transaction capability,
//! and the tests count effects and work runs themselves. Every wait is bounded
//! and every spawned task is joined.
//!
//! while the mounted proof exercises request identity and HTTP responses.

#![cfg(feature = "integration")]
// Integration tests are test code; the workspace's production lint levels
// for unwrap/expect/panic do not apply to them.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

// template:begin http-idempotency-mounted:http-idempotency-mounted-module
mod mounted;
// template:end http-idempotency-mounted:http-idempotency-mounted-module

use std::fmt::Write as _;
use std::future::Future;
use std::num::NonZeroU32;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::FutureExt as _;
use futures_util::future::join_all;
use infra_idempotency_store::{
    Attempted, CallerIdentity, CallerKind, Digest, HeaderPair, Record, ScopeKey, StartupError,
    Store, WorkOutput,
};
use infra_postgres::{Closed, Dsn, Isolation, PgPool, PoolOptions, Tx, connection};
use integration_tests::dsn_for;
use sqlx::Executor as _;
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const APP: &str = "integration-tests-idempotency";
/// How long the replicas' records stay live.
const RETENTION: Duration = Duration::from_secs(3_600);
/// Bound on every wait in this suite.
const WAIT: Duration = Duration::from_secs(10);
/// Pause between retries while a dropped attempt's transaction ends.
const RETRY_PAUSE: Duration = Duration::from_millis(20);
/// Bound on closing a pool.
const CLOSE_BUDGET: Duration = Duration::from_secs(5);
/// Bound on the cleanup task's return after cancellation.
const CANCEL_BUDGET: Duration = Duration::from_secs(1);

// Scope digests. The advisory lock key is a digest's first eight bytes, so
// every scope differs there.
const SCOPE: Digest = [0x11; 32];
const OTHER_CALLER: Digest = [0x12; 32];
const OTHER_SCOPE: Digest = [0x13; 32];
// Fingerprint digests.
const INPUT: Digest = [0xa1; 32];
const OTHER_INPUT: Digest = [0xa2; 32];

/// Expired records to clean: more than two batches of 500.
const BACKLOG: i64 = 1_201;
/// Live records beside the backlog.
const LIVE: i64 = 10;

const EFFECTS: &str = "SELECT count(*) FROM effects";
const EXPIRED: &str = "SELECT count(*) FROM http_idempotency_records WHERE expires_at <= now()";
const LIVE_RECORDS: &str = "SELECT count(*) FROM http_idempotency_records WHERE expires_at > now()";
const SEED_EXPIRED: &str = "INSERT INTO http_idempotency_records \
    (scope_key, fingerprint, status, headers, body, issuer, caller_kind, caller_value, expires_at) \
    SELECT sha256(int8send(n)), sha256(int8send(n)), 201, \
    ARRAY[ROW('content-type', convert_to('application/json', 'UTF8'))::http_idempotency_header_pair], \
    '{}', 'https://issuer.example', 'subject', 'fixture-subject', now() - interval '1 hour' \
    FROM generate_series(1, $1::bigint) AS n";
const SEED_LIVE: &str = "INSERT INTO http_idempotency_records \
    (scope_key, fingerprint, status, headers, body, issuer, caller_kind, caller_value, expires_at) \
    SELECT sha256(int8send(n)), sha256(int8send(n)), 201, \
    ARRAY[ROW('content-type', convert_to('application/json', 'UTF8'))::http_idempotency_header_pair], \
    '{}', 'https://issuer.example', 'subject', 'fixture-subject', now() + interval '1 hour' \
    FROM generate_series(1000001, 1000000 + $1::bigint) AS n";
/// Locks the row of the first seeded expired record.
const LOCK_SEEDED_ROW: &str = "SELECT 1 FROM http_idempotency_records \
    WHERE scope_key = sha256(int8send(1::bigint)) FOR UPDATE";

/// The template pool on `dsn`: the service's session defaults and budgets.
async fn template_pool(dsn: &Dsn, max_connections: u32) -> PgPool {
    infra_postgres::connect(
        dsn,
        &PoolOptions {
            max_connections: NonZeroU32::new(max_connections).expect("a pool size"),
            application_name: APP,
            default_isolation: Isolation::ServerDefault,
        },
    )
    .await
    .expect("the template pool connects")
}

/// One replica: an independent template pool on the per-test database, and
/// the store over it.
async fn replica(dsn: &Dsn) -> (PgPool, Store) {
    let pool = template_pool(dsn, 3).await;
    (pool.clone(), Store::new(pool, RETENTION))
}

/// Close each pool within its budget.
async fn close(pools: &[&PgPool]) {
    for pool in pools {
        assert_eq!(
            infra_postgres::close(pool, CLOSE_BUDGET).await,
            Closed::Complete
        );
    }
}

/// Make every later session on the per-test database read-only by default;
/// sessions already open keep their mode.
async fn make_read_only(pool: &PgPool) {
    pool.execute(
        "DO $$ BEGIN EXECUTE format(\
         'ALTER DATABASE %I SET default_transaction_read_only = on', current_database()); END $$",
    )
    .await
    .expect("the database default changes");
}

async fn create_effects(pool: &PgPool) {
    pool.execute("CREATE TABLE effects (id bigserial PRIMARY KEY)")
        .await
        .expect("the effect table");
}

async fn count(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(pool)
        .await
        .expect("a count")
}

/// How many records, live or expired, `scope` has.
async fn records(pool: &PgPool, scope: Digest) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM http_idempotency_records WHERE scope_key = $1")
        .bind(scope)
        .fetch_one(pool)
        .await
        .expect("a record count")
}

/// Move the record of `scope` into the past on the database clock.
async fn expire(pool: &PgPool, scope: Digest) {
    let moved = sqlx::query(
        "UPDATE http_idempotency_records SET expires_at = now() - interval '1 second' \
         WHERE scope_key = $1",
    )
    .bind(scope)
    .execute(pool)
    .await
    .expect("the expiry moves");
    assert_eq!(moved.rows_affected(), 1);
}

/// Insert `records` records with `sql`, bypassing the store.
async fn seed(pool: &PgPool, sql: &'static str, records: i64) {
    let seeded = sqlx::query(sql)
        .bind(records)
        .execute(pool)
        .await
        .expect("the records are seeded");
    assert_eq!(i64::try_from(seeded.rows_affected()), Ok(records));
}

/// A stored success as the seam encodes it: status 201, all replayable
/// headers, and `body`.
fn success(fingerprint: Digest, body: &str) -> Record {
    Record {
        fingerprint,
        status: 201,
        headers: vec![
            HeaderPair {
                name: "content-type".to_owned(),
                value: b"application/json".to_vec(),
            },
            HeaderPair {
                name: "content-encoding".to_owned(),
                value: b"br".to_vec(),
            },
            HeaderPair {
                name: "content-language".to_owned(),
                value: b"en".to_vec(),
            },
            HeaderPair {
                name: "content-language".to_owned(),
                value: b"fr".to_vec(),
            },
            HeaderPair {
                name: "content-disposition".to_owned(),
                value: b"attachment; filename=widget.json".to_vec(),
            },
            HeaderPair {
                name: "location".to_owned(),
                value: b"/widgets/1".to_vec(),
            },
            HeaderPair {
                name: "etag".to_owned(),
                value: vec![b'\"', 0x80, b'\"'],
            },
            HeaderPair {
                name: "last-modified".to_owned(),
                value: b"Sun, 06 Nov 1994 08:49:37 GMT".to_vec(),
            },
        ],
        body: body.as_bytes().to_vec(),
    }
}

fn caller(value: &str) -> CallerIdentity {
    CallerIdentity {
        issuer: "https://issuer.example".to_owned(),
        kind: CallerKind::Subject,
        value: value.to_owned(),
    }
}

/// Holds the next armed work inside its transaction, after its effect, until
/// the test releases it.
#[derive(Debug, Default)]
struct Hold {
    armed: AtomicBool,
    entered: Notify,
    release: Notify,
}

impl Hold {
    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    /// Called by the work: an armed hold signals its entry and waits for the
    /// release; otherwise it returns at once.
    async fn pass(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }

    /// Wait until the held work has entered.
    async fn entered(&self) {
        bounded("the held work to enter", self.entered.notified()).await;
    }

    fn release(&self) {
        self.release.notify_one();
    }
}

/// The operation's work: it counts its runs and writes one effect row per run
/// inside the attempt's transaction.
#[derive(Debug, Default)]
struct Work {
    runs: AtomicUsize,
    hold: Hold,
}

impl Work {
    async fn run(&self, tx: &mut Tx<'_>) {
        self.runs.fetch_add(1, Ordering::SeqCst);
        sqlx::query("INSERT INTO effects DEFAULT VALUES")
            .execute(connection(tx))
            .await
            .expect("the effect is written");
        self.hold.pass().await;
    }

    fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }
}

/// Why a work asks for rollback. The seam rolls back a non-2xx response and
/// an unstorable success alike; the store sees only the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    NotSuccess,
    Unstorable,
}

/// One attempt at `scope` and `fingerprint`, whose work, if it runs,
/// writes an effect and commits `record`.
async fn execute(
    store: &Store,
    scope: Digest,
    fingerprint: Digest,
    record: &Record,
    work: &Work,
) -> Attempted<Refusal> {
    let caller = caller("fixture-subject");
    store
        .attempt(
            &ScopeKey::from_digest(scope),
            &caller,
            &fingerprint,
            async |tx: &mut Tx<'_>| {
                work.run(tx).await;
                WorkOutput::Commit(record.clone())
            },
        )
        .await
}

/// [`execute`] until the key is free: a dropped attempt holds it until
/// `PostgreSQL` ends that attempt's transaction.
async fn once_free(
    store: &Store,
    scope: Digest,
    record: &Record,
    work: &Work,
) -> Attempted<Refusal> {
    let deadline = Instant::now() + WAIT;
    loop {
        let outcome = execute(store, scope, record.fingerprint, record, work).await;
        if !matches!(outcome, Attempted::InProgress) {
            return outcome;
        }
        assert!(
            Instant::now() < deadline,
            "the key stayed held for {WAIT:?}"
        );
        tokio::time::sleep(RETRY_PAUSE).await;
    }
}

/// `future`, which must finish within [`WAIT`].
async fn bounded<F: Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(WAIT, future).await.expect(what)
}

fn committed(outcome: Attempted<Refusal>) -> Record {
    match outcome {
        Attempted::Committed(record) => record,
        unexpected => panic!("expected a commit, got {unexpected:?}"),
    }
}

/// Whether the live record that decided an attempt matched, and the record.
fn live(outcome: Attempted<Refusal>) -> (bool, Record) {
    match outcome {
        Attempted::Live { matched, record } => (matched, record),
        unexpected => panic!("expected a live record, got {unexpected:?}"),
    }
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p1_a_held_key_refuses_duplicates_and_commits_one_effect(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (pool_1, replica_1) = replica(&dsn).await;
    let (pool_2, replica_2) = replica(&dsn).await;
    let record = success(INPUT, r#"{"id":1}"#);
    let work = Work::default();

    // One attempt holds the key inside its work. Duplicates on either
    // replica, with the same or another input, are in progress at once and
    // never run the work.
    work.hold.arm();
    let (held, ()) = tokio::join!(execute(&replica_1, SCOPE, INPUT, &record, &work), async {
        work.hold.entered().await;
        for replica in [&replica_1, &replica_2] {
            for fingerprint in [INPUT, OTHER_INPUT] {
                let duplicate = execute(replica, SCOPE, fingerprint, &record, &work).await;
                assert!(matches!(duplicate, Attempted::InProgress), "{duplicate:?}");
            }
        }
        assert_eq!(work.runs(), 1, "no duplicate ran its work");
        work.hold.release();
    },);
    assert_eq!(committed(held), record);

    // After the commit, retries replay on both replicas, one after another
    // and all at once, and none runs the work.
    for replica in [&replica_1, &replica_2] {
        let retry = execute(replica, SCOPE, INPUT, &record, &work).await;
        assert_eq!(live(retry), (true, record.clone()));
    }
    let concurrent = [
        &replica_1, &replica_2, &replica_1, &replica_2, &replica_1, &replica_2,
    ]
    .map(|replica| execute(replica, SCOPE, INPUT, &record, &work));
    for retry in join_all(concurrent).await {
        assert_eq!(live(retry), (true, record.clone()));
    }
    assert_eq!(work.runs(), 1);
    assert_eq!(count(&pool, EFFECTS).await, 1);
    close(&[&pool_1, &pool_2]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p2_rolled_back_work_leaves_no_effect_or_record_and_the_retry_executes(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (pool_1, replica_1) = replica(&dsn).await;
    let (pool_2, replica_2) = replica(&dsn).await;
    let record = success(INPUT, r#"{"id":1}"#);
    let work = Work::default();

    let caller = caller("fixture-subject");
    // A non-2xx response and an unstorable success both ask for rollback
    // after the work wrote its effect.
    for (scope, refusal) in [
        (SCOPE, Refusal::NotSuccess),
        (OTHER_CALLER, Refusal::Unstorable),
    ] {
        let outcome = replica_1
            .attempt(
                &ScopeKey::from_digest(scope),
                &caller,
                &INPUT,
                async |tx: &mut Tx<'_>| {
                    work.run(tx).await;
                    WorkOutput::Rollback(refusal)
                },
            )
            .await;
        assert!(
            matches!(outcome, Attempted::RolledBack(returned) if returned == refusal),
            "{outcome:?}"
        );
    }
    // A panic in the work unwinds out of the attempt and drops its
    // transaction.
    let panicked = AssertUnwindSafe(replica_1.attempt(
        &ScopeKey::from_digest(OTHER_SCOPE),
        &caller,
        &INPUT,
        async |tx: &mut Tx<'_>| -> WorkOutput<Refusal> {
            work.run(tx).await;
            panic!("the work panics after writing its effect");
        },
    ))
    .catch_unwind()
    .await;
    assert!(panicked.is_err());
    assert_eq!(work.runs(), 3);
    assert_eq!(count(&pool, EFFECTS).await, 0);

    // Nothing was stored, so each retry executes, here on the other replica.
    for scope in [SCOPE, OTHER_CALLER, OTHER_SCOPE] {
        assert_eq!(records(&pool, scope).await, 0);
        let retry = once_free(&replica_2, scope, &record, &work).await;
        assert_eq!(committed(retry), record);
    }
    assert_eq!(work.runs(), 6);
    assert_eq!(count(&pool, EFFECTS).await, 3);
    close(&[&pool_1, &pool_2]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p3_a_replay_returns_the_stored_bytes_and_other_scopes_are_independent(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (pool_1, replica_1) = replica(&dsn).await;
    let (pool_2, replica_2) = replica(&dsn).await;
    let record = success(INPUT, r#"{"id":1,"name":"first"}"#);
    let other = success(OTHER_INPUT, r#"{"id":2,"name":"other"}"#);
    let work = Work::default();
    let first = execute(&replica_1, SCOPE, INPUT, &record, &work).await;
    assert_eq!(committed(first), record);

    // The same fingerprint gets the stored status, headers, and body bytes on
    // the other replica, not what its own work would have produced.
    let replayed = execute(&replica_2, SCOPE, INPUT, &other, &work).await;
    assert_eq!(live(replayed), (true, record.clone()));
    // Another fingerprint is refused by the live record, which stays.
    let mismatched = execute(&replica_2, SCOPE, OTHER_INPUT, &other, &work).await;
    assert_eq!(live(mismatched), (false, record.clone()));
    assert_eq!(work.runs(), 1);

    // Other scopes hold keys of their own.
    for scope in [OTHER_CALLER, OTHER_SCOPE] {
        let independent = execute(&replica_2, scope, INPUT, &other, &work).await;
        assert_eq!(committed(independent), other);
    }
    assert_eq!(work.runs(), 3);
    assert_eq!(count(&pool, EFFECTS).await, 3);
    let metadata: (String, String, String) = sqlx::query_as(
        "SELECT issuer, caller_kind, caller_value FROM http_idempotency_records WHERE scope_key = $1",
    )
    .bind(SCOPE)
    .fetch_one(&pool)
    .await
    .expect("the stored caller metadata");
    assert_eq!(
        metadata,
        (
            "https://issuer.example".to_owned(),
            "subject".to_owned(),
            "fixture-subject".to_owned()
        )
    );
    close(&[&pool_1, &pool_2]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p3_a_long_verified_caller_identity_commits_and_replays(pool: PgPool) {
    create_effects(&pool).await;
    let (store_pool, store) = replica(&dsn_for(&pool).await).await;
    // This is deliberately larger than PostgreSQL's btree entry limit and
    // non-repeating. A raw caller-value index would reject the committed
    // record; digest-backed scope identity must preserve the caller domain.
    let mut caller_value = String::new();
    for part in 0..2_048 {
        write!(&mut caller_value, "caller-{part:08x};").expect("String writes do not fail");
    }
    let caller = caller(&caller_value);
    let record = success(INPUT, r#"{"id":1,"name":"long-caller"}"#);

    let first: Attempted<()> = store
        .attempt(
            &ScopeKey::from_digest(SCOPE),
            &caller,
            &INPUT,
            async |tx: &mut Tx<'_>| {
                sqlx::query("INSERT INTO effects DEFAULT VALUES")
                    .execute(connection(tx))
                    .await
                    .expect("the committed effect");
                WorkOutput::Commit(record.clone())
            },
        )
        .await;
    match first {
        Attempted::Committed(actual) => assert_eq!(actual, record),
        unexpected => panic!("expected a committed long-caller record, got {unexpected:?}"),
    }

    let replayed: Attempted<()> = store
        .attempt(
            &ScopeKey::from_digest(SCOPE),
            &caller,
            &INPUT,
            async |tx: &mut Tx<'_>| {
                sqlx::query("INSERT INTO effects DEFAULT VALUES")
                    .execute(connection(tx))
                    .await
                    .expect("a replay must not run this effect");
                WorkOutput::Commit(record.clone())
            },
        )
        .await;
    match replayed {
        Attempted::Live {
            matched: true,
            record: actual,
        } => assert_eq!(actual, record),
        unexpected => panic!("expected a matching long-caller replay, got {unexpected:?}"),
    }
    assert_eq!(count(&pool, EFFECTS).await, 1);
    let stored: String = sqlx::query_scalar(
        "SELECT caller_value FROM http_idempotency_records WHERE scope_key = $1",
    )
    .bind(SCOPE)
    .fetch_one(&pool)
    .await
    .expect("the stored caller identity");
    assert_eq!(stored, caller_value);
    close(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p4_an_expired_record_is_not_replayed_and_is_replaced(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (pool_1, replica_1) = replica(&dsn).await;
    let (pool_2, replica_2) = replica(&dsn).await;
    let record = success(INPUT, r#"{"id":1}"#);
    let replacement = success(OTHER_INPUT, r#"{"id":2}"#);
    let work = Work::default();
    let first = execute(&replica_1, SCOPE, INPUT, &record, &work).await;
    assert_eq!(committed(first), record);
    expire(&pool, SCOPE).await;

    // The key executes afresh, whatever the input, and the new record
    // replaces the expired one.
    let afresh = execute(&replica_2, SCOPE, OTHER_INPUT, &replacement, &work).await;
    assert_eq!(committed(afresh), replacement);
    let decided = execute(&replica_1, SCOPE, INPUT, &record, &work).await;
    assert_eq!(live(decided), (false, replacement.clone()));
    assert_eq!(records(&pool, SCOPE).await, 1);
    assert_eq!(work.runs(), 2);
    assert_eq!(count(&pool, EFFECTS).await, 2);
    close(&[&pool_1, &pool_2]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p4_a_retention_with_a_sub_microsecond_part_writes_its_record(pool: PgPool) {
    create_effects(&pool).await;
    let store_pool = template_pool(&dsn_for(&pool).await, 1).await;
    // Configuration accepts nanosecond units; `sqlx` binds an interval only
    // in whole microseconds.
    let store = Store::new(store_pool.clone(), Duration::new(3_600, 123_456_789));
    let record = success(INPUT, r#"{"id":1}"#);
    let written = execute(&store, SCOPE, INPUT, &record, &Work::default()).await;
    assert_eq!(committed(written), record);
    let on_time: bool = sqlx::query_scalar(
        "SELECT expires_at BETWEEN now() + interval '3599 seconds' \
         AND now() + interval '3601 seconds' \
         FROM http_idempotency_records WHERE scope_key = $1",
    )
    .bind(SCOPE)
    .fetch_one(&pool)
    .await
    .expect("the record's expiry");
    assert!(on_time, "the record lives for the retention");
    close(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p4_cleanup_drains_the_backlog_and_keeps_live_and_held_records(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (pool_1, replica_1) = replica(&dsn).await;
    let (pool_2, replica_2) = replica(&dsn).await;
    let record = success(INPUT, r#"{"id":1}"#);
    let work = Work::default();
    // The attempt that executes below replaces an expired record of its own.
    let first = execute(&replica_1, SCOPE, INPUT, &record, &work).await;
    assert_eq!(committed(first), record);
    expire(&pool, SCOPE).await;
    seed(&pool, SEED_EXPIRED, BACKLOG).await;
    seed(&pool, SEED_LIVE, LIVE).await;
    // One expired row stays locked, as an attempt's write holds it until its
    // commit.
    let mut holder = pool.begin().await.expect("a row-lock holder");
    sqlx::query(LOCK_SEEDED_ROW)
        .execute(&mut *holder)
        .await
        .expect("the row lock");

    work.hold.arm();
    let (executed, removed) =
        tokio::join!(execute(&replica_1, SCOPE, INPUT, &record, &work), async {
            work.hold.entered().await;
            let removed = bounded("a cleanup run", replica_2.remove_expired()).await;
            // The executing attempt still holds its key.
            let duplicate = execute(&replica_2, SCOPE, INPUT, &record, &work).await;
            assert!(matches!(duplicate, Attempted::InProgress), "{duplicate:?}");
            work.hold.release();
            removed
        },);
    // Every expired record went, in more than one batch: the backlog less its
    // locked row, which was skipped without waiting, plus the executing
    // attempt's own expired record.
    let drained = u64::try_from(BACKLOG).expect("a row count");
    assert_eq!(removed, Ok(drained));
    assert_eq!(committed(executed), record);
    assert_eq!(count(&pool, EXPIRED).await, 1);
    assert_eq!(count(&pool, LIVE_RECORDS).await, LIVE + 1);

    holder.rollback().await.expect("the row lock ends");
    assert_eq!(replica_2.remove_expired().await, Ok(1));
    assert_eq!(count(&pool, EXPIRED).await, 0);
    assert_eq!(count(&pool, LIVE_RECORDS).await, LIVE + 1);
    assert_eq!(work.runs(), 2);
    assert_eq!(count(&pool, EFFECTS).await, 2);
    close(&[&pool_1, &pool_2]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p4_the_cleanup_task_runs_at_once_and_returns_promptly_on_cancel(pool: PgPool) {
    let (store_pool, store) = replica(&dsn_for(&pool).await).await;
    seed(&pool, SEED_EXPIRED, BACKLOG).await;
    let cancel = CancellationToken::new();
    let task = tokio::spawn(store.run_cleanup(cancel.clone()));

    // The first run starts at once and drains the backlog.
    bounded("the first cleanup run", async {
        while count(&pool, EXPIRED).await > 0 {
            tokio::time::sleep(RETRY_PAUSE).await;
        }
    })
    .await;
    assert!(!task.is_finished(), "the task waits for its next tick");
    cancel.cancel();
    tokio::time::timeout(CANCEL_BUDGET, task)
        .await
        .expect("the cleanup task returns promptly on cancel")
        .expect("the cleanup task completes");
    close(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p5_a_read_only_session_is_unavailable_even_with_a_live_record(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (writer_pool, writer) = replica(&dsn).await;
    let record = success(INPUT, r#"{"id":1}"#);
    let work = Work::default();
    let first = execute(&writer, SCOPE, INPUT, &record, &work).await;
    assert_eq!(committed(first), record);

    // Sessions opened from now on are read-only.
    make_read_only(&pool).await;
    let (reader_pool, reader) = replica(&dsn).await;
    for scope in [SCOPE, OTHER_CALLER] {
        let refused = execute(&reader, scope, INPUT, &record, &work).await;
        assert!(matches!(refused, Attempted::Unavailable), "{refused:?}");
    }
    assert_eq!(work.runs(), 1);
    assert_eq!(count(&pool, EFFECTS).await, 1);
    close(&[&writer_pool, &reader_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p6_an_aborted_statement_is_internal_and_the_same_key_can_retry(pool: PgPool) {
    create_effects(&pool).await;
    let (store_pool, store) = replica(&dsn_for(&pool).await).await;
    let record = success(INPUT, r#"{"id":1}"#);
    let caller = caller("fixture-subject");

    // PostgreSQL marks the transaction failed after division by zero. The
    // store's normal record write then observes SQLSTATE 25P02 and classifies
    // it as an internal fault, not transient unavailability.
    let aborted: Attempted<()> = store
        .attempt(
            &ScopeKey::from_digest(SCOPE),
            &caller,
            &INPUT,
            async |tx: &mut Tx<'_>| {
                sqlx::query("INSERT INTO effects DEFAULT VALUES")
                    .execute(connection(tx))
                    .await
                    .expect("the effect starts inside the transaction");
                assert!(
                    sqlx::query("SELECT 1 / 0")
                        .execute(connection(tx))
                        .await
                        .is_err(),
                    "the first statement aborts the transaction"
                );
                WorkOutput::Commit(record.clone())
            },
        )
        .await;
    assert!(matches!(aborted, Attempted::Internal), "{aborted:?}");
    assert_eq!(count(&pool, EFFECTS).await, 0);
    assert_eq!(records(&pool, SCOPE).await, 0);

    let retry = execute(&store, SCOPE, INPUT, &record, &Work::default()).await;
    assert_eq!(committed(retry), record);
    assert_eq!(count(&pool, EFFECTS).await, 1);
    close(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p7_a_dropped_attempt_leaves_no_effect_and_frees_its_key(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (pool_1, replica_1) = replica(&dsn).await;
    let (pool_2, replica_2) = replica(&dsn).await;
    let record = success(INPUT, r#"{"id":1}"#);
    let work = Work::default();

    // Drive one attempt until its work has written its effect and waits, then
    // drop it, as an expired request budget or a disconnect does.
    work.hold.arm();
    let mut attempt = Box::pin(execute(&replica_1, SCOPE, INPUT, &record, &work));
    tokio::select! {
        outcome = &mut attempt => panic!("the held attempt finished: {outcome:?}"),
        () = work.hold.entered() => {}
    }
    drop(attempt);
    assert_eq!(count(&pool, EFFECTS).await, 0);

    // Once `PostgreSQL` ends the dropped transaction, the key is free on
    // every replica.
    let retry = once_free(&replica_2, SCOPE, &record, &work).await;
    assert_eq!(committed(retry), record);
    assert_eq!(work.runs(), 2);
    assert_eq!(count(&pool, EFFECTS).await, 1);
    assert_eq!(records(&pool, SCOPE).await, 1);
    close(&[&pool_1, &pool_2]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p8_startup_admits_a_migrated_writer_and_refuses_a_read_only_session(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let (writer_pool, writer) = replica(&dsn).await;
    assert_eq!(writer.check_startup().await, Ok(()));

    // Set before the pool opens, so every session of the pool is read-only.
    make_read_only(&pool).await;
    let (reader_pool, reader) = replica(&dsn).await;
    assert_eq!(reader.check_startup().await, Err(StartupError::NotWritable));
    close(&[&writer_pool, &reader_pool]).await;
}
