use base64::{Engine as _, engine::general_purpose::STANDARD};
use infra_jobs::operator::{self, OperatorError, RecoveryTarget};
use infra_messaging::Messaging;
use infra_messaging::outbox::{OutboxEnqueueError, OutboxEnqueued, kinds};
use infra_postgres::{PgPool, in_tx};
use integration_tests::{close_pools, dsn_for, poll_until};
use migrate::{MIGRATOR, RunError, RunOptions};
use serde_json::{Value, json};
use sqlx::migrate::MigrateError;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
    APP, Fixture, OUTBOX_KIND, POLL, Step, WAIT, close, event, event_key, finish,
    messaging_options, outbox_count, prepared, start, template_pool,
};

pub(super) const HISTORICAL_SCHEMA: i64 = 20_261_117_120_000;
const RECEIPT_SCHEMA: i64 = 20_261_118_120_010;
const RECEIPT_BACKFILL: i64 = 20_261_118_120_011;
const SUBJECT: &str = "test.outbox.upgrade";
const BODY: &[u8] = br#"{"value":"legacy\nbytes"}"#;
const LOGICAL_ID: &str = "logical-id";
const LOGICAL_KEY: &str = "event-4f63c63f6201a9f96cf5efafede950d7bb7d4c83edfff141074bfbf9622e1d03";

struct LegacyIntent {
    key: String,
    wire: String,
}

fn historical_intent(subject: &str, id: &str, payload: &[u8]) -> LegacyIntent {
    historical_encoded_intent(subject, id, &STANDARD.encode(payload))
}

fn historical_encoded_intent(subject: &str, id: &str, encoded: &str) -> LegacyIntent {
    LegacyIntent {
        key: event_key(id),
        wire: serde_json::to_string_pretty(&json!({
            "version": 1,
            "subject": subject,
            "message_id": id,
            "publication_id": id,
            "event_type": "test.outbox.created",
            "schema_version": 1,
            "occurred_at_unix_seconds": 1_700_000_000,
            "occurred_at_nanosecond": 0,
            "payload_base64": encoded,
        }))
        .unwrap(),
    }
}

pub(super) async fn stage_encoded(pool: &PgPool, subject: &str, id: &str, encoded: &str) {
    let intent = historical_encoded_intent(subject, id, encoded);
    historical_job(pool, &intent, Storage::Text, LegacyState::Pending, false).await;
}

#[derive(Clone, Copy)]
enum Storage {
    Jsonb,
    Text,
}

#[derive(Clone, Copy)]
enum LegacyState<'a> {
    Pending,
    Completed,
    Failed {
        reason: &'a str,
        summary: &'a str,
        attempts: i16,
        last_error: Option<&'a str>,
    },
}

const fn exhausted<'a>(summary: &'a str, last_error: Option<&'a str>) -> LegacyState<'a> {
    LegacyState::Failed {
        reason: "exhausted",
        summary,
        attempts: 25,
        last_error,
    }
}

async fn historical_job(
    pool: &PgPool,
    intent: &LegacyIntent,
    storage: Storage,
    state: LegacyState<'_>,
    oldest: bool,
) -> String {
    let (state, attempts, reason, summary, last_error) = match state {
        LegacyState::Pending => ("pending", 0, None, None, None),
        LegacyState::Completed => ("completed", 1, None, None, None),
        LegacyState::Failed {
            reason,
            summary,
            attempts,
            last_error,
        } => ("failed", attempts, Some(reason), Some(summary), last_error),
    };
    sqlx::query_scalar(
        "INSERT INTO background_jobs (
            id, kind, payload, payload_text, unique_key, created_at, not_before, state,
            attempts, claim_generation, attempted_by, finished_at, failure_reason, error_summary, errors
         ) VALUES (
            gen_random_uuid(), $1,
            CASE WHEN $4 THEN NULL ELSE $2::text::jsonb END,
            CASE WHEN $4 THEN $2::text ELSE NULL END, $3,
            CASE WHEN $9 THEN statement_timestamp() - INTERVAL '1 hour' ELSE statement_timestamp() END,
            CASE WHEN $9 THEN statement_timestamp() - INTERVAL '1 hour' ELSE statement_timestamp() END,
            $5, $6,
            CASE WHEN $5 = 'pending' THEN 0 ELSE nextval('background_jobs_claim_generation') END,
            CASE WHEN $5 = 'pending' THEN NULL ELSE gen_random_uuid() END,
            CASE WHEN $5 = 'pending' THEN NULL ELSE statement_timestamp() END, $7, $8,
            CASE WHEN $10::text IS NULL THEN '[]'::jsonb ELSE jsonb_build_array(jsonb_build_object(
                'attempt', $6::smallint, 'at', statement_timestamp(), 'error', $10::text)) END
         ) RETURNING id::text",
    )
    .bind(OUTBOX_KIND)
    .bind(&intent.wire)
    .bind(&intent.key)
    .bind(matches!(storage, Storage::Text))
    .bind(state)
    .bind(attempts)
    .bind(reason)
    .bind(summary)
    .bind(oldest)
    .bind(last_error)
    .fetch_one(pool)
    .await
    .expect("insert pre-upgrade outbox job")
}

