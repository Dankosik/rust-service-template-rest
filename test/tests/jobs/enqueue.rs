use std::sync::Arc;
use std::time::Duration;

use infra_jobs::{
    EnqueueError, EnqueueOptions, Enqueued, JobId, JobKind, MAX_DELAY, MAX_PAYLOAD_BYTES, enqueue,
};
use infra_postgres::{
    Isolation, PgPool, Tx, TxError, TxOptions, connection, in_tx, in_tx_with, retryable,
};
use integration_tests::dsn_for;
use serde::ser::Error as _;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::postgres::PgConnection;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

#[derive(Debug, Serialize, Deserialize)]
struct Note {
    text: String,
}

impl JobKind for Note {
    const NAME: &'static str = "test.note";
}

#[derive(Debug, Serialize, Deserialize)]
struct Other {
    n: u32,
}

impl JobKind for Other {
    const NAME: &'static str = "test.other";
}

#[derive(Debug, Serialize, Deserialize)]
struct BadKind;

impl JobKind for BadKind {
    const NAME: &'static str = "Bad";
}

#[derive(Debug, Serialize, Deserialize)]
struct Raw(String);

impl JobKind for Raw {
    const NAME: &'static str = "test.raw";
}

#[derive(Debug, Deserialize)]
struct Refuse;

impl Serialize for Refuse {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let _ = (self, serializer);
        Err(S::Error::custom("refused"))
    }
}

impl JobKind for Refuse {
    const NAME: &'static str = "test.refuse";
}

#[derive(Debug)]
enum AttemptError {
    Enqueue(EnqueueError),
    Query(sqlx::Error),
    Rejected,
    Tx(TxError),
}

impl From<TxError> for AttemptError {
    fn from(err: TxError) -> Self {
        Self::Tx(err)
    }
}

impl From<EnqueueError> for AttemptError {
    fn from(err: EnqueueError) -> Self {
        Self::Enqueue(err)
    }
}

impl From<sqlx::Error> for AttemptError {
    fn from(err: sqlx::Error) -> Self {
        Self::Query(err)
    }
}

fn explain(err: &AttemptError) -> String {
    match err {
        AttemptError::Enqueue(err) => format!("enqueue failed: {err}"),
        AttemptError::Query(err) => format!("query failed: {err}"),
        AttemptError::Rejected => "the caller rejected the transaction".to_owned(),
        AttemptError::Tx(err) => format!("transaction failed: {err}"),
    }
}

#[allow(clippy::struct_excessive_bools)] // One flag per nullable column of the row under test.
struct StoredJob {
    id_text: String,
    kind: String,
    payload: serde_json::Value,
    unique_key_is_null: bool,
    state: String,
    attempts: i16,
    claim_generation: i64,
    claim_expires_at_is_null: bool,
    finished_at_is_null: bool,
    failure_reason_is_null: bool,
    error_summary_is_null: bool,
    trace_context_is_null: bool,
}

const ANY: [Isolation; 3] = [
    Isolation::ReadCommitted,
    Isolation::RepeatableRead,
    Isolation::Serializable,
];

const SNAPSHOT: [Isolation; 2] = [Isolation::RepeatableRead, Isolation::Serializable];

const CLAIM_LIKE: &str = "UPDATE background_jobs \
     SET state = 'running', \
         attempts = attempts + 1, \
         claim_generation = nextval('background_jobs_claim_generation'), \
         claim_expires_at = statement_timestamp() + interval '30 seconds' \
     WHERE kind = $1 AND unique_key = $2 AND state = 'pending'";

struct Unblock(Arc<Notify>);

impl Drop for Unblock {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

fn created(enqueued: Enqueued) -> JobId {
    match enqueued {
        Enqueued::Created(id) => id,
        Enqueued::Duplicate => panic!("expected Created, got Duplicate"),
    }
}

fn keyed(key: &str) -> EnqueueOptions<'_> {
    EnqueueOptions {
        delay: Duration::ZERO,
        unique_key: Some(key),
    }
}

fn writable(isolation: Isolation) -> TxOptions {
    TxOptions {
        isolation,
        read_only: false,
    }
}

fn isolation_name(isolation: Isolation) -> &'static str {
    match isolation {
        Isolation::ReadCommitted => "rc",
        Isolation::RepeatableRead => "rr",
        Isolation::Serializable => "ser",
        Isolation::ServerDefault => "default",
    }
}

async fn open(pool: &PgPool, max_connections: u32) -> PgPool {
    super::template_pool(&dsn_for(pool).await, max_connections).await
}

