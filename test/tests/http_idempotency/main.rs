#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a broken fixture or setup fails the test"
)]

#[allow(
    dead_code,
    reason = "the shared transport also supplies faults to other integration targets"
)]
#[path = "../support/commit_proxy.rs"]
mod commit_proxy;
// template:begin http-idempotency-mounted:http-idempotency-mounted-module
mod mounted;
// template:end http-idempotency-mounted:http-idempotency-mounted-module

use std::fmt::Write as _;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::header::{
    CONTENT_DISPOSITION, CONTENT_ENCODING, CONTENT_LANGUAGE, CONTENT_TYPE, ETAG, LAST_MODIFIED,
    LOCATION,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use backon::{ConstantBuilder, Retryable};
use bytes::Bytes;
use futures_util::FutureExt as _;
use futures_util::future::join_all;
use infra_idempotency_store::{
    AttemptError, Attempted, CLEANUP_REMOVED_METRIC, CLEANUP_RUNS_METRIC, CallerIdentity,
    CallerKind, CleanupError, Record, RequestFingerprint, ScopeKey, StartupError, Store,
    SuccessRecord, TerminalRefusal, WriteOutcome,
};
use infra_postgres::{Dsn, Isolation, Pool, SavepointEnd, Tx};
use integration_tests::{close_pools, dsn_for, poll_until};
use sqlx::{Executor as _, PgPool};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use commit_proxy::{CommitProxy, ReadyBoundary};

type Outcome<R> = Result<Attempted<R>, AttemptError>;

const APP: &str = "integration-tests-idempotency";
const RETENTION: Duration = Duration::from_hours(1);
const WAIT: Duration = Duration::from_secs(10);
const RETRY_PAUSE: Duration = Duration::from_millis(20);
const CANCEL_BUDGET: Duration = Duration::from_secs(1);

const KEY: &[u8] = b"k-123";
const OTHER_KEY: &[u8] = b"other-key";
const SUBJECT: &str = "fixture-subject";
const OTHER_SUBJECT: &str = "other-subject";
const INPUT: RequestFingerprint = RequestFingerprint::from_bytes([0xa1; 32]);
const OTHER_INPUT: RequestFingerprint = RequestFingerprint::from_bytes([0xa2; 32]);

const BACKLOG: i64 = 1_201;
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
const LOCK_SEEDED_ROW: &str = "SELECT 1 FROM http_idempotency_records \
    WHERE scope_key = sha256(int8send(1::bigint)) FOR UPDATE";

const INSERT_LIVE: &str = "INSERT INTO http_idempotency_records \
    (scope_key, fingerprint, status, headers, body, issuer, caller_kind, caller_value, expires_at) \
    VALUES ($1, $2, 201, ARRAY[]::http_idempotency_header_pair[], '{}', \
    'https://issuer.example', 'subject', 'fixture-subject', now() + interval '1 hour')";

async fn template_pool(dsn: &Dsn, max_connections: u32) -> Pool {
    integration_tests::template_pool(dsn, APP, max_connections, Isolation::ServerDefault).await
}

async fn replica(dsn: &Dsn) -> (Pool, Store) {
    let pool = template_pool(dsn, 3).await;
    let store = Store::new(pool.clone(), RETENTION).expect("retention fits PostgreSQL");
    (pool, store)
}

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

async fn records(pool: &PgPool, scope: &ScopeKey) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM http_idempotency_records WHERE scope_key = $1")
        .bind(scope.digest())
        .fetch_one(pool)
        .await
        .expect("a record count")
}

async fn expire(pool: &PgPool, scope: &ScopeKey) {
    let moved = sqlx::query(
        "UPDATE http_idempotency_records SET expires_at = now() - interval '1 second' \
         WHERE scope_key = $1",
    )
    .bind(scope.digest())
    .execute(pool)
    .await
    .expect("the expiry moves");
    assert_eq!(moved.rows_affected(), 1);
}

async fn seed(pool: &PgPool, sql: &'static str, records: i64) {
    let seeded = sqlx::query(sql)
        .bind(records)
        .execute(pool)
        .await
        .expect("the records are seeded");
    assert_eq!(i64::try_from(seeded.rows_affected()), Ok(records));
}

fn success(body: &str) -> SuccessRecord {
    let mut headers = HeaderMap::new();
    for (name, value) in [
        (CONTENT_TYPE, "application/json"),
        (CONTENT_LANGUAGE, "en"),
        (CONTENT_LANGUAGE, "fr"),
        (CONTENT_DISPOSITION, "attachment; filename=widget.json"),
        (LOCATION, "/widgets/1"),
        (LAST_MODIFIED, "Sun, 06 Nov 1994 08:49:37 GMT"),
    ] {
        headers.append(name, HeaderValue::from_static(value));
    }
    headers.append(
        ETAG,
        HeaderValue::from_bytes(b"\"\x80\"").expect("valid opaque bytes"),
    );
    SuccessRecord::capture(
        StatusCode::CREATED,
        &headers,
        Bytes::copy_from_slice(body.as_bytes()),
    )
    .expect("a storable response")
}

