//! Recovery observes the committed queue, including arbitration and lost commit replies.

use infra_jobs::operator::{
    self, Inspection, InspectionResult, JobState, OperatorError, RecoveryTarget,
};
use infra_jobs::{EnqueueOptions, Enqueued, JobKind, enqueue};
use infra_postgres::{Dsn, Isolation, PgPool, TxOptions, in_tx, in_tx_with};
use integration_tests::{DATABASE_URL, dsn_for, url_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use uuid::Uuid;

use super::commit_proxy::{CommitProxy, Fault};

const KIND: &str = "test.operator";
const SECRET: &str = "private-job-content";

#[derive(Serialize, Deserialize)]
struct Note {
    text: String,
}

impl JobKind for Note {
    const NAME: &'static str = KIND;
}

async fn open(pool: &PgPool) -> PgPool {
    super::template_pool(&dsn_for(pool).await, 3).await
}

// A completed failed cycle derived from the queue constraints. No engine is
// needed here: execution.rs owns creation of failures by handlers and rescue.
async fn failed(pool: &PgPool, id: Uuid, key: &str) -> RecoveryTarget {
    let version: i64 = sqlx::query_scalar(
        "INSERT INTO background_jobs
         (id, kind, payload, unique_key, state, attempts, claim_generation,
          not_before, attempted_by, finished_at, failure_reason, error_summary,
          errors, trace_context, trace_state)
         VALUES ($1, $2, $3, $4, 'failed', 3,
                 nextval('background_jobs_claim_generation'), '2020-01-01T00:00:00Z',
                 $1, '2020-01-02T00:00:00Z', 'exhausted', $5, $6, $5, $5)
         RETURNING claim_generation",
    )
    .bind(id)
    .bind(KIND)
    .bind(json!({"text": SECRET}))
    .bind(key)
    .bind(SECRET)
    .bind(json!([{"attempt": 3, "at": "2020-01-02T00:00:00Z", "error": SECRET}]))
    .fetch_one(pool)
    .await
    .expect("the failed cycle is stored");
    RecoveryTarget::new(&id.to_string(), KIND, &version.to_string()).unwrap()
}

async fn stored(pool: &PgPool, target: &RecoveryTarget) -> Value {
    sqlx::query_scalar("SELECT to_jsonb(j) FROM background_jobs j WHERE id::text = $1")
        .bind(target.id().to_string())
        .fetch_one(pool)
        .await
        .expect("the durable job")
}

async fn inspect(pool: &PgPool, request: &Inspection) -> InspectionResult {
    in_tx_with(
        pool,
        TxOptions {
            isolation: Isolation::ReadCommitted,
            read_only: true,
        },
        async |tx| operator::inspect(tx, request).await,
    )
    .await
    .expect("read-only inspection commits")
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn redrive_preserves_identity_archives_each_cycle_and_rejects_old_versions(pool: PgPool) {
    let jobs = open(&pool).await;
    let target = failed(&pool, Uuid::new_v4(), SECRET).await;
    let before = stored(&pool, &target).await;
    let wrong_kind = RecoveryTarget::new(
        &target.id().to_string(),
        "test.other",
        &target.version().to_string(),
    )
    .unwrap();
    assert!(matches!(
        in_tx(&jobs, async |tx| operator::discard(tx, &wrong_kind).await).await,
        Err(OperatorError::Stale)
    ));
    let receipt = in_tx(&jobs, async |tx| operator::redrive(tx, &target).await)
        .await
        .expect("redrive commits");
    assert_eq!(receipt.target, target);
    assert_ne!(receipt.new_version, target.version().to_string());
    let after = stored(&pool, &target).await;
    for field in [
        "id",
        "kind",
        "payload",
        "unique_key",
        "created_at",
        "trace_context",
        "trace_state",
    ] {
        assert_eq!(after[field], before[field], "preserved {field}");
    }
    assert_eq!(after["state"], "pending");
    assert_eq!(after["attempts"], 0);
    assert_eq!(after["claim_generation"].to_string(), receipt.new_version);
    assert_eq!(after["errors"], json!([]));
    for field in [
        "claim_expires_at",
        "attempted_by",
        "finished_at",
        "failure_reason",
        "error_summary",
    ] {
        assert!(after[field].is_null(), "reset {field}");
    }
    let history = after["recovery_history"].as_array().unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["version"], target.version().to_string());
    for field in [
        "attempts",
        "failure_reason",
        "finished_at",
        "attempted_by",
        "error_summary",
        "errors",
    ] {
        assert_eq!(history[0][field], before[field], "archived {field}");
    }
    assert_eq!(history[0].as_object().unwrap().len(), 8);
    assert_eq!(history[0]["redriven_at"], after["not_before"]);

    // A later failed cycle must refuse the old version even before another
    // claim advances it: the redrive itself invalidates the previous token.
    sqlx::query(
        "UPDATE background_jobs SET state = 'failed', attempts = 1,
                 failure_reason = 'permanent', finished_at = statement_timestamp()
                 WHERE id::text = $1",
    )
    .bind(target.id().to_string())
    .execute(&pool)
    .await
    .unwrap();
    for discard in [false, true] {
        let error = in_tx(&jobs, async |tx| {
            if discard {
                operator::discard(tx, &target).await.map(|_| ())
            } else {
                operator::redrive(tx, &target).await.map(|_| ())
            }
        })
        .await
        .unwrap_err();
        assert!(matches!(error, OperatorError::Stale));
    }
    let next = RecoveryTarget::new(&target.id().to_string(), KIND, &receipt.new_version).unwrap();
    in_tx(&jobs, async |tx| operator::redrive(tx, &next).await)
        .await
        .unwrap();
    let final_row = stored(&pool, &target).await;
    let history = final_row["recovery_history"].as_array().unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0], after["recovery_history"][0]);
    assert_eq!(history[1]["version"], receipt.new_version);
    assert_eq!(history[1]["attempts"], 1);
    super::close(&[&jobs]).await;
}