async fn stored_job(pool: &PgPool, id: &str) -> Value {
    sqlx::query_scalar("SELECT to_jsonb(job) FROM background_jobs AS job WHERE id = $1::text::uuid")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("the historical job remains present")
}

async fn historical_target(pool: &PgPool, id: &str) -> RecoveryTarget {
    let version: String = sqlx::query_scalar(
        "SELECT claim_generation::text FROM background_jobs WHERE id = $1::text::uuid",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("historical operator version");
    RecoveryTarget::try_new(id, OUTBOX_KIND, &version).unwrap()
}

#[sqlx::test(migrations = false)]
async fn upgrade_reconciles_equivalent_history_and_redrives_the_original_job(pool: PgPool) {
    MIGRATOR.run_to(HISTORICAL_SCHEMA, &pool).await.unwrap();
    let fixture = Fixture::create().await;
    let intent = historical_intent(&fixture.subject, LOGICAL_ID, BODY);
    let failed = LegacyState::Failed {
        reason: "permanent",
        summary: "outbox_envelope_invalid",
        attempts: 1,
        last_error: Some("outbox_envelope_invalid"),
    };
    let first = historical_job(&pool, &intent, Storage::Text, LegacyState::Completed, true).await;
    let duplicate = historical_job(&pool, &intent, Storage::Jsonb, failed, false).await;
    let alias_target = historical_target(&pool, &duplicate).await;
    let legacy_text = stored_job(&pool, &first).await["payload_text"].clone();

    MIGRATOR.run(&pool).await.unwrap();
    let receipts: Vec<(String, String, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT event_key, job_id::text, aggregate_key, aggregate_version FROM outbox_event_receipts",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        receipts,
        [(LOGICAL_KEY.to_owned(), first.clone(), None, None)]
    );
    assert_eq!(outbox_count(&pool).await, 2);
    assert_eq!(stored_job(&pool, &first).await["payload_text"], legacy_text);

    let jobs = template_pool(&pool, 3).await;
    close_pools(&[&pool]).await;
    let replay = fixture
        .publication
        .prepare(&event(LOGICAL_ID, "legacy\nbytes"))
        .unwrap();
    let changed = fixture
        .publication
        .prepare(&event(LOGICAL_ID, "changed"))
        .unwrap();
    in_tx(&jobs, async |tx| -> Result<(), Step> {
        assert_eq!(replay.enqueue(tx).await?, OutboxEnqueued::Duplicate);
        std::assert_matches!(
            changed.enqueue(tx).await,
            Err(OutboxEnqueueError::EventIdConflict)
        );
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(outbox_count(&jobs).await, 2);
    std::assert_matches!(
        in_tx(&jobs, async |tx| operator::discard(tx, &alias_target).await).await,
        Err(OperatorError::Conflict)
    );
    in_tx(&jobs, async |tx| operator::redrive(tx, &alias_target).await)
        .await
        .expect("matching historical job can be redriven");

    let messaging = Box::pin(Messaging::connect(
        messaging_options(&fixture),
        Instant::now() + WAIT,
        CancellationToken::new(),
    ))
    .await
    .unwrap();
    let registry = kinds(messaging.outbox_producer().unwrap()).unwrap();
    let run = start(&jobs, registry).await;
    poll_until(
        "redriven historical job completes under the original UUID",
        WAIT,
        POLL,
        async || (stored_job(&jobs, &duplicate).await["state"] == "completed").then_some(()),
    )
    .await;
    assert_eq!(fixture.published().await, 1);
    let message = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .unwrap()
        .get_raw_message(1)
        .await
        .unwrap();
    for header in ["Message-Id", "Nats-Msg-Id"] {
        assert_eq!(message.headers.get(header).unwrap().as_str(), LOGICAL_ID);
    }
    assert_eq!(message.payload.as_ref(), BODY);
    let independent = prepared(&fixture, LOGICAL_ID, "legacy\nbytes");
    in_tx(&jobs, async |tx| -> Result<(), Step> {
        assert_eq!(independent.enqueue(tx).await?, OutboxEnqueued::Created);
        assert_eq!(replay.enqueue(tx).await?, OutboxEnqueued::Duplicate);
        Ok(())
    })
    .await
    .unwrap();
    poll_until(
        "new publisher with identical local intent also delivers",
        WAIT,
        POLL,
        async || (fixture.published().await == 2).then_some(()),
    )
    .await;
    let fresh = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .unwrap()
        .get_raw_message(2)
        .await
        .unwrap();
    for header in ["Message-Id", "Nats-Msg-Id"] {
        assert_ne!(fresh.headers.get(header).unwrap().as_str(), LOGICAL_ID);
        assert_eq!(
            fresh.headers.get(header).unwrap().as_str(),
            independent.message_id()
        );
    }
    assert_eq!(fresh.payload.as_ref(), BODY);
    finish(run, &[&jobs]).await;
    close(messaging).await;
    fixture.cleanup().await;
}

#[sqlx::test(migrations = false)]
async fn upgrade_refuses_conflicting_history_without_changing_jobs_or_schema(pool: PgPool) {
    MIGRATOR.run_to(HISTORICAL_SCHEMA, &pool).await.unwrap();
    let id = "conflicting-history";
    let first = historical_intent(SUBJECT, id, BODY);
    let conflicting = historical_intent(SUBJECT, id, br#"{ "value": "legacy\nbytes" }"#);
    let first = historical_job(&pool, &first, Storage::Jsonb, LegacyState::Completed, true).await;
    let conflicting = historical_job(
        &pool,
        &conflicting,
        Storage::Text,
        LegacyState::Pending,
        false,
    )
    .await;
    MIGRATOR.run_to(RECEIPT_SCHEMA, &pool).await.unwrap();
    let before = [
        stored_job(&pool, &first).await,
        stored_job(&pool, &conflicting).await,
    ];

    let dsn = dsn_for(&pool).await;
    let error = migrate::run(&MIGRATOR, &RunOptions::defaults(&dsn, APP, WAIT))
        .await
        .expect_err("different immutable bytes under the same event ID prevent upgrade");
    let RunError::Migrate(MigrateError::ExecuteMigration(error, version)) = error.error else {
        panic!("unexpected migration failure: {error}");
    };
    assert_eq!(version, RECEIPT_BACKFILL);
    let database = error
        .as_database_error()
        .expect("database refused conflicting history");
    assert_eq!(database.code().as_deref(), Some("23505"));
    assert_eq!(database.constraint(), Some("outbox_event_receipts_pkey"));
    let receipts_untouched: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM outbox_event_receipts)
            AND NOT EXISTS (SELECT 1 FROM outbox_legacy_job_aliases)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(receipts_untouched);
    let latest: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(latest, RECEIPT_SCHEMA);
    assert_eq!(
        [
            stored_job(&pool, &first).await,
            stored_job(&pool, &conflicting).await
        ],
        before
    );
    close_pools(&[&pool]).await;
}

#[sqlx::test(migrations = false)]
async fn upgrade_requeues_only_verified_transient_exhaustion_without_displacing_live_work(
    pool: PgPool,
) {
    const TRANSIENT: &str = "broker_refused";
    const AMBIGUOUS: &str = "publication_ack_unknown";
    MIGRATOR.run_to(HISTORICAL_SCHEMA, &pool).await.unwrap();
    let prior_generation: String =
        sqlx::query_scalar("SELECT nextval('background_jobs_claim_generation')::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    let eligible = historical_job(
        &pool,
        &historical_intent(SUBJECT, "transient", BODY),
        Storage::Jsonb,
        exhausted(TRANSIENT, Some(TRANSIENT)),
        true,
    )
    .await;
    let previous = json!({
        "version": prior_generation, "attempts": 2, "failure_reason": "permanent", "attempted_by": null,
        "finished_at": "2026-11-01T00:00:00+00:00", "error_summary": "outbox_envelope_invalid",
        "errors": [{ "attempt": 2, "at": "2026-11-01T00:00:00+00:00", "error": "outbox_envelope_invalid" }],
        "redriven_at": "2026-11-01T01:00:00+00:00",
    });
    sqlx::query("UPDATE background_jobs SET recovery_history = $2 WHERE id = $1::text::uuid")
        .bind(&eligible)
        .bind(json!([previous]))
        .execute(&pool)
        .await
        .unwrap();
    let old_target = historical_target(&pool, &eligible).await;
    let before = stored_job(&pool, &eligible).await;

    let mut untouched = Vec::new();
    for (id, state) in [
        ("ambiguous", exhausted(AMBIGUOUS, Some(AMBIGUOUS))),
        ("unknown-last-error", exhausted(TRANSIENT, None)),
        (
            "conflicting-last-error",
            exhausted(TRANSIENT, Some(AMBIGUOUS)),
        ),
        (
            "permanent",
            LegacyState::Failed {
                reason: "permanent",
                summary: TRANSIENT,
                attempts: 25,
                last_error: Some(TRANSIENT),
            },
        ),
        ("live-peer", exhausted(TRANSIENT, Some(TRANSIENT))),
        ("digest-mismatch", exhausted(TRANSIENT, Some(TRANSIENT))),
        ("malformed-outer", exhausted(TRANSIENT, Some(TRANSIENT))),
    ] {
        let intent = historical_intent(SUBJECT, id, BODY);
        let failed = historical_job(&pool, &intent, Storage::Text, state, true).await;
        if id == "live-peer" {
            untouched.push(
                historical_job(&pool, &intent, Storage::Text, LegacyState::Pending, false).await,
            );
        }
        untouched.push(failed);
    }

    MIGRATOR.run_to(RECEIPT_BACKFILL, &pool).await.unwrap();
    let altered = sqlx::query(
        "UPDATE background_jobs
         SET payload_text = jsonb_set(payload_text::jsonb, '{event_type}', '\"different.event\"')::text
         WHERE unique_key = $1",
    )
    .bind(event_key("digest-mismatch"))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(altered.rows_affected(), 1);
    sqlx::query("UPDATE background_jobs SET payload_text = '{' WHERE unique_key = $1")
        .bind(event_key("malformed-outer"))
        .execute(&pool)
        .await
        .unwrap();
    let mut parked = Vec::new();
    for id in untouched {
        let snapshot = stored_job(&pool, &id).await;
        parked.push((id, snapshot));
    }

    MIGRATOR.run(&pool).await.unwrap();
    for (id, snapshot) in parked {
        assert_eq!(stored_job(&pool, &id).await, snapshot, "legacy job {id}");
    }
    let after = stored_job(&pool, &eligible).await;
    assert_eq!(after["state"], "pending");
    assert_eq!(after["attempts"], before["attempts"]);
    assert_eq!(after["payload"], before["payload"]);
    assert_eq!(after["errors"], before["errors"]);
    assert_eq!(after["error_summary"], before["error_summary"]);
    assert!(after["attempted_by"].is_null());
    assert!(after["failure_reason"].is_null());
    assert!(after["finished_at"].is_null());
    assert!(after["claim_generation"].as_i64().unwrap() > old_target.version());
    let history = after["recovery_history"].as_array().unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0], previous);
    assert_eq!(history[1]["version"], old_target.version().to_string());
    for field in [
        "attempts",
        "attempted_by",
        "failure_reason",
        "finished_at",
        "error_summary",
        "errors",
    ] {
        assert_eq!(history[1][field], before[field], "archived {field}");
    }
    let stale = in_tx(&pool, async |tx| operator::redrive(tx, &old_target).await).await;
    std::assert_matches!(stale, Err(OperatorError::Stale));
    close_pools(&[&pool]).await;
}

#[sqlx::test(migrations = false)]
#[warn(unused_results)]
async fn upgrade_bounds_only_pending_outbox_retries_with_legacy_schedule_evidence(pool: PgPool) {
    MIGRATOR.run_to(HISTORICAL_SCHEMA, &pool).await.unwrap();
    let mut staged_jobs = Vec::new();
    for (scenario, state, due_in, attempt) in [
        ("verified-retry", "pending", 259_200_i32, 24_i16),
        ("ambiguous-retry", "pending", 259_200, 24),
        ("within-budget", "pending", 30, 24),
        ("already-due", "pending", -1, 24),
        ("delayed-admission", "pending", 259_200, 0),
        ("snoozed-or-rescheduled", "pending", 259_200, 24),
        ("mismatched-history", "pending", 259_200, 24),
        ("bad-history-date", "pending", 259_200, 24),
        ("conflicting-receipt", "pending", 259_200, 24),
        ("ordinary-job", "pending", 259_200, 24),
        ("running-lease", "running", 259_200, 24),
        ("completed", "completed", 259_200, 24),
        ("failed", "failed", 259_200, 24),
    ] {
        let intent = historical_intent(SUBJECT, scenario, BODY);
        let id = historical_job(&pool, &intent, Storage::Text, LegacyState::Pending, true).await;
        let staged = sqlx::query(
            "UPDATE background_jobs SET
                kind = CASE WHEN $5 = 'ordinary-job' THEN 'ordinary_job' ELSE kind END,
                state = $2, attempts = $4, created_at = statement_timestamp() - interval '5 days',
                not_before = statement_timestamp() + $3::integer * interval '1 second',
                claim_generation = CASE WHEN $4 > 0 THEN nextval('background_jobs_claim_generation') ELSE 0 END,
                attempted_by = CASE WHEN $4 > 0 THEN gen_random_uuid() END,
                claim_expires_at = CASE WHEN $2 = 'running' THEN statement_timestamp() + interval '30 seconds' END,
                finished_at = CASE WHEN $2 IN ('completed', 'failed') THEN statement_timestamp() END,
                failure_reason = CASE WHEN $2 = 'failed' THEN 'permanent' END,
                error_summary = $6,
                errors = CASE WHEN $4 = 0 THEN '[]'::jsonb ELSE jsonb_build_array(jsonb_build_object(
                    'attempt', CASE WHEN $5 = 'mismatched-history' THEN $4 - 1 ELSE $4 END,
                    'at', CASE
                        WHEN $5 = 'bad-history-date' THEN to_jsonb('2026-77-41T00:00:00+00:00'::text)
                        WHEN $5 = 'snoozed-or-rescheduled' THEN to_jsonb(statement_timestamp())
                        ELSE to_jsonb(statement_timestamp() + ($3 - 331776) * interval '1 second')
                    END,
                    'error', $6
                )) END
             WHERE id = $1::text::uuid",
        )
        .bind(&id)
        .bind(state)
        .bind(due_in)
        .bind(attempt)
        .bind(scenario)
        .bind(match scenario {
            "delayed-admission" => None,
            "ambiguous-retry" => Some("publication_ack_unknown"),
            _ => Some("broker_refused"),
        })
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(staged.rows_affected(), 1, "stage legacy job {scenario}");
        staged_jobs.push((scenario, id));
    }
    MIGRATOR.run_to(RECEIPT_BACKFILL, &pool).await.unwrap();
    let mut snapshots = Vec::new();
    for (scenario, id) in staged_jobs {
        if scenario == "conflicting-receipt" {
            let staged = sqlx::query(
                "UPDATE background_jobs SET payload_text = '{' WHERE id = $1::text::uuid",
            )
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
            assert_eq!(staged.rows_affected(), 1, "stage conflicting receipt");
        }
        let before = stored_job(&pool, &id).await;
        snapshots.push((scenario, id, before));
    }
    let upgrade_started: String = sqlx::query_scalar("SELECT statement_timestamp()::text")
        .fetch_one(&pool)
        .await
        .unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    for (scenario, id, mut before) in snapshots {
        let mut after = stored_job(&pool, &id).await;
        if matches!(scenario, "verified-retry" | "ambiguous-retry") {
            let in_window: bool = sqlx::query_scalar(
                "SELECT not_before BETWEEN $2::text::timestamptz + interval '48.6 seconds'
                                       AND statement_timestamp() + interval '59.4 seconds'
                 FROM background_jobs WHERE id = $1::text::uuid",
            )
            .bind(&id)
            .bind(&upgrade_started)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert!(
                in_window,
                "legacy retry {scenario} must be due within the cap"
            );
            assert_ne!(
                before
                    .as_object_mut()
                    .unwrap()
                    .remove("not_before")
                    .unwrap(),
                after.as_object_mut().unwrap().remove("not_before").unwrap(),
                "reschedule legacy job {scenario}"
            );
        }
        assert_eq!(after, before, "legacy job {scenario}");
    }
    close_pools(&[&pool]).await;
}