fn caller(value: &str) -> CallerIdentity {
    CallerIdentity {
        issuer: "https://issuer.example".to_owned(),
        kind: CallerKind::Subject,
        value: value.to_owned(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldPhase {
    Idle,
    Armed,
    Entered,
    Released,
}

#[derive(Debug)]
struct Hold(watch::Sender<HoldPhase>);

impl Default for Hold {
    fn default() -> Self {
        let (tx, _) = watch::channel(HoldPhase::Idle);
        Self(tx)
    }
}

impl Hold {
    fn arm(&self) {
        self.0.send_replace(HoldPhase::Armed);
    }

    async fn pass(&self) {
        if self.0.send_if_modified(|phase| {
            if *phase == HoldPhase::Armed {
                *phase = HoldPhase::Entered;
                true
            } else {
                false
            }
        }) {
            let mut rx = self.0.subscribe();
            let _ = rx.wait_for(|phase| *phase == HoldPhase::Released).await;
        }
    }

    async fn entered(&self) {
        let mut rx = self.0.subscribe();
        bounded(
            "the held work to enter",
            rx.wait_for(|phase| matches!(phase, HoldPhase::Entered | HoldPhase::Released)),
        )
        .await
        .expect("hold sender stays open");
    }

    fn release(&self) {
        self.0.send_replace(HoldPhase::Released);
    }
}

#[derive(Debug, Default)]
struct Work {
    runs: AtomicUsize,
    hold: Hold,
}

impl Work {
    async fn run(&self, tx: &mut Tx<'_>) {
        self.runs.fetch_add(1, Ordering::SeqCst);
        sqlx::query("INSERT INTO effects DEFAULT VALUES")
            .execute(&mut *tx)
            .await
            .expect("the effect is written");
        self.hold.pass().await;
    }

    fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    NotSuccess,
}

async fn execute(
    store: &Store,
    scope: &ScopeKey,
    fingerprint: RequestFingerprint,
    record: &SuccessRecord,
    work: &Work,
) -> Outcome<Refusal> {
    store
        .attempt(scope, &fingerprint, async |tx: &mut Tx<'_>| {
            work.run(tx).await;
            Ok(WriteOutcome::success(record.clone()))
        })
        .await
}

async fn once_free(
    store: &Store,
    scope: &ScopeKey,
    record: &SuccessRecord,
    work: &Work,
) -> Outcome<Refusal> {
    bounded(
        "the key is released",
        (|| async {
            match execute(store, scope, INPUT, record, work).await {
                Ok(Attempted::InProgress) => Err(()),
                outcome => Ok(outcome),
            }
        })
        .retry(
            ConstantBuilder::default()
                .with_delay(RETRY_PAUSE)
                .without_max_times(),
        ),
    )
    .await
    .expect("retry stays open until timeout")
}

async fn bounded<F: Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(WAIT, future).await.expect(what)
}

fn committed(outcome: Outcome<Refusal>) -> Record {
    match outcome {
        Ok(Attempted::Committed(record)) => record,
        unexpected => panic!("expected a commit, got {unexpected:?}"),
    }
}

fn live(outcome: Outcome<Refusal>) -> Option<Record> {
    match outcome {
        Ok(Attempted::Replay(record)) => Some(record),
        Ok(Attempted::Mismatch) => None,
        unexpected => panic!("expected a live record, got {unexpected:?}"),
    }
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p1_a_held_key_refuses_duplicates_and_commits_one_effect(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (pool_1, replica_1) = replica(&dsn).await;
    let (pool_2, replica_2) = replica(&dsn).await;
    let record = success(r#"{"id":1}"#);
    let work = Work::default();
    let scope = ScopeKey::new(caller(SUBJECT), KEY);

    work.hold.arm();
    let (held, ()) = tokio::join!(execute(&replica_1, &scope, INPUT, &record, &work), async {
        work.hold.entered().await;
        for replica in [&replica_1, &replica_2] {
            for fingerprint in [INPUT, OTHER_INPUT] {
                let duplicate = execute(replica, &scope, fingerprint, &record, &work).await;
                std::assert_matches!(duplicate, Ok(Attempted::InProgress), "{duplicate:?}");
            }
        }
        assert_eq!(work.runs(), 1, "no duplicate ran its work");
        work.hold.release();
    },);
    assert_eq!(committed(held), Record::from(record.clone()));

    let concurrent = [&replica_1, &replica_2, &replica_1, &replica_2]
        .map(|replica| execute(replica, &scope, INPUT, &record, &work));
    for retry in join_all(concurrent).await {
        assert_eq!(live(retry), Some(Record::from(record.clone())));
    }
    assert_eq!(work.runs(), 1);
    assert_eq!(count(&pool, EFFECTS).await, 1);
    close_pools(&[&pool_1, &pool_2]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p1_a_record_committed_under_the_work_is_kept_and_the_work_rolls_back(pool: PgPool) {
    create_effects(&pool).await;
    let (store_pool, store) = replica(&dsn_for(&pool).await).await;
    let record = success(r#"{"id":1}"#);
    let work = Work::default();

    let scope = ScopeKey::new(caller(SUBJECT), KEY);
    let refused: Outcome<Refusal> = store
        .attempt(&scope, &INPUT, async |tx: &mut Tx<'_>| {
            work.run(tx).await;
            sqlx::query(INSERT_LIVE)
                .bind(scope.digest())
                .bind(OTHER_INPUT.as_bytes())
                .execute(&pool)
                .await
                .expect("another writer commits a live record");
            Ok(WriteOutcome::success(record.clone()))
        })
        .await;
    std::assert_matches!(refused, Err(AttemptError::Internal), "{refused:?}");
    assert_eq!(count(&pool, EFFECTS).await, 0);

    let decided = execute(&store, &scope, INPUT, &record, &work).await;
    assert_eq!(live(decided), None);
    assert_eq!(records(&pool, &scope).await, 1);
    assert_eq!(work.runs(), 1);
    close_pools(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p2_rolled_back_work_leaves_no_effect_or_record_and_the_retry_executes(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (pool_1, replica_1) = replica(&dsn).await;
    let (pool_2, replica_2) = replica(&dsn).await;
    let record = success(r#"{"id":1}"#);
    let work = Work::default();

    let scope = ScopeKey::new(caller(SUBJECT), KEY);
    let other_key = ScopeKey::new(caller(SUBJECT), OTHER_KEY);
    let outcome: Outcome<Refusal> = replica_1
        .attempt(
            &scope,
            &INPUT,
            async |tx: &mut Tx<'_>| -> Result<WriteOutcome, Refusal> {
                work.run(tx).await;
                Err(Refusal::NotSuccess)
            },
        )
        .await;
    std::assert_matches!(outcome, Ok(Attempted::RolledBack(Refusal::NotSuccess)));
    let panicked = AssertUnwindSafe(replica_1.attempt(
        &other_key,
        &INPUT,
        async |tx: &mut Tx<'_>| -> Result<WriteOutcome, Refusal> {
            work.run(tx).await;
            panic!("the work panics after writing its effect");
        },
    ))
    .catch_unwind()
    .await;
    assert!(panicked.is_err());
    assert_eq!(work.runs(), 2);
    assert_eq!(count(&pool, EFFECTS).await, 0);

    for scope in [&scope, &other_key] {
        assert_eq!(records(&pool, scope).await, 0);
        let retry = once_free(&replica_2, scope, &record, &work).await;
        assert_eq!(committed(retry), Record::from(record.clone()));
    }
    assert_eq!(work.runs(), 4);
    assert_eq!(count(&pool, EFFECTS).await, 2);
    close_pools(&[&pool_1, &pool_2]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn a_command_cannot_shadow_the_terminal_refusal_savepoint(pool: PgPool) {
    create_effects(&pool).await;
    let store = Store::new(Pool::from(pool.clone()), RETENTION).unwrap();
    let scope = ScopeKey::new(caller(SUBJECT), KEY);
    let refusal = TerminalRefusal::capture(
        StatusCode::CONFLICT,
        &HeaderMap::new(),
        Bytes::from_static(b"refused"),
    )
    .unwrap();
    let result = store
        .attempt(&scope, &INPUT, async |tx: &mut Tx<'_>| {
            sqlx::query("INSERT INTO effects DEFAULT VALUES")
                .execute(&mut *tx)
                .await
                .unwrap();
            tx.with_savepoint(async |tx| -> Result<SavepointEnd<()>, sqlx::Error> {
                sqlx::query("INSERT INTO effects DEFAULT VALUES")
                    .execute(&mut *tx)
                    .await?;
                Ok(SavepointEnd::Rollback(()))
            })
            .await
            .unwrap();
            Ok::<_, Refusal>(WriteOutcome::TerminalRefusal(refusal.clone()))
        })
        .await;
    std::assert_matches!(
        result,
        Ok(Attempted::TerminalRefusal(actual)) if actual == Record::from(refusal)
    );
    assert_eq!(records(&pool, &scope).await, 1);
    assert_eq!(count(&pool, EFFECTS).await, 0);
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn only_committed_command_work_wakes_listeners(pool: PgPool) {
    const CHANNEL: &str = "idempotency_test_wakeup";
    let mut listener = sqlx::postgres::PgListener::connect_with(&pool)
        .await
        .unwrap();
    listener.listen(CHANNEL).await.unwrap();
    let producer = template_pool(&dsn_for(&pool).await, 1).await;
    let store = Store::new(producer.clone(), RETENTION).unwrap();
    let refusal = TerminalRefusal::capture(
        StatusCode::CONFLICT,
        &HeaderMap::new(),
        Bytes::from_static(b"declined"),
    )
    .unwrap();
    for (payload, outcome) in [
        ("rolled_back", WriteOutcome::TerminalRefusal(refusal)),
        ("committed", WriteOutcome::Success(success("created"))),
    ] {
        let scope = ScopeKey::new(caller(SUBJECT), payload.as_bytes());
        let result = store
            .attempt(&scope, &INPUT, async |tx: &mut Tx<'_>| {
                tx.notify_after_commit(CHANNEL, payload, Duration::ZERO);
                Ok::<_, Refusal>(outcome)
            })
            .await;
        std::assert_matches!(
            result,
            Ok(Attempted::Committed(_) | Attempted::TerminalRefusal(_))
        );
    }
    let notification = bounded("committed command wakes the listener", listener.recv())
        .await
        .unwrap();
    assert_eq!(notification.payload(), "committed");
    close_pools(&[&producer]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn disabling_terminal_writes_still_replays_live_refusals(pool: PgPool) {
    create_effects(&pool).await;
    let store = Store::new(Pool::from(pool.clone()), RETENTION).unwrap();
    let work = Work::default();
    let scope = ScopeKey::new(caller(SUBJECT), KEY);
    let refusal = TerminalRefusal::capture(
        StatusCode::UNPROCESSABLE_ENTITY,
        &HeaderMap::new(),
        Bytes::from_static(b"reserved"),
    )
    .unwrap();
    let written = store
        .attempt(&scope, &INPUT, async |tx: &mut Tx<'_>| {
            work.run(tx).await;
            Ok::<_, Refusal>(WriteOutcome::TerminalRefusal(refusal.clone()))
        })
        .await;
    std::assert_matches!(
        written,
        Ok(Attempted::TerminalRefusal(actual)) if actual == Record::from(refusal.clone())
    );
    let retry = execute(&store, &scope, INPUT, &success("would execute"), &work).await;
    assert_eq!(live(retry), Some(Record::from(refusal)));
    assert_eq!(work.runs(), 1);
    assert_eq!(count(&pool, EFFECTS).await, 0);
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p3_a_replay_returns_the_stored_bytes_and_other_scopes_are_independent(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (pool_1, replica_1) = replica(&dsn).await;
    let (pool_2, replica_2) = replica(&dsn).await;
    let record = success(r#"{"id":1,"name":"first"}"#);
    let other = success(r#"{"id":2,"name":"other"}"#);
    let work = Work::default();
    let scope = ScopeKey::new(caller(SUBJECT), KEY);
    let first = execute(&replica_1, &scope, INPUT, &record, &work).await;
    assert_eq!(committed(first), Record::from(record.clone()));

    let replayed = execute(&replica_2, &scope, INPUT, &other, &work).await;
    assert_eq!(live(replayed), Some(Record::from(record)));
    let mismatched = execute(&replica_2, &scope, OTHER_INPUT, &other, &work).await;
    assert_eq!(live(mismatched), None);
    assert_eq!(work.runs(), 1);

    for independent_scope in [
        ScopeKey::new(caller(OTHER_SUBJECT), KEY),
        ScopeKey::new(caller(SUBJECT), OTHER_KEY),
        ScopeKey::new(
            CallerIdentity {
                issuer: "https://other-issuer.example".into(),
                ..caller(SUBJECT)
            },
            KEY,
        ),
        ScopeKey::new(
            CallerIdentity {
                kind: CallerKind::Client,
                ..caller(SUBJECT)
            },
            KEY,
        ),
    ] {
        let independent = execute(&replica_2, &independent_scope, INPUT, &other, &work).await;
        assert_eq!(committed(independent), Record::from(other.clone()));
    }
    assert_eq!(work.runs(), 5);
    assert_eq!(count(&pool, EFFECTS).await, 5);
    let metadata: (String, String, String) = sqlx::query_as(
        "SELECT issuer, caller_kind, caller_value FROM http_idempotency_records WHERE scope_key = $1",
    )
    .bind(scope.digest())
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
    close_pools(&[&pool_1, &pool_2]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p3_an_existing_format_row_replays_without_running_work(pool: PgPool) {
    const GZIP_BODY: &[u8] = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\xff\xabV\xcaLQ\xb22\xac\x05\x00\xc5\xf8]D\x08\x00\x00\x00";
    let fingerprint = RequestFingerprint::from_bytes([
        0x2e, 0x4f, 0xb5, 0x7a, 0x46, 0x09, 0x5c, 0x45, 0xe8, 0x86, 0xd2, 0x90, 0x70, 0x7b, 0xf6,
        0x45, 0x41, 0x23, 0x66, 0x96, 0x6b, 0xf1, 0x00, 0x09, 0x31, 0x40, 0x01, 0xfd, 0x08, 0x7e,
        0xb3, 0x0b,
    ]);
    sqlx::query(
        "INSERT INTO http_idempotency_records \
         (scope_key, fingerprint, status, headers, body, issuer, caller_kind, caller_value, expires_at) \
         VALUES (decode('98df4043b9e795011fe4f84c3844bcfe9ade67032cc0044471d5097f7a30ad94', 'hex'), \
         $1, 201, ARRAY[ \
           ROW('content-type', convert_to('application/json', 'UTF8'))::http_idempotency_header_pair, \
           ROW('content-encoding', convert_to('gzip', 'UTF8'))::http_idempotency_header_pair, \
           ROW('content-language', convert_to('en', 'UTF8'))::http_idempotency_header_pair, \
           ROW('content-language', convert_to('fr', 'UTF8'))::http_idempotency_header_pair, \
           ROW('etag', decode('228022', 'hex'))::http_idempotency_header_pair \
         ], $2, 'https://issuer.example', 'subject', 'fixture-subject', now() + interval '1 hour')",
    )
    .bind(fingerprint.as_bytes())
    .bind(GZIP_BODY)
    .execute(&pool)
    .await
    .expect("seed a record in the existing format");

    let (store_pool, store) = replica(&dsn_for(&pool).await).await;
    let scope = ScopeKey::new(caller(SUBJECT), KEY);
    for (status, expected_body) in [
        (StatusCode::CREATED, GZIP_BODY),
        (StatusCode::NO_CONTENT, b"".as_slice()),
        (StatusCode::RESET_CONTENT, b"".as_slice()),
    ] {
        if status != StatusCode::CREATED {
            let changed =
                sqlx::query("UPDATE http_idempotency_records SET status = $2 WHERE scope_key = $1")
                    .bind(scope.digest())
                    .bind(status.as_u16().cast_signed())
                    .execute(&pool)
                    .await
                    .expect("seed legacy content under a status that forbids it");
            assert_eq!(changed.rows_affected(), 1);
        }
        let replay: Outcome<()> = store
            .attempt(
                &scope,
                &fingerprint,
                async |_: &mut Tx<'_>| -> Result<WriteOutcome, ()> {
                    panic!("an existing row must replay without running work")
                },
            )
            .await;
        let Ok(Attempted::Replay(record)) = replay else {
            panic!("expected a replay of the existing record, got {replay:?}");
        };
        let (replayed_status, headers, body) = record.into_parts();
        assert_eq!(replayed_status, status);
        assert_eq!(body.as_ref(), expected_body);
        assert_eq!(headers.len(), 5);
        assert_eq!(headers[CONTENT_TYPE], "application/json");
        assert_eq!(headers[CONTENT_ENCODING], "gzip");
        assert_eq!(
            headers
                .get_all(CONTENT_LANGUAGE)
                .iter()
                .map(HeaderValue::as_bytes)
                .collect::<Vec<_>>(),
            [b"en".as_slice(), b"fr".as_slice()]
        );
        assert_eq!(headers[ETAG].as_bytes(), b"\"\x80\"");
    }
    close_pools(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p3_corrupt_columns_are_integrity_failures_even_for_a_mismatch(pool: PgPool) {
    create_effects(&pool).await;
    let (store_pool, store) = replica(&dsn_for(&pool).await).await;
    let record = success("stored body");
    let work = Work::default();
    let scope = ScopeKey::new(caller(SUBJECT), KEY);
    assert_eq!(
        committed(execute(&store, &scope, INPUT, &record, &work).await),
        Record::from(record.clone())
    );

    sqlx::query("ALTER TABLE http_idempotency_records ALTER COLUMN body DROP NOT NULL")
        .execute(&pool)
        .await
        .expect("the fixture permits a corrupt body");
    for (corruption, mismatch_sees_it) in [
        (
            "UPDATE http_idempotency_records SET headers = \
             ARRAY[NULL]::http_idempotency_header_pair[] WHERE scope_key = $1",
            true,
        ),
        (
            "UPDATE http_idempotency_records SET headers = \
             ARRAY[ROW(NULL, ''::bytea)::http_idempotency_header_pair] WHERE scope_key = $1",
            true,
        ),
        (
            "UPDATE http_idempotency_records SET headers = \
             ARRAY[ROW('content-type', NULL)::http_idempotency_header_pair] WHERE scope_key = $1",
            true,
        ),
        (
            "UPDATE http_idempotency_records SET headers = \
             ARRAY[]::http_idempotency_header_pair[], body = NULL WHERE scope_key = $1",
            false,
        ),
    ] {
        let changed = sqlx::query(corruption)
            .bind(scope.digest())
            .execute(&pool)
            .await
            .expect("one stored column is corrupted");
        assert_eq!(changed.rows_affected(), 1);
        for fingerprint in [INPUT, OTHER_INPUT] {
            let result = bounded(
                "the corrupt record is refused",
                execute(&store, &scope, fingerprint, &record, &work),
            )
            .await;
            if fingerprint == INPUT || mismatch_sees_it {
                std::assert_matches!(result, Err(AttemptError::Integrity), "{result:?}");
            } else {
                std::assert_matches!(result, Ok(Attempted::Mismatch), "{result:?}");
            }
        }
    }
    assert_eq!(work.runs(), 1);
    assert_eq!(count(&pool, EFFECTS).await, 1);
    close_pools(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p3_a_long_verified_caller_identity_commits_and_replays(pool: PgPool) {
    create_effects(&pool).await;
    let (store_pool, store) = replica(&dsn_for(&pool).await).await;
    let mut caller_value = String::new();
    for part in 0..2_048 {
        write!(&mut caller_value, "caller-{part:08x};").expect("String writes do not fail");
    }
    let scope = ScopeKey::new(caller(&caller_value), KEY);
    let record = success(r#"{"id":1,"name":"long-caller"}"#);

    let first: Outcome<()> = store
        .attempt(&scope, &INPUT, async |tx: &mut Tx<'_>| {
            sqlx::query("INSERT INTO effects DEFAULT VALUES")
                .execute(&mut *tx)
                .await
                .expect("the committed effect");
            Ok(WriteOutcome::success(record.clone()))
        })
        .await;
    match first {
        Ok(Attempted::Committed(actual)) => assert_eq!(actual, Record::from(record.clone())),
        unexpected => panic!("expected a committed long-caller record, got {unexpected:?}"),
    }

    let replayed: Outcome<()> = store
        .attempt(&scope, &INPUT, async |tx: &mut Tx<'_>| {
            sqlx::query("INSERT INTO effects DEFAULT VALUES")
                .execute(&mut *tx)
                .await
                .expect("a replay must not run this effect");
            Ok(WriteOutcome::success(record.clone()))
        })
        .await;
    match replayed {
        Ok(Attempted::Replay(actual)) => assert_eq!(actual, Record::from(record)),
        unexpected => panic!("expected a matching long-caller replay, got {unexpected:?}"),
    }
    assert_eq!(count(&pool, EFFECTS).await, 1);
    let stored: String = sqlx::query_scalar(
        "SELECT caller_value FROM http_idempotency_records WHERE scope_key = $1",
    )
    .bind(scope.digest())
    .fetch_one(&pool)
    .await
    .expect("the stored caller identity");
    assert_eq!(stored, caller_value);
    close_pools(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p4_an_expired_record_is_not_replayed_and_is_replaced(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (pool_1, replica_1) = replica(&dsn).await;
    let (pool_2, replica_2) = replica(&dsn).await;
    let record = success(r#"{"id":1}"#);
    let replacement = success(r#"{"id":2}"#);
    let work = Work::default();
    let scope = ScopeKey::new(caller(SUBJECT), KEY);
    let first = execute(&replica_1, &scope, INPUT, &record, &work).await;
    assert_eq!(committed(first), Record::from(record.clone()));
    expire(&pool, &scope).await;

    let afresh = execute(&replica_2, &scope, OTHER_INPUT, &replacement, &work).await;
    assert_eq!(committed(afresh), Record::from(replacement));
    let decided = execute(&replica_1, &scope, INPUT, &record, &work).await;
    assert_eq!(live(decided), None);
    assert_eq!(records(&pool, &scope).await, 1);
    assert_eq!(work.runs(), 2);
    assert_eq!(count(&pool, EFFECTS).await, 2);
    close_pools(&[&pool_1, &pool_2]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p4_waiting_for_an_expired_row_preserves_retention_and_replay(pool: PgPool) {
    create_effects(&pool).await;
    let store_pool = template_pool(&dsn_for(&pool).await, 1).await;
    let retention = Duration::from_secs(2);
    let store = Store::new(store_pool.clone(), retention).expect("retention fits PostgreSQL");

    for (key, delete_while_locked) in [(KEY, false), (OTHER_KEY, true)] {
        let scope = ScopeKey::new(caller(SUBJECT), key);
        sqlx::query(INSERT_LIVE)
            .bind(scope.digest())
            .bind(INPUT.as_bytes())
            .execute(&pool)
            .await
            .expect("seed the old record");
        expire(&pool, &scope).await;

        let mut holder = pool.begin().await.expect("a row-lock holder");
        let holder_pid: i32 = sqlx::query_scalar(
            "SELECT pg_backend_pid() FROM http_idempotency_records WHERE scope_key = $1 FOR UPDATE",
        )
        .bind(scope.digest())
        .fetch_one(&mut *holder)
        .await
        .expect("lock the expired record");

        let work = Work::default();
        let response = success(r#"{"id":2}"#);
        let (written, before_release) = bounded("the blocked record write to commit", async {
            tokio::join!(execute(&store, &scope, INPUT, &response, &work), async {
                poll_until(
                    "the writer waits on the row lock",
                    WAIT,
                    RETRY_PAUSE,
                    async || {
                        let blocked: bool = sqlx::query_scalar(
                            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
                             WHERE datname = current_database() AND application_name = $1 \
                             AND wait_event_type = 'Lock' AND $2 = ANY(pg_blocking_pids(pid)))",
                        )
                        .bind(APP)
                        .bind(holder_pid)
                        .fetch_one(&pool)
                        .await
                        .expect("observe the writer waiting on the row lock");
                        blocked.then_some(())
                    },
                )
                .await;
                tokio::time::sleep(retention + RETRY_PAUSE).await;
                if delete_while_locked {
                    sqlx::query("DELETE FROM http_idempotency_records WHERE scope_key = $1")
                        .bind(scope.digest())
                        .execute(&mut *holder)
                        .await
                        .expect("delete the expired record as cleanup would");
                }
                let before_release: String = sqlx::query_scalar("SELECT clock_timestamp()::text")
                    .fetch_one(&mut *holder)
                    .await
                    .expect("read the database clock while the row remains locked");
                holder.commit().await.expect("release the row lock");
                before_release
            })
        })
        .await;
        assert_eq!(committed(written), Record::from(response.clone()));

        let retry_response = success(r#"{"id":3}"#);
        let retry = bounded(
            "same-key replay after the write",
            execute(&store, &scope, INPUT, &retry_response, &work),
        )
        .await;
        assert_eq!(live(retry), Some(Record::from(response)));
        assert_eq!(work.runs(), 1);
        let full_retention: bool = sqlx::query_scalar(
            "SELECT expires_at >= $2::text::timestamptz + $3::interval \
             FROM http_idempotency_records WHERE scope_key = $1",
        )
        .bind(scope.digest())
        .bind(before_release)
        .bind(retention)
        .fetch_one(&pool)
        .await
        .expect("read the stored expiry");
        assert!(full_retention, "retention starts after the row lock wait");
    }
    assert_eq!(count(&pool, EFFECTS).await, 2);
    close_pools(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p4_an_expired_key_can_execute_while_a_replay_is_in_flight(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (proxy, proxied) = CommitProxy::start_for(&pool).await;
    let replay_pool = template_pool(&proxied, 1).await;
    let replaying = Store::new(replay_pool.clone(), RETENTION).expect("retention fits PostgreSQL");
    let (writer_pool, writer) = replica(&dsn).await;
    let original = success(r#"{"id":1}"#);
    let replacement = success(r#"{"id":2}"#);
    let work = Work::default();
    let scope = ScopeKey::new(caller(SUBJECT), KEY);

    let seeded = execute(&writer, &scope, INPUT, &original, &work).await;
    assert_eq!(committed(seeded), Record::from(original.clone()));
    proxy.arm_ready_hold(ReadyBoundary::ExecuteSync);
    let (replay, afresh) = bounded("replay across expiry", async {
        tokio::join!(
            execute(&replaying, &scope, INPUT, &original, &work),
            async {
                proxy.ready_held().await;
                expire(&pool, &scope).await;
                let outcome = execute(&writer, &scope, OTHER_INPUT, &replacement, &work).await;
                proxy.release_ready();
                outcome
            }
        )
    })
    .await;

    assert_eq!(live(replay), Some(Record::from(original)));
    assert_eq!(committed(afresh), Record::from(replacement.clone()));
    let retry = execute(&writer, &scope, OTHER_INPUT, &replacement, &work).await;
    assert_eq!(live(retry), Some(Record::from(replacement)));
    assert_eq!(count(&pool, EFFECTS).await, 2);
    close_pools(&[&replay_pool, &writer_pool]).await;
    proxy.shutdown().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p4_a_retention_with_a_sub_microsecond_part_writes_its_record(pool: PgPool) {
    create_effects(&pool).await;
    let store_pool = template_pool(&dsn_for(&pool).await, 1).await;
    let store = Store::new(store_pool.clone(), Duration::new(3_600, 123_456_789))
        .expect("retention fits PostgreSQL");
    let record = success(r#"{"id":1}"#);
    let scope = ScopeKey::new(caller(SUBJECT), KEY);
    let written = execute(&store, &scope, INPUT, &record, &Work::default()).await;
    assert_eq!(committed(written), Record::from(record));
    let on_time: bool = sqlx::query_scalar(
        "SELECT expires_at BETWEEN now() + interval '3599 seconds' \
         AND now() + interval '3601 seconds' \
         FROM http_idempotency_records WHERE scope_key = $1",
    )
    .bind(scope.digest())
    .fetch_one(&pool)
    .await
    .expect("the record's expiry");
    assert!(on_time, "the record lives for the retention");
    close_pools(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p4_cleanup_drains_the_backlog_and_keeps_live_and_held_records(pool: PgPool) {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let _local = metrics::set_default_local_recorder(&recorder);
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (pool_1, replica_1) = replica(&dsn).await;
    let (pool_2, replica_2) = replica(&dsn).await;
    let record = success(r#"{"id":1}"#);
    let work = Work::default();
    let scope = ScopeKey::new(caller(SUBJECT), KEY);
    let first = execute(&replica_1, &scope, INPUT, &record, &work).await;
    assert_eq!(committed(first), Record::from(record.clone()));
    expire(&pool, &scope).await;
    seed(&pool, SEED_EXPIRED, BACKLOG).await;
    seed(&pool, SEED_LIVE, LIVE).await;
    let mut holder = pool.begin().await.expect("a row-lock holder");
    sqlx::query(LOCK_SEEDED_ROW)
        .execute(&mut *holder)
        .await
        .expect("the row lock");

    work.hold.arm();
    let (executed, removed) =
        tokio::join!(execute(&replica_1, &scope, INPUT, &record, &work), async {
            work.hold.entered().await;
            let removed = bounded("a cleanup run", Box::pin(replica_2.remove_expired())).await;
            let duplicate = execute(&replica_2, &scope, INPUT, &record, &work).await;
            std::assert_matches!(duplicate, Ok(Attempted::InProgress), "{duplicate:?}");
            work.hold.release();
            removed
        },);
    let drained = u64::try_from(BACKLOG).expect("a row count");
    assert_eq!(removed, Ok(drained));
    let scrape = recorder.handle().render();
    for line in [
        "postgres_cleanup_active_passes{cleanup=\"http_idempotency\"} 0",
        "postgres_cleanup_committed_batches_total{cleanup=\"http_idempotency\"} 3",
        "postgres_cleanup_removed_rows_total{cleanup=\"http_idempotency\"} 1201",
        "postgres_cleanup_passes_total{cleanup=\"http_idempotency\",outcome=\"completed\"} 1",
    ] {
        assert!(scrape.contains(line), "{line} missing from {scrape}");
    }
    assert!(
        !scrape.contains(CLEANUP_RUNS_METRIC),
        "direct calls are not scheduler runs"
    );
    assert_eq!(committed(executed), Record::from(record));
    assert_eq!(count(&pool, EXPIRED).await, 1);
    assert_eq!(count(&pool, LIVE_RECORDS).await, LIVE + 1);

    holder.rollback().await.expect("the row lock ends");
    assert_eq!(replica_2.remove_expired().await, Ok(1));
    assert_eq!(count(&pool, EXPIRED).await, 0);
    assert_eq!(count(&pool, LIVE_RECORDS).await, LIVE + 1);
    assert_eq!(work.runs(), 2);
    assert_eq!(count(&pool, EFFECTS).await, 2);
    close_pools(&[&pool_1, &pool_2]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p4_cleanup_keeps_confirmed_progress_after_failure_and_counts_waiting_callers(
    pool: PgPool,
) {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let _local = metrics::set_default_local_recorder(&recorder);
    let store_pool = template_pool(&dsn_for(&pool).await, 1).await;
    let store = Store::new(store_pool.clone(), RETENTION).expect("retention fits PostgreSQL");
    seed(&pool, SEED_EXPIRED, 501).await;
    sqlx::query(
        "UPDATE http_idempotency_records SET expires_at = now() - interval '2 hours' \
         WHERE scope_key <> sha256(int8send(501::bigint))",
    )
    .execute(&pool)
    .await
    .expect("order the two batches");
    sqlx::raw_sql(
        "CREATE FUNCTION reject_last_cleanup_row() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'test cleanup rejection'; END; $$; \
         CREATE TRIGGER reject_last_cleanup_row BEFORE DELETE ON http_idempotency_records \
         FOR EACH ROW WHEN (OLD.scope_key = sha256(int8send(501::bigint))) \
         EXECUTE FUNCTION reject_last_cleanup_row()",
    )
    .execute(&pool)
    .await
    .expect("a rejection in the later batch");
    assert_eq!(
        bounded("failed cleanup", Box::pin(store.remove_expired())).await,
        Err(CleanupError::Statement)
    );
    assert_eq!(count(&pool, EXPIRED).await, 1);
    let scrape = recorder.handle().render();
    for line in [
        "postgres_cleanup_committed_batches_total{cleanup=\"http_idempotency\"} 1",
        "postgres_cleanup_removed_rows_total{cleanup=\"http_idempotency\"} 500",
        "postgres_cleanup_passes_total{cleanup=\"http_idempotency\",outcome=\"failed\"} 1",
        "http_idempotency_cleanup_removed_records_total 500",
    ] {
        assert!(scrape.contains(line), "{line} missing from {scrape}");
    }
    let held = infra_postgres::acquire(&store_pool, "hold cleanup admission")
        .await
        .unwrap();
    let never_polled = store.remove_expired();
    drop(never_polled);
    let mut first = Box::pin(store.remove_expired());
    let mut second = Box::pin(store.remove_expired());
    assert!(futures_util::poll!(&mut first).is_pending());
    assert!(futures_util::poll!(&mut second).is_pending());
    assert!(
        recorder
            .handle()
            .render()
            .contains("postgres_cleanup_active_passes{cleanup=\"http_idempotency\"} 2")
    );
    drop(first);
    assert!(
        recorder
            .handle()
            .render()
            .contains("postgres_cleanup_active_passes{cleanup=\"http_idempotency\"} 1")
    );
    drop(second);
    drop(held);
    let scrape = recorder.handle().render();
    for line in [
        "postgres_cleanup_active_passes{cleanup=\"http_idempotency\"} 0",
        "postgres_cleanup_committed_batches_total{cleanup=\"http_idempotency\"} 1",
        "postgres_cleanup_removed_rows_total{cleanup=\"http_idempotency\"} 500",
        "postgres_cleanup_passes_total{cleanup=\"http_idempotency\",outcome=\"cancelled\"} 2",
    ] {
        assert!(scrape.contains(line), "{line} missing from {scrape}");
    }
    assert!(!scrape.contains(CLEANUP_RUNS_METRIC), "no scheduler calls");
    close_pools(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p4_the_cleanup_task_runs_at_once_and_returns_promptly_on_cancel(pool: PgPool) {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let _local = metrics::set_default_local_recorder(&recorder);
    let (store_pool, store) = replica(&dsn_for(&pool).await).await;
    seed(&pool, SEED_EXPIRED, BACKLOG).await;
    let cancel = CancellationToken::new();
    let task = tokio::spawn(store.run_cleanup(cancel.clone()));

    let completed = format!("{CLEANUP_RUNS_METRIC}{{outcome=\"completed\"}} 1");
    poll_until("the first cleanup run", WAIT, RETRY_PAUSE, async || {
        recorder
            .handle()
            .render()
            .contains(&completed)
            .then_some(())
    })
    .await;
    assert_eq!(count(&pool, EXPIRED).await, 0);
    let rendered = recorder.handle().render();
    assert!(
        rendered.contains(&format!("{CLEANUP_REMOVED_METRIC} {BACKLOG}\n")),
        "{rendered}"
    );
    assert!(!task.is_finished(), "the task waits for its next tick");
    cancel.cancel();
    tokio::time::timeout(CANCEL_BUDGET, task)
        .await
        .expect("the cleanup task returns promptly on cancel")
        .expect("the cleanup task completes");
    let scrape = recorder.handle().render();
    assert!(scrape.contains(
        "postgres_cleanup_passes_total{cleanup=\"http_idempotency\",outcome=\"completed\"} 1"
    ));
    assert!(
        !scrape.contains(
            "postgres_cleanup_passes_total{cleanup=\"http_idempotency\",outcome=\"cancelled\"}"
        ) || scrape.contains(
            "postgres_cleanup_passes_total{cleanup=\"http_idempotency\",outcome=\"cancelled\"} 0\n"
        ),
        "idle cancellation creates no cleanup pass: {scrape}"
    );
    close_pools(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p4_population_distinguishes_unknown_empty_and_locked_expired_records(pool: PgPool) {
    seed(&pool, SEED_LIVE, 1).await;
    let store_pool = template_pool(&dsn_for(&pool).await, 1).await;
    for expired in [true, false] {
        if expired {
            seed(&pool, SEED_EXPIRED, 1).await;
        }
        let expected_oldest: Option<f64> = sqlx::query_scalar(
            "SELECT extract(epoch FROM min(expires_at))::float8 \
             FROM http_idempotency_records WHERE expires_at <= statement_timestamp()",
        )
        .fetch_one(&pool)
        .await
        .expect("the independent population timestamp");
        let mut holder = pool.begin().await.expect("a row-lock holder");
        sqlx::query(LOCK_SEEDED_ROW)
            .execute(&mut *holder)
            .await
            .expect("the population's oldest row remains locked");

        let recorder = metrics_util::debugging::DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let _local = metrics::set_default_local_recorder(&recorder);
        let snapshot_gauges = || -> std::collections::BTreeMap<String, f64> {
            snapshotter
                .snapshot()
                .into_vec()
                .into_iter()
                .filter_map(|(key, _, _, value)| {
                    if !key
                        .key()
                        .labels()
                        .any(|l| l.key() == "population" && l.value() == "http_idempotency")
                    {
                        return None;
                    }
                    let metrics_util::debugging::DebugValue::Gauge(v) = value else {
                        return None;
                    };
                    Some((key.key().name().to_owned(), v.into_inner()))
                })
                .collect()
        };
        let admission = infra_postgres::acquire(&store_pool, "hold sampler admission")
            .await
            .expect("the sole slot");
        let cancel = CancellationToken::new();
        let store = Store::new(store_pool.clone(), RETENTION).expect("retention fits PostgreSQL");
        let mut task = Box::pin(store.run_cleanup(cancel.clone()));
        assert!(futures_util::poll!(&mut task).is_pending());
        let initial = snapshot_gauges();
        assert_eq!(initial["postgres_maintenance_observer_enabled"], 1.0);
        assert_eq!(
            initial["postgres_maintenance_last_success_timestamp_seconds"],
            0.0
        );
        assert!(initial["postgres_maintenance_present"].is_nan());
        assert!(initial["postgres_maintenance_oldest_timestamp_seconds"].is_nan());
        drop(admission);

        let sampled = tokio::select! {
            () = &mut task => panic!("maintenance ended before cancellation"),
            sampled = poll_until(
                "the independently polled population sample",
                WAIT,
                RETRY_PAUSE,
                async || {
                    let gauges = snapshot_gauges();
                    (gauges
                        .get("postgres_maintenance_last_attempt_success")
                        .copied()
                        == Some(1.0))
                    .then_some(gauges)
                },
            ) => sampled,
        };
        assert_eq!(sampled["postgres_maintenance_present"], f64::from(expired));
        assert_eq!(
            sampled["postgres_maintenance_oldest_timestamp_seconds"],
            expected_oldest.unwrap_or(0.0)
        );
        assert!(sampled["postgres_maintenance_database_observed_timestamp_seconds"] > 0.0);
        assert_eq!(sampled["postgres_maintenance_clock_valid"], 1.0);
        assert_eq!(count(&pool, LIVE_RECORDS).await, 1);
        assert_eq!(count(&pool, EXPIRED).await, i64::from(expired));
        metrics::gauge!(
            "postgres_maintenance_observer_enabled",
            "population" => "http_idempotency"
        )
        .set(1.0);
        cancel.cancel();
        tokio::time::timeout(CANCEL_BUDGET, &mut task)
            .await
            .expect("both maintenance loops finish on cancellation");
        let stopped = snapshot_gauges();
        assert_eq!(stopped["postgres_maintenance_observer_enabled"], 0.0);
        holder.rollback().await.expect("release the oldest row");
        sqlx::query(
            "DELETE FROM http_idempotency_records WHERE expires_at <= statement_timestamp()",
        )
        .execute(&pool)
        .await
        .expect("remove the fixture for the empty sample");
    }
    close_pools(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p5_a_read_only_session_is_unavailable_even_with_a_live_record(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (writer_pool, writer) = replica(&dsn).await;
    let record = success(r#"{"id":1}"#);
    let work = Work::default();
    let scope = ScopeKey::new(caller(SUBJECT), KEY);
    let other_caller = ScopeKey::new(caller(OTHER_SUBJECT), KEY);
    let first = execute(&writer, &scope, INPUT, &record, &work).await;
    assert_eq!(committed(first), Record::from(record.clone()));

    sqlx::query("UPDATE http_idempotency_records SET headers = ARRAY[ROW('set-cookie', ''::bytea)::http_idempotency_header_pair] WHERE scope_key = $1")
        .bind(scope.digest())
        .execute(&pool)
        .await
        .expect("seed a disallowed replay header");
    make_read_only(&pool).await;
    let (reader_pool, reader) = replica(&dsn).await;
    for scope in [&scope, &other_caller] {
        let refused = execute(&reader, scope, INPUT, &record, &work).await;
        std::assert_matches!(refused, Err(AttemptError::Unavailable), "{refused:?}");
    }
    assert_eq!(work.runs(), 1);
    assert_eq!(count(&pool, EFFECTS).await, 1);
    assert_eq!(reader.remove_expired().await, Err(CleanupError::Statement));
    close_pools(&[&writer_pool, &reader_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p6_an_aborted_statement_is_internal_and_the_same_key_can_retry(pool: PgPool) {
    create_effects(&pool).await;
    let (store_pool, store) = replica(&dsn_for(&pool).await).await;
    let record = success(r#"{"id":1}"#);
    let scope = ScopeKey::new(caller(SUBJECT), KEY);

    let aborted: Outcome<()> = store
        .attempt(&scope, &INPUT, async |tx: &mut Tx<'_>| {
            sqlx::query("INSERT INTO effects DEFAULT VALUES")
                .execute(&mut *tx)
                .await
                .expect("the effect starts inside the transaction");
            assert!(
                sqlx::query("SELECT 1 / 0").execute(&mut *tx).await.is_err(),
                "the first statement aborts the transaction"
            );
            Ok(WriteOutcome::success(record.clone()))
        })
        .await;
    std::assert_matches!(aborted, Err(AttemptError::Internal), "{aborted:?}");
    assert_eq!(count(&pool, EFFECTS).await, 0);
    assert_eq!(records(&pool, &scope).await, 0);

    let retry = execute(&store, &scope, INPUT, &record, &Work::default()).await;
    assert_eq!(committed(retry), Record::from(record));
    assert_eq!(count(&pool, EFFECTS).await, 1);
    close_pools(&[&store_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p7_a_dropped_attempt_leaves_no_effect_and_frees_its_key(pool: PgPool) {
    create_effects(&pool).await;
    let dsn = dsn_for(&pool).await;
    let (pool_1, replica_1) = replica(&dsn).await;
    let (pool_2, replica_2) = replica(&dsn).await;
    let record = success(r#"{"id":1}"#);
    let work = Work::default();
    let scope = ScopeKey::new(caller(SUBJECT), KEY);

    work.hold.arm();
    let mut attempt = Box::pin(execute(&replica_1, &scope, INPUT, &record, &work));
    tokio::select! {
        outcome = &mut attempt => panic!("the held attempt finished: {outcome:?}"),
        () = work.hold.entered() => {}
    }
    drop(attempt);
    assert_eq!(count(&pool, EFFECTS).await, 0);

    let retry = once_free(&replica_2, &scope, &record, &work).await;
    assert_eq!(committed(retry), Record::from(record));
    assert_eq!(work.runs(), 2);
    assert_eq!(count(&pool, EFFECTS).await, 1);
    assert_eq!(records(&pool, &scope).await, 1);
    close_pools(&[&pool_1, &pool_2]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p8_startup_admits_a_migrated_writer_and_refuses_a_read_only_session(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let (writer_pool, writer) = replica(&dsn).await;
    assert_eq!(writer.check_startup().await, Ok(()));

    make_read_only(&pool).await;
    let (reader_pool, reader) = replica(&dsn).await;
    assert_eq!(reader.check_startup().await, Err(StartupError::NotWritable));
    close_pools(&[&writer_pool, &reader_pool]).await;
}