// template:begin outbox:test-jobs-operator-outbox
mod outbox {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use domain_events::{Event, EventPayload};
    use infra_messaging::{PreparedEvent, outbox::OutboxEnqueued};

    use super::*;

    impl EventPayload for Note {
        const EVENT_TYPE: &'static str = "test.operator.created";
        const SCHEMA_VERSION: u16 = 1;
    }

    #[sqlx::test(migrator = "migrate::MIGRATOR")]
    async fn redrive_keeps_prepared_outbox_bytes_routing_and_publication_identity(pool: PgPool) {
        let jobs = open(&pool).await;
        let event = Event {
            id: "operator-recovery-event".into(),
            occurred_at: time::UtcDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            payload: Note {
                text: "literal \\u0000 and 1.0".into(),
            },
        };
        let prepared = PreparedEvent::prepare("events.operator.created", &event, 1024).unwrap();
        in_tx(&jobs, async |tx| {
            assert_eq!(prepared.enqueue(tx).await.unwrap(), OutboxEnqueued::Created);
            Ok::<_, OperatorError>(())
        })
        .await
        .unwrap();
        let (id, version): (String, i64) = sqlx::query_as(
            "UPDATE background_jobs SET state = 'failed', attempts = 1,
             claim_generation = nextval('background_jobs_claim_generation'),
             failure_reason = 'permanent', finished_at = statement_timestamp()
             WHERE kind = 'publish_domain_event' RETURNING id::text, claim_generation",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let target =
            RecoveryTarget::new(&id, "publish_domain_event", &version.to_string()).unwrap();
        let before = stored(&pool, &target).await;
        in_tx(&jobs, async |tx| operator::redrive(tx, &target).await)
            .await
            .unwrap();
        let after = stored(&pool, &target).await;
        assert_eq!(after["payload"], before["payload"]);
        assert_eq!(after["unique_key"], before["unique_key"]);
        assert_eq!(after["id"], before["id"]);
        assert_eq!(super::super::job_count(&pool).await, 1);
        let intent = &after["payload"];
        assert_eq!(intent["subject"], "events.operator.created");
        assert_eq!(intent["message_id"], "operator-recovery-event");
        assert_eq!(intent["publication_id"], "operator-recovery-event");
        assert_eq!(intent["event_type"], "test.operator.created");
        assert_eq!(intent["schema_version"], 1);
        assert_eq!(
            STANDARD
                .decode(intent["payload_base64"].as_str().unwrap())
                .unwrap(),
            prepared.payload().as_ref()
        );
        super::super::close(&[&jobs]).await;
    }
}
// template:end outbox:test-jobs-operator-outbox

#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
async fn run_operator(url: &str, args: &[&str], code: i32) -> Value {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    // Use the existing fixture's shipped run(args, register) path. A
    // one-connection pool and absent broker configuration cannot start
    // an ordinary worker, but suffice for this admitted command mode.
    let stdout = tempfile::NamedTempFile::new().unwrap();
    let stderr = tempfile::NamedTempFile::new().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_jobs-worker-fixture"))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("APP__POSTGRES__ENABLED", "true")
        .env("APP__POSTGRES__DSN", url)
        .env("APP__POSTGRES__MAX_CONNECTIONS", "1")
        .stdout(Stdio::from(stdout.reopen().unwrap()))
        .stderr(Stdio::from(stderr.reopen().unwrap()))
        .spawn()
        .expect("the existing worker fixture starts");
    let deadline = Instant::now() + Duration::from_secs(40);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => tokio::time::sleep(super::POLL).await,
            wait => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("operator failed to exit within its bound: {wait:?}");
            }
        }
    };
    let output = std::fs::read_to_string(stdout.path()).unwrap();
    let diagnostic = std::fs::read_to_string(stderr.path()).unwrap();
    assert_eq!(status.code(), Some(code), "{diagnostic}");
    assert!(!output.contains(SECRET) && !diagnostic.contains(SECRET));
    assert!(!output.contains(url) && !diagnostic.contains(url));
    let receipt: Value = serde_json::from_str(&output).expect("one JSON receipt only");
    assert_eq!(receipt["schema_version"], 1);
    assert_eq!(receipt["action"], args[0]);
    receipt
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn worker_operator_commands_commit_with_postgres_only_and_emit_safe_receipts(pool: PgPool) {
    let target = failed(&pool, Uuid::new_v4(), SECRET).await;
    let discard_target = failed(&pool, Uuid::new_v4(), "discard").await;
    let url = url_for(&pool, DATABASE_URL).await;
    let id = target.id().to_string();
    let version = target.version().to_string();
    let discard_id = discard_target.id().to_string();
    let discard_version = discard_target.version().to_string();
    let cases: &[(&[&str], i32, &str)] = &[
        (&["inspect", &id], 0, "found"),
        (&["failed"], 0, "ok"),
        (&["unhandled", "--handled-kinds", ""], 0, "ok"),
        (
            &["unhandled", "--handled-kinds", "zebra,alpha,zebra"],
            0,
            "ok",
        ),
        (
            &["redrive", &id, "--kind", KIND, "--version", &version],
            0,
            "redriven",
        ),
        (
            &["redrive", &id, "--kind", KIND, "--version", &version],
            1,
            "stale",
        ),
        (
            &[
                "discard",
                &discard_id,
                "--kind",
                KIND,
                "--version",
                &discard_version,
            ],
            0,
            "discarded",
        ),
        (&["inspect", &discard_id], 0, "missing"),
        (
            &[
                "discard",
                &discard_id,
                "--kind",
                KIND,
                "--version",
                &discard_version,
            ],
            1,
            "missing",
        ),
    ];
    for (args, code, outcome) in cases {
        let receipt = run_operator(url.as_str(), args, *code).await;
        assert_eq!(receipt["outcome"], *outcome);
        if args[0] == "unhandled" {
            let expected = if args[2].is_empty() {
                json!([])
            } else {
                json!(["alpha", "zebra"])
            };
            assert_eq!(receipt["handled_kinds"], expected);
        } else {
            assert!(receipt.get("handled_kinds").is_none());
        }
        if *outcome == "redriven" {
            let row = stored(&pool, &target).await;
            assert_eq!(row["state"], "pending");
            assert_eq!(receipt["expected_version"], version);
            assert_eq!(receipt["new_version"], row["claim_generation"].to_string());
        }
        if *outcome == "found" {
            assert_eq!(receipt["item"]["version"], version);
            assert_eq!(receipt["item"]["id"], id);
            assert_eq!(receipt["item"].as_object().unwrap().len(), 11);
        }
        if *outcome == "discarded" {
            let remaining: i64 =
                sqlx::query_scalar("SELECT count(*) FROM background_jobs WHERE id::text = $1")
                    .bind(&discard_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(remaining, 0);
        }
    }
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn worker_operator_refuses_read_only_sessions_for_every_command(pool: PgPool) {
    let target = failed(&pool, Uuid::new_v4(), "read-only-admission").await;
    let before = stored(&pool, &target).await;
    let url = url_for(&pool, DATABASE_URL).await;
    let id = target.id().to_string();
    let version = target.version().to_string();
    // Existing sessions remain writable; each operator opens a new session
    // against this database's read-only default, as a replica would expose.
    sqlx::query(
        "DO $$ BEGIN EXECUTE format(\
         'ALTER DATABASE %I SET default_transaction_read_only = on', current_database()); END $$",
    )
    .execute(&pool)
    .await
    .unwrap();
    let cases: &[&[&str]] = &[
        &["inspect", &id],
        &["failed"],
        &["unhandled", "--handled-kinds", "zebra,alpha,zebra"],
        &["redrive", &id, "--kind", KIND, "--version", &version],
        &["discard", &id, "--kind", KIND, "--version", &version],
    ];
    for args in cases {
        let receipt = run_operator(url.as_str(), args, 1).await;
        assert_eq!(receipt["cause"], "session_admission");
        assert_eq!(
            receipt["outcome"],
            if matches!(args[0], "redrive" | "discard") {
                "failed"
            } else {
                "unavailable"
            }
        );
        assert!(receipt.get("items").is_none());
        assert!(receipt.get("next_cursor").is_none());
        assert!(receipt.get("complete").is_none());
        if args[0] == "unhandled" {
            assert_eq!(receipt["handled_kinds"], json!(["alpha", "zebra"]));
        }
    }
    assert_eq!(stored(&pool, &target).await, before);
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn recovery_initial_lock_has_local_statement_budget_and_preserves_failed_custody(
    pool: PgPool,
) {
    let jobs = super::template_pool(&dsn_for(&pool).await, 1).await;
    for discard in [false, true] {
        let target = failed(&pool, Uuid::new_v4(), "blocked-recovery").await;
        let before = stored(&pool, &target).await;
        #[expect(
            clippy::disallowed_methods,
            reason = "a raw independent transaction holds the row lock while the adapter is exercised"
        )]
        let mut holder = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM background_jobs WHERE id::text = $1 FOR UPDATE")
            .bind(target.id().to_string())
            .fetch_one(&mut *holder)
            .await
            .unwrap();
        // The admitted pool's ordinary statement budget is eight seconds.
        // A local two-second timeout must fail at the first lock while it is
        // still held, rather than escape via the caller's operation deadline.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            in_tx(&jobs, async |tx| {
                if discard {
                    operator::discard(tx, &target).await.map(|_| ())
                } else {
                    operator::redrive(tx, &target).await.map(|_| ())
                }
            }),
        )
        .await;
        holder.rollback().await.unwrap();
        let error = result
            .expect("the local statement budget expires before the caller deadline")
            .unwrap_err();
        assert_eq!(error.sqlstate().as_deref(), Some("57014"));
        assert_eq!(stored(&pool, &target).await, before);
        let budget: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(&jobs)
            .await
            .unwrap();
        assert_eq!(
            budget, "8s",
            "the failed transaction restores the session budget"
        );
    }
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn concurrent_recovery_has_one_winner_after_the_row_lock(pool: PgPool) {
    let jobs = open(&pool).await;
    for discard in [false, true] {
        let target = failed(&pool, Uuid::new_v4(), "arbitration").await;
        let (ready, held) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let winner = tokio::spawn({
            let jobs = jobs.clone();
            let target = target.clone();
            async move {
                in_tx(&jobs, async |tx| {
                    let receipt = operator::redrive(tx, &target).await?;
                    ready.send(()).unwrap();
                    super::bounded("release the winning transaction", released)
                        .await
                        .unwrap();
                    Ok::<_, OperatorError>(receipt)
                })
                .await
            }
        });
        super::bounded("the provisional redrive", held)
            .await
            .unwrap();
        let loser = tokio::spawn({
            let jobs = jobs.clone();
            let target = target.clone();
            async move {
                in_tx(&jobs, async |tx| {
                    if discard {
                        operator::discard(tx, &target).await.map(|_| ())
                    } else {
                        operator::redrive(tx, &target).await.map(|_| ())
                    }
                })
                .await
            }
        });
        let error = super::join_after_lock_wait(
            &pool,
            async {
                release.send(()).unwrap();
                super::bounded("the winner commits", winner)
                    .await
                    .unwrap()
                    .unwrap();
            },
            loser,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, OperatorError::Stale));
        let row = stored(&pool, &target).await;
        assert_eq!(row["state"], "pending");
        assert_eq!(row["recovery_history"].as_array().unwrap().len(), 1);
        // Free this iteration's live key without changing the next case.
        sqlx::query("DELETE FROM background_jobs WHERE id::text = $1")
            .bind(target.id().to_string())
            .execute(&pool)
            .await
            .unwrap();
    }
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn concurrent_enqueue_owns_the_live_key_and_rolls_back_the_entire_redrive(pool: PgPool) {
    let jobs = open(&pool).await;
    let target = failed(&pool, Uuid::new_v4(), SECRET).await;
    let before = stored(&pool, &target).await;
    let (ready, held) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let enqueue_task = tokio::spawn({
        let jobs = jobs.clone();
        async move {
            in_tx(&jobs, async |tx| {
                let result = enqueue(
                    tx,
                    &Note {
                        text: "new producer".into(),
                    },
                    EnqueueOptions {
                        unique_key: Some(SECRET),
                        ..EnqueueOptions::default()
                    },
                )
                .await
                .expect("the producer inserts a new live row");
                assert!(matches!(result, Enqueued::Created(_)));
                ready.send(()).unwrap();
                super::bounded("release enqueue", released).await.unwrap();
                Ok::<_, OperatorError>(())
            })
            .await
        }
    });
    super::bounded("enqueue holds the key", held).await.unwrap();
    let redrive = tokio::spawn({
        let jobs = jobs.clone();
        let target = target.clone();
        async move { in_tx(&jobs, async |tx| operator::redrive(tx, &target).await).await }
    });
    let error = super::join_after_lock_wait(
        &pool,
        async {
            release.send(()).unwrap();
            super::bounded("enqueue commits", enqueue_task)
                .await
                .unwrap()
                .unwrap();
        },
        redrive,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, OperatorError::Conflict));
    assert!(!format!("{error:?}").contains(SECRET));
    assert_eq!(stored(&pool, &target).await, before);
    assert_eq!(super::job_count(&pool).await, 2);
    super::close(&[&jobs]).await;
}