async fn stored_job(pool: &PgPool, id: &str) -> StoredJob {
    let row = sqlx::query(
        "SELECT id::text AS id_text, kind, payload::text AS payload, \
         unique_key IS NULL AS unique_key_is_null, state, attempts, claim_generation, \
         claim_expires_at IS NULL AS claim_expires_at_is_null, \
         finished_at IS NULL AS finished_at_is_null, \
         failure_reason IS NULL AS failure_reason_is_null, \
         error_summary IS NULL AS error_summary_is_null, \
         trace_context IS NULL AS trace_context_is_null \
         FROM background_jobs WHERE id::text = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("the committed job");
    let payload: String = row.try_get("payload").expect("payload");
    StoredJob {
        id_text: row.try_get("id_text").expect("id_text"),
        kind: row.try_get("kind").expect("kind"),
        payload: serde_json::from_str(&payload).expect("JSONB payload"),
        unique_key_is_null: row
            .try_get("unique_key_is_null")
            .expect("unique_key_is_null"),
        state: row.try_get("state").expect("state"),
        attempts: row.try_get("attempts").expect("attempts"),
        claim_generation: row.try_get("claim_generation").expect("claim_generation"),
        claim_expires_at_is_null: row
            .try_get("claim_expires_at_is_null")
            .expect("claim_expires_at_is_null"),
        finished_at_is_null: row
            .try_get("finished_at_is_null")
            .expect("finished_at_is_null"),
        failure_reason_is_null: row
            .try_get("failure_reason_is_null")
            .expect("failure_reason_is_null"),
        error_summary_is_null: row
            .try_get("error_summary_is_null")
            .expect("error_summary_is_null"),
        trace_context_is_null: row
            .try_get("trace_context_is_null")
            .expect("trace_context_is_null"),
    }
}

async fn rows_for_key(pool: &PgPool, kind: &str, key: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM background_jobs WHERE kind = $1 AND unique_key = $2")
        .bind(kind)
        .bind(key)
        .fetch_one(pool)
        .await
        .expect("a key count")
}

async fn states_for_key(pool: &PgPool, key: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT state FROM background_jobs WHERE kind = $1 AND unique_key = $2 ORDER BY state",
    )
    .bind(Note::NAME)
    .bind(key)
    .fetch_all(pool)
    .await
    .expect("states")
}

async fn unkeyed(pool: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM background_jobs WHERE kind = $1 AND unique_key IS NULL",
    )
    .bind(kind)
    .fetch_one(pool)
    .await
    .expect("an unkeyed count")
}

async fn visible(pool: &PgPool, id: &str) -> bool {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM background_jobs WHERE id::text = $1)")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("visibility")
}

async fn write<K: JobKind>(
    pool: &PgPool,
    isolation: Isolation,
    payload: &K,
    unique_key: Option<&str>,
) -> Enqueued {
    let options = EnqueueOptions {
        delay: Duration::ZERO,
        unique_key,
    };
    in_tx_with(pool, writable(isolation), async |tx| {
        enqueue(tx, payload, options)
            .await
            .map_err(AttemptError::from)
    })
    .await
    .expect("enqueue commits")
}

async fn commit_note(pool: &PgPool, key: &str) -> JobId {
    let enqueued = in_tx(pool, async |tx| -> Result<Enqueued, AttemptError> {
        enqueue(
            tx,
            &Note {
                text: key.to_owned(),
            },
            keyed(key),
        )
        .await
        .map_err(AttemptError::from)
    })
    .await
    .expect("the holder commits");
    created(enqueued)
}

async fn insert_terminal(pool: &PgPool, key: &str, failed: bool) {
    let sql = if failed {
        "INSERT INTO background_jobs \
         (kind, payload, unique_key, state, failure_reason, finished_at, not_before) \
         VALUES ($1, $2::jsonb, $3::text COLLATE \"C\", 'failed', 'exhausted', \
                 statement_timestamp(), statement_timestamp())"
    } else {
        "INSERT INTO background_jobs \
         (kind, payload, unique_key, state, finished_at, not_before) \
         VALUES ($1, $2::jsonb, $3::text COLLATE \"C\", 'completed', \
                 statement_timestamp(), statement_timestamp())"
    };
    let inserted = sqlx::query(sql)
        .bind(Note::NAME)
        .bind("{}")
        .bind(key)
        .execute(pool)
        .await
        .expect("the terminal holder");
    assert_eq!(inserted.rows_affected(), 1);
}

async fn insert_running(pool: &PgPool, key: &str) {
    let inserted = sqlx::query(
        "INSERT INTO background_jobs \
         (kind, payload, unique_key, state, attempts, claim_generation, not_before, claim_expires_at) \
         VALUES ($1, $2::jsonb, $3::text COLLATE \"C\", 'running', 1, \
                 nextval('background_jobs_claim_generation'), \
                 statement_timestamp(), statement_timestamp() + interval '30 seconds')",
    )
        .bind(Note::NAME)
        .bind("{}")
        .bind(key)
    .execute(pool)
    .await
    .expect("the running holder");
    assert_eq!(inserted.rows_affected(), 1);
}

async fn claim_like(conn: &mut PgConnection, key: &str) {
    let updated = sqlx::query(CLAIM_LIKE)
        .bind(Note::NAME)
        .bind(key)
        .execute(&mut *conn)
        .await
        .expect("the claim-like write");
    assert_eq!(updated.rows_affected(), 1, "the live holder was claimed");
}

async fn complete_holder(pool: &PgPool, key: &str) {
    let updated = sqlx::query(
        "UPDATE background_jobs \
         SET state = 'completed', finished_at = statement_timestamp() \
         WHERE kind = $1 AND unique_key = $2 AND state = 'pending'",
    )
    .bind(Note::NAME)
    .bind(key)
    .execute(pool)
    .await
    .expect("the holder becomes terminal");
    assert_eq!(updated.rows_affected(), 1);
}

async fn still_usable(tx: &mut Tx<'_>) {
    let one: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&mut *connection(tx))
        .await
        .expect("the transaction stays usable");
    assert_eq!(one, 1);
}

fn refused(
    result: Result<Enqueued, EnqueueError>,
    what: &str,
    check: impl FnOnce(&EnqueueError) -> bool,
) {
    match result {
        Err(err) if check(&err) => {}
        other => panic!("expected {what}, got {other:?}"),
    }
}

async fn expect_serialization(tx: &mut Tx<'_>, key: &str) -> Result<Enqueued, AttemptError> {
    let err = match enqueue(
        tx,
        &Note {
            text: key.to_owned(),
        },
        keyed(key),
    )
    .await
    {
        Err(EnqueueError::Database(err)) => err,
        other => panic!("expected a serialization failure, got {other:?}"),
    };
    assert_eq!(super::sqlstate(&err).as_deref(), Some("40001"), "{err}");
    assert!(retryable(&err), "{err}");
    let next_err = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&mut *connection(tx))
        .await
        .expect_err("the transaction is aborted");
    assert_eq!(super::sqlstate(&next_err).as_deref(), Some("25P02"));
    Err(AttemptError::Rejected)
}

#[derive(Clone, Copy)]
enum AfterSnapshot {
    Created,
    Serialization,
    Duplicate,
}

async fn after_snapshot(
    tx: &mut Tx<'_>,
    key: &str,
    action: AfterSnapshot,
) -> Result<Enqueued, AttemptError> {
    match action {
        AfterSnapshot::Created => enqueue(
            tx,
            &Note {
                text: "next".to_owned(),
            },
            keyed(key),
        )
        .await
        .map_err(AttemptError::from),
        AfterSnapshot::Serialization => expect_serialization(tx, key).await,
        AfterSnapshot::Duplicate => {
            let enqueued = enqueue(
                tx,
                &Note {
                    text: "racer".to_owned(),
                },
                keyed(key),
            )
            .await
            .map_err(AttemptError::from)?;
            still_usable(tx).await;
            Ok(enqueued)
        }
    }
}