async fn proxied_pool(pool: &PgPool) -> (CommitProxy, PgPool) {
    let dsn = dsn_for(pool).await;
    assert_eq!(dsn.ssl_mode_name(), "disable");
    let host = dsn.host().trim_start_matches('[').trim_end_matches(']');
    let server = tokio::net::lookup_host((host, dsn.port()))
        .await
        .unwrap()
        .next()
        .unwrap();
    let proxy = CommitProxy::start(server).await;
    let mut url = url_for(pool, DATABASE_URL).await;
    url.set_ip_host(proxy.address().ip()).unwrap();
    url.set_port(Some(proxy.address().port())).unwrap();
    let dsn = Dsn::admit(url.as_str()).unwrap();
    let jobs = super::template_pool(&dsn, 1).await;
    (proxy, jobs)
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn recovery_results_remain_provisional_until_commit_is_acknowledged(pool: PgPool) {
    let jobs = super::template_pool(&dsn_for(&pool).await, 1).await;
    let target = failed(&pool, Uuid::new_v4(), "rollback").await;
    let before = stored(&pool, &target).await;
    let result = in_tx(&jobs, async |tx| {
        operator::redrive(tx, &target).await?;
        Err::<(), _>(OperatorError::Stale)
    })
    .await;
    assert!(matches!(result, Err(OperatorError::Stale)));
    // Acquiring from the same pool flushes the queued rollback before readback.
    assert_eq!(stored(&jobs, &target).await, before);
    for fault in [Fault::DropBeforeForward, Fault::ForwardThenDrop] {
        let target = failed(&pool, Uuid::new_v4(), "uncertain").await;
        let before = stored(&pool, &target).await;
        let (proxy, proxied) = proxied_pool(&pool).await;
        proxy.arm(fault);
        let error = super::bounded(
            "the uncertain redrive",
            in_tx(&proxied, async |tx| operator::redrive(tx, &target).await),
        )
        .await
        .unwrap_err();
        assert!(error.is_commit_unknown());
        assert_eq!(proxy.fired(), Some(fault));
        super::close(&[&proxied]).await;
        proxy.shutdown().await;
        let row = stored(&pool, &target).await;
        match fault {
            Fault::DropBeforeForward => assert_eq!(row, before),
            Fault::ForwardThenSilence => panic!("this case tests connection-close faults only"),
            Fault::ForwardThenDrop | Fault::ForwardThenCorruptReady => {
                assert_eq!(row["state"], "pending");
                assert_eq!(row["recovery_history"].as_array().unwrap().len(), 1);
                assert_ne!(row["claim_generation"], before["claim_generation"]);
            }
        }
    }
    super::close(&[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn inspection_advances_over_nonmatches_and_returns_only_safe_lossless_fields(pool: PgPool) {
    let jobs = open(&pool).await;
    for n in 1..=4 {
        failed(&pool, Uuid::from_u128(n), &format!("{SECRET}-{n}")).await;
    }
    sqlx::query(
        "UPDATE background_jobs SET state = 'pending', failure_reason = NULL,
                 finished_at = NULL WHERE id <> $1",
    )
    .bind(Uuid::from_u128(3))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE background_jobs SET kind = 'test.unknown' WHERE id = $1")
        .bind(Uuid::from_u128(4))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE background_jobs SET claim_generation = 9007199254740993 WHERE id = $1")
        .bind(Uuid::from_u128(3))
        .execute(&pool)
        .await
        .unwrap();
    for unhandled in [false, true] {
        let request = if unhandled {
            Inspection::unhandled(KIND, None, 2)
        } else {
            Inspection::failed(None, 2)
        }
        .unwrap();
        let InspectionResult::Page {
            scanned,
            items,
            complete,
            next_cursor,
            ..
        } = inspect(&jobs, &request).await
        else {
            panic!("a page")
        };
        assert_eq!(scanned, 2);
        assert!(items.is_empty());
        assert!(!complete);
        assert_eq!(
            next_cursor.as_deref(),
            Some("v1:00000000-0000-0000-0000-000000000002")
        );
        let request = if unhandled {
            Inspection::unhandled(KIND, next_cursor.as_deref(), 2)
        } else {
            Inspection::failed(next_cursor.as_deref(), 2)
        }
        .unwrap();
        let result = inspect(&jobs, &request).await;
        assert!(!format!("{result:?}").contains(SECRET));
        let InspectionResult::Page {
            scanned,
            items,
            complete,
            next_cursor,
            ..
        } = result
        else {
            panic!("a page")
        };
        assert_eq!(scanned, 2);
        assert!(!complete);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].id.to_string(),
            Uuid::from_u128(if unhandled { 4 } else { 3 }).to_string()
        );
        if !unhandled {
            assert_eq!(items[0].version, "9007199254740993");
        }
        let request = Inspection::failed(next_cursor.as_deref(), 2).unwrap();
        let InspectionResult::Page {
            scanned,
            items,
            complete,
            next_cursor,
            ..
        } = inspect(&jobs, &request).await
        else {
            panic!("a page")
        };
        assert_eq!(scanned, 0);
        assert!(items.is_empty() && complete && next_cursor.is_none());
    }
    let request = Inspection::one(&Uuid::from_u128(3).to_string()).unwrap();
    let result = in_tx_with(
        &jobs,
        TxOptions {
            isolation: Isolation::ReadCommitted,
            read_only: true,
        },
        async |tx| {
            let result = operator::inspect(tx, &request).await?;
            let budget: String = sqlx::query_scalar("SHOW statement_timeout")
                .fetch_one(&mut *tx)
                .await?;
            assert_eq!(budget, "2s");
            Ok::<_, OperatorError>(result)
        },
    )
    .await
    .unwrap();
    assert!(!format!("{result:?}").contains(SECRET));
    let InspectionResult::One {
        observed_at,
        item: Some(item),
    } = result
    else {
        panic!("one job")
    };
    assert_eq!(item.state, JobState::Failed);
    assert_eq!(item.version, "9007199254740993");
    assert_eq!(item.recovery_count, 0);
    for timestamp in [
        &observed_at,
        &item.created_at,
        &item.not_before,
        item.finished_at.as_ref().unwrap(),
    ] {
        assert!(timestamp.ends_with('Z'));
        assert_eq!(timestamp.len(), 27);
        time::OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339)
            .unwrap();
    }
    let missing = Inspection::one(&Uuid::from_u128(5).to_string()).unwrap();
    assert!(matches!(
        inspect(&jobs, &missing).await,
        InspectionResult::One { item: None, .. }
    ));
    super::close(&[&jobs]).await;
}