fn spawn_snapshot_caller(
    pool: PgPool,
    isolation: Isolation,
    key: String,
    seen_rows: i64,
    snapshot: Arc<Notify>,
    go: Arc<Notify>,
    action: AfterSnapshot,
) -> JoinHandle<Result<Enqueued, AttemptError>> {
    tokio::spawn(async move {
        in_tx_with(&pool, writable(isolation), async move |tx| {
            let seen: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM background_jobs WHERE kind = $1 AND unique_key = $2",
            )
            .bind(Note::NAME)
            .bind(&key)
            .fetch_one(&mut *connection(tx))
            .await?;
            assert_eq!(seen, seen_rows, "rows visible to the caller's snapshot");
            snapshot.notify_one();
            go.notified().await;
            after_snapshot(tx, &key, action).await
        })
        .await
    })
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e1_e2_e8_a_committed_enqueue_returns_created_and_one_row(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let note = Note {
        text: "kept".to_owned(),
    };
    let expected = serde_json::to_value(&note).expect("payload JSON");
    let id = in_tx(&jobs, async |tx| -> Result<JobId, AttemptError> {
        let id = created(enqueue(tx, &note, EnqueueOptions::default()).await?);
        let on_time: bool = sqlx::query_scalar(
            "SELECT not_before >= now() AND not_before <= clock_timestamp() \
             FROM background_jobs WHERE id::text = $1",
        )
        .bind(id.to_string())
        .fetch_one(&mut *connection(tx))
        .await?;
        assert!(
            on_time,
            "not_before is outside the transaction's statement times"
        );
        Ok(id)
    })
    .await
    .expect("the enqueue commits");

    let row = stored_job(&jobs, &id.to_string()).await;
    assert_eq!(row.id_text, id.to_string());
    assert_eq!(row.kind, Note::NAME);
    assert_eq!(row.payload, expected);
    assert!(row.unique_key_is_null);
    assert_eq!(row.state, "pending");
    assert_eq!(row.attempts, 0);
    assert_eq!(row.claim_generation, 0);
    assert!(row.claim_expires_at_is_null);
    assert!(row.finished_at_is_null);
    assert!(row.failure_reason_is_null);
    assert!(row.error_summary_is_null);
    assert!(row.trace_context_is_null);
    assert_eq!(super::job_count(&jobs).await, 1);
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e2_a_closure_error_after_enqueue_leaves_no_job(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let result = in_tx(&jobs, async |tx| -> Result<(), AttemptError> {
        let enqueued = enqueue(
            tx,
            &Note {
                text: "rolled".to_owned(),
            },
            EnqueueOptions::default(),
        )
        .await?;
        assert!(matches!(enqueued, Enqueued::Created(_)));
        Err(AttemptError::Rejected)
    })
    .await;
    assert!(
        matches!(&result, Err(AttemptError::Rejected)),
        "{}",
        result.as_ref().err().map(explain).unwrap_or_default()
    );
    assert_eq!(super::job_count(&jobs).await, 0);
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e2_a_commit_the_server_rejects_leaves_no_job(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    sqlx::query(
        "CREATE TABLE staged ( \
             id int PRIMARY KEY, \
             other int, \
             CONSTRAINT staged_other UNIQUE (other) DEFERRABLE INITIALLY DEFERRED)",
    )
    .execute(&jobs)
    .await
    .expect("the staged table");

    let result = in_tx(&jobs, async |tx| -> Result<(), AttemptError> {
        let enqueued = enqueue(
            tx,
            &Note {
                text: "rejected".to_owned(),
            },
            EnqueueOptions::default(),
        )
        .await?;
        assert!(matches!(enqueued, Enqueued::Created(_)));
        sqlx::query("INSERT INTO staged (id, other) VALUES (1, 1), (2, 1)")
            .execute(&mut *connection(tx))
            .await?;
        Ok(())
    })
    .await;

    match result {
        Err(AttemptError::Tx(TxError::CommitFailed(err))) => {
            assert_eq!(super::sqlstate(&err).as_deref(), Some("23505"), "{err}");
        }
        other => panic!("expected CommitFailed, got {other:?}"),
    }
    assert_eq!(super::job_count(&jobs).await, 0);
    let staged: i64 = sqlx::query_scalar("SELECT count(*) FROM staged")
        .fetch_one(&jobs)
        .await
        .expect("the staged count");
    assert_eq!(staged, 0);
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e2_an_open_transaction_hides_the_job_until_commit(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (id, ()) = tokio::join!(
        async {
            in_tx(&jobs, {
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                async move |tx| -> Result<JobId, AttemptError> {
                    let id = created(
                        enqueue(
                            tx,
                            &Note {
                                text: "hidden".to_owned(),
                            },
                            EnqueueOptions::default(),
                        )
                        .await?,
                    );
                    entered.notify_one();
                    release.notified().await;
                    Ok(id)
                }
            })
            .await
            .expect("commit")
        },
        async {
            entered.notified().await;
            assert_eq!(super::job_count(&pool).await, 0);
            release.notify_one();
        },
    );
    let id_text = id.to_string();
    assert_eq!(super::job_count(&pool).await, 1);
    assert!(visible(&pool, &id_text).await);
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e4_validation_failures_leave_the_transaction_usable(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let note = Note {
        text: "ok".to_owned(),
    };
    let too_big = Raw("a".repeat(MAX_PAYLOAD_BYTES - 1));
    let id = in_tx(&jobs, async |tx| -> Result<JobId, AttemptError> {
        refused(
            enqueue(tx, &BadKind, EnqueueOptions::default()).await,
            "InvalidKind",
            |err| matches!(err, EnqueueError::InvalidKind("Bad")),
        );
        still_usable(tx).await;
        refused(
            enqueue(
                tx,
                &note,
                EnqueueOptions {
                    delay: Duration::ZERO,
                    unique_key: Some(""),
                },
            )
            .await,
            "InvalidUniqueKey",
            |err| matches!(err, EnqueueError::InvalidUniqueKey),
        );
        still_usable(tx).await;
        refused(
            enqueue(
                tx,
                &note,
                EnqueueOptions {
                    delay: MAX_DELAY + Duration::from_nanos(1),
                    unique_key: None,
                },
            )
            .await,
            "InvalidDelay",
            |err| matches!(err, EnqueueError::InvalidDelay),
        );
        still_usable(tx).await;
        refused(
            enqueue(tx, &too_big, EnqueueOptions::default()).await,
            "PayloadTooLarge",
            |err| {
                matches!(err, EnqueueError::PayloadTooLarge { bytes } if *bytes > MAX_PAYLOAD_BYTES)
            },
        );
        still_usable(tx).await;
        refused(
            enqueue(tx, &Raw("\0".to_owned()), EnqueueOptions::default()).await,
            "PayloadContainsNul",
            |err| matches!(err, EnqueueError::PayloadContainsNul),
        );
        still_usable(tx).await;
        refused(
            enqueue(tx, &Refuse, EnqueueOptions::default()).await,
            "Serialize",
            |err| matches!(err, EnqueueError::Serialize(_)),
        );
        still_usable(tx).await;
        Ok(created(enqueue(tx, &note, EnqueueOptions::default()).await?))
    })
    .await
    .expect("the valid enqueue commits");

    assert_eq!(super::job_count(&jobs).await, 1);
    let row = stored_job(&jobs, &id.to_string()).await;
    assert_eq!(row.kind, Note::NAME);
    assert_eq!(row.id_text, id.to_string());
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrations = false)]
async fn e4_a_missing_schema_returns_42p01_and_aborts_the_transaction(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let result = in_tx(&jobs, async |tx| -> Result<(), AttemptError> {
        let err = match enqueue(
            tx,
            &Note {
                text: "missing".to_owned(),
            },
            EnqueueOptions::default(),
        )
        .await
        {
            Err(EnqueueError::Database(err)) => err,
            other => panic!("expected a database failure, got {other:?}"),
        };
        assert_eq!(super::sqlstate(&err).as_deref(), Some("42P01"), "{err}");
        let next_err = sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&mut *connection(tx))
            .await
            .expect_err("the transaction is aborted");
        assert_eq!(super::sqlstate(&next_err).as_deref(), Some("25P02"));
        Err(AttemptError::Rejected)
    })
    .await;
    assert!(
        matches!(&result, Err(AttemptError::Rejected)),
        "{}",
        result.as_ref().err().map(explain).unwrap_or_default()
    );
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e4_a_read_only_transaction_returns_25006_and_writes_nothing(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let result = in_tx_with(
        &jobs,
        TxOptions {
            isolation: Isolation::ReadCommitted,
            read_only: true,
        },
        async |tx| -> Result<(), AttemptError> {
            let err = match enqueue(
                tx,
                &Note {
                    text: "readonly".to_owned(),
                },
                EnqueueOptions::default(),
            )
            .await
            {
                Err(EnqueueError::Database(err)) => err,
                other => panic!("expected a database failure, got {other:?}"),
            };
            assert_eq!(super::sqlstate(&err).as_deref(), Some("25006"), "{err}");
            let next_err = sqlx::query_scalar::<_, i32>("SELECT 1")
                .fetch_one(&mut *connection(tx))
                .await
                .expect_err("the transaction is aborted");
            assert_eq!(super::sqlstate(&next_err).as_deref(), Some("25P02"));
            Err(AttemptError::Rejected)
        },
    )
    .await;
    assert!(
        matches!(&result, Err(AttemptError::Rejected)),
        "{}",
        result.as_ref().err().map(explain).unwrap_or_default()
    );
    assert_eq!(super::job_count(&jobs).await, 0);
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e5_a_one_hour_delay_is_the_statement_time_plus_one_hour(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let id = in_tx(&jobs, async |tx| -> Result<JobId, AttemptError> {
        let id = created(
            enqueue(
                tx,
                &Note {
                    text: "later".to_owned(),
                },
                EnqueueOptions {
                    delay: Duration::from_secs(3_600),
                    unique_key: None,
                },
            )
            .await?,
        );
        let on_time: bool = sqlx::query_scalar(
            "SELECT not_before >= now() + interval '1 hour' \
             AND not_before <= clock_timestamp() + interval '1 hour' \
             FROM background_jobs WHERE id::text = $1",
        )
        .bind(id.to_string())
        .fetch_one(&mut *connection(tx))
        .await?;
        assert!(
            on_time,
            "not_before is outside now() + 1 hour .. clock_timestamp() + 1 hour"
        );
        Ok(id)
    })
    .await
    .expect("the delayed enqueue commits");
    assert!(visible(&jobs, &id.to_string()).await);
    assert_eq!(super::job_count(&jobs).await, 1);
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e5_a_sub_microsecond_delay_is_accepted(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let id = in_tx(&jobs, async |tx| -> Result<JobId, AttemptError> {
        let id = created(
            enqueue(
                tx,
                &Note {
                    text: "fine".to_owned(),
                },
                EnqueueOptions {
                    delay: Duration::new(1, 1),
                    unique_key: None,
                },
            )
            .await?,
        );
        let on_time: bool = sqlx::query_scalar(
            "SELECT not_before >= now() + interval '1 second' \
             AND not_before <= clock_timestamp() + interval '1 second' \
             FROM background_jobs WHERE id::text = $1",
        )
        .bind(id.to_string())
        .fetch_one(&mut *connection(tx))
        .await?;
        assert!(
            on_time,
            "the sub-microsecond part was not accepted as a delay"
        );
        Ok(id)
    })
    .await
    .expect("the truncated delay commits");
    assert!(visible(&jobs, &id.to_string()).await);
    assert_eq!(super::job_count(&jobs).await, 1);
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_any_isolation_writes_when_no_holder_and_a_key_is_per_kind(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    for isolation in ANY {
        let name = isolation_name(isolation);
        let free = format!("free-{name}");
        let created = write(&jobs, isolation, &Note { text: free.clone() }, Some(&free)).await;
        assert!(matches!(created, Enqueued::Created(_)), "{isolation:?}");
        assert_eq!(rows_for_key(&jobs, Note::NAME, &free).await, 1);

        let shared = format!("shared-{name}");
        let note = write(
            &jobs,
            isolation,
            &Note {
                text: shared.clone(),
            },
            Some(&shared),
        )
        .await;
        let other = write(&jobs, isolation, &Other { n: 1 }, Some(&shared)).await;
        assert!(matches!(note, Enqueued::Created(_)), "{isolation:?}");
        assert!(matches!(other, Enqueued::Created(_)), "{isolation:?}");
        assert_eq!(rows_for_key(&jobs, Note::NAME, &shared).await, 1);
        assert_eq!(rows_for_key(&jobs, Other::NAME, &shared).await, 1);

        let before = unkeyed(&jobs, Note::NAME).await;
        let first = write(
            &jobs,
            isolation,
            &Note {
                text: format!("plain-{name}"),
            },
            None,
        )
        .await;
        let second = write(
            &jobs,
            isolation,
            &Note {
                text: format!("again-{name}"),
            },
            None,
        )
        .await;
        assert!(matches!(first, Enqueued::Created(_)), "{isolation:?}");
        assert!(matches!(second, Enqueued::Created(_)), "{isolation:?}");
        assert_eq!(unkeyed(&jobs, Note::NAME).await, before + 2);
    }
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_any_isolation_writes_beside_a_terminal_holder(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    for isolation in ANY {
        for failed in [false, true] {
            let state = if failed { "failed" } else { "completed" };
            let key = format!("{state}-{}", isolation_name(isolation));
            insert_terminal(&jobs, &key, failed).await;
            let enqueued = write(&jobs, isolation, &Note { text: key.clone() }, Some(&key)).await;
            assert!(
                matches!(enqueued, Enqueued::Created(_)),
                "{isolation:?} {state}"
            );
            assert_eq!(
                states_for_key(&jobs, &key).await,
                vec![state.to_owned(), "pending".to_owned()]
            );
        }
    }
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_repeatable_read_writes_when_a_live_holder_became_terminal_after_the_snapshot(
    pool: PgPool,
) {
    let jobs = open(&pool, 3).await;
    let key = "became-terminal";
    let _holder = commit_note(&jobs, key).await;
    let snapshot = Arc::new(Notify::new());
    let go = Arc::new(Notify::new());
    let _unblock = Unblock(Arc::clone(&go));
    let caller = spawn_snapshot_caller(
        jobs.clone(),
        Isolation::RepeatableRead,
        key.to_owned(),
        1,
        Arc::clone(&snapshot),
        Arc::clone(&go),
        AfterSnapshot::Created,
    );
    super::bounded("the snapshot", snapshot.notified()).await;
    complete_holder(&jobs, key).await;
    go.notify_one();
    let enqueued = super::bounded("the caller", caller)
        .await
        .expect("the caller joins")
        .expect("the enqueue commits");
    assert!(matches!(enqueued, Enqueued::Created(_)));
    assert_eq!(
        states_for_key(&jobs, key).await,
        vec!["completed".to_owned(), "pending".to_owned()]
    );
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_read_committed_duplicate_against_a_live_holder_keeps_the_transaction_usable(
    pool: PgPool,
) {
    let jobs = open(&pool, 1).await;
    for (key, running) in [("live-pending", false), ("live-running", true)] {
        if running {
            insert_running(&jobs, key).await;
        } else {
            let _holder = commit_note(&jobs, key).await;
        }
        let enqueued = in_tx_with(
            &jobs,
            writable(Isolation::ReadCommitted),
            async |tx| -> Result<Enqueued, AttemptError> {
                let seen: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM background_jobs \
                     WHERE kind = $1 AND unique_key = $2 AND state IN ('pending', 'running')",
                )
                .bind(Note::NAME)
                .bind(key)
                .fetch_one(&mut *connection(tx))
                .await?;
                assert_eq!(seen, 1, "a live holder is visible");
                let enqueued = enqueue(
                    tx,
                    &Note {
                        text: key.to_owned(),
                    },
                    keyed(key),
                )
                .await?;
                still_usable(tx).await;
                Ok(enqueued)
            },
        )
        .await
        .expect("the duplicate commits");
        assert_eq!(enqueued, Enqueued::Duplicate, "{key}");
        assert_eq!(rows_for_key(&jobs, Note::NAME, key).await, 1, "{key}");
    }
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_read_committed_inflight_insert_keeps_commit_and_rollback_oracles(pool: PgPool) {
    let jobs = open(&pool, 3).await;
    for (label, commits) in [("commit", true), ("rollback", false)] {
        let key = format!("race-{label}");
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let holder = tokio::spawn({
            let jobs = jobs.clone();
            let key = key.clone();
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            async move {
                in_tx(&jobs, async move |tx| -> Result<JobId, AttemptError> {
                    let id = created(
                        enqueue(
                            tx,
                            &Note {
                                text: "holder".to_owned(),
                            },
                            keyed(&key),
                        )
                        .await?,
                    );
                    entered.notify_one();
                    release.notified().await;
                    if commits {
                        Ok(id)
                    } else {
                        Err(AttemptError::Rejected)
                    }
                })
                .await
            }
        });
        entered.notified().await;
        let jobs_caller = jobs.clone();
        let caller_key = key.clone();
        let caller = tokio::spawn(async move {
            in_tx_with(
                &jobs_caller,
                writable(Isolation::ReadCommitted),
                async move |tx| -> Result<Enqueued, AttemptError> {
                    let enqueued = enqueue(
                        tx,
                        &Note {
                            text: "racer".to_owned(),
                        },
                        keyed(&caller_key),
                    )
                    .await?;
                    still_usable(tx).await;
                    Ok(enqueued)
                },
            )
            .await
        });
        let enqueued = super::join_after_lock_wait(
            &pool,
            async {
                release.notify_one();
            },
            caller,
        )
        .await
        .expect("the caller commits");
        let held = super::bounded("the holder finishes", holder)
            .await
            .expect("holder joins");
        assert_eq!(rows_for_key(&jobs, Note::NAME, &key).await, 1, "{label}");
        if commits {
            let held_id = held.expect("the holder commits");
            assert_eq!(enqueued, Enqueued::Duplicate, "{label}");
            assert!(visible(&jobs, &held_id.to_string()).await, "{label}");
        } else {
            assert!(matches!(held, Err(AttemptError::Rejected)), "{label}");
            let id = created(enqueued);
            assert!(visible(&jobs, &id.to_string()).await, "{label}");
        }
    }
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_snapshot_isolation_duplicate_when_the_snapshot_sees_the_live_holder(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    for isolation in SNAPSHOT {
        let key = format!("seen-{}", isolation_name(isolation));
        let _holder = commit_note(&jobs, &key).await;
        let enqueued = in_tx_with(
            &jobs,
            writable(isolation),
            async |tx| -> Result<Enqueued, AttemptError> {
                let seen: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM background_jobs \
                     WHERE kind = $1 AND unique_key = $2 AND state = 'pending'",
                )
                .bind(Note::NAME)
                .bind(&key)
                .fetch_one(&mut *connection(tx))
                .await?;
                assert_eq!(seen, 1, "the snapshot sees the live holder");
                let enqueued = enqueue(tx, &Note { text: key.clone() }, keyed(&key)).await?;
                still_usable(tx).await;
                Ok(enqueued)
            },
        )
        .await
        .expect("the duplicate commits");
        assert_eq!(enqueued, Enqueued::Duplicate, "{isolation:?}");
        assert_eq!(rows_for_key(&jobs, Note::NAME, &key).await, 1);
    }
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_snapshot_isolation_40001_when_a_live_job_is_inserted_after_the_snapshot(pool: PgPool) {
    let jobs = open(&pool, 3).await;
    for isolation in SNAPSHOT {
        let key = format!("inserted-{}", isolation_name(isolation));
        let snapshot = Arc::new(Notify::new());
        let go = Arc::new(Notify::new());
        let _unblock = Unblock(Arc::clone(&go));
        let caller = spawn_snapshot_caller(
            jobs.clone(),
            isolation,
            key.clone(),
            0,
            Arc::clone(&snapshot),
            Arc::clone(&go),
            AfterSnapshot::Serialization,
        );
        super::bounded("the snapshot", snapshot.notified()).await;
        let inserted = in_tx(&jobs, async |tx| -> Result<Enqueued, AttemptError> {
            enqueue(
                tx,
                &Note {
                    text: "other".to_owned(),
                },
                keyed(&key),
            )
            .await
            .map_err(AttemptError::from)
        })
        .await
        .expect("the other transaction commits");
        assert!(matches!(inserted, Enqueued::Created(_)), "{isolation:?}");
        go.notify_one();
        let result = super::bounded("the caller", caller)
            .await
            .expect("the caller joins");
        assert!(
            matches!(&result, Err(AttemptError::Rejected)),
            "{isolation:?} {}",
            result.as_ref().err().map(explain).unwrap_or_default()
        );
        assert_eq!(rows_for_key(&jobs, Note::NAME, &key).await, 1);
    }
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_repeatable_read_40001_when_a_claim_write_commits_after_the_snapshot(pool: PgPool) {
    let jobs = open(&pool, 3).await;
    let key = "claim-committed";
    let _holder = commit_note(&jobs, key).await;
    let snapshot = Arc::new(Notify::new());
    let go = Arc::new(Notify::new());
    let _unblock = Unblock(Arc::clone(&go));
    let caller = spawn_snapshot_caller(
        jobs.clone(),
        Isolation::RepeatableRead,
        key.to_owned(),
        1,
        Arc::clone(&snapshot),
        Arc::clone(&go),
        AfterSnapshot::Serialization,
    );
    super::bounded("the snapshot", snapshot.notified()).await;
    in_tx(&jobs, async |tx| -> Result<(), AttemptError> {
        claim_like(connection(tx), key).await;
        Ok(())
    })
    .await
    .expect("the claim commits");
    go.notify_one();
    let result = super::bounded("the caller", caller)
        .await
        .expect("the caller joins");
    assert!(
        matches!(&result, Err(AttemptError::Rejected)),
        "{}",
        result.as_ref().err().map(explain).unwrap_or_default()
    );
    assert_eq!(states_for_key(&jobs, key).await, vec!["running".to_owned()]);
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e6_repeatable_read_inflight_claim_keeps_commit_and_rollback_oracles(pool: PgPool) {
    let jobs = open(&pool, 3).await;
    for (label, commits) in [("commit", true), ("rollback", false)] {
        let key = format!("claim-waits-{label}");
        let _holder = commit_note(&jobs, &key).await;
        let snapshot = Arc::new(Notify::new());
        let go = Arc::new(Notify::new());
        let _unblock = Unblock(Arc::clone(&go));
        let action = if commits {
            AfterSnapshot::Serialization
        } else {
            AfterSnapshot::Duplicate
        };
        let caller = spawn_snapshot_caller(
            jobs.clone(),
            Isolation::RepeatableRead,
            key.clone(),
            1,
            Arc::clone(&snapshot),
            Arc::clone(&go),
            action,
        );
        super::bounded("the snapshot", snapshot.notified()).await;
        let mut claim = jobs.begin().await.expect("the claim transaction");
        claim_like(&mut claim, &key).await;
        go.notify_one();
        let result = super::join_after_lock_wait(
            &pool,
            async {
                if commits {
                    claim.commit().await.expect("the claim commits");
                } else {
                    claim.rollback().await.expect("the claim rolls back");
                }
            },
            caller,
        )
        .await;
        if commits {
            assert!(
                matches!(&result, Err(AttemptError::Rejected)),
                "{label}: {}",
                result.as_ref().err().map(explain).unwrap_or_default()
            );
            assert_eq!(
                states_for_key(&jobs, &key).await,
                vec!["running".to_owned()]
            );
        } else {
            assert_eq!(
                result.expect("the caller commits"),
                Enqueued::Duplicate,
                "{label}"
            );
            assert_eq!(
                states_for_key(&jobs, &key).await,
                vec!["pending".to_owned()]
            );
        }
    }
    super::close(&[&jobs]).await;
}
