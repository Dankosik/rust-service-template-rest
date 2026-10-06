//! One opt-in native recovery proof. Ordinary integration discovery compiles
//! this carrier but cannot silently satisfy it without its historical actors.
//!
//! Primary oracle: exact durable identities from actual old/new processes and
//! native archives. Existing jobs/outbox tests own individual adapter branches;
//! they cannot detect a missing archive, wrong executable or restore fence.

#![cfg(feature = "integration")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use async_nats::jetstream::{self, consumer};
use infra_postgres::PgPool;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::time::{Instant, timeout};

const WAIT: Duration = Duration::from_secs(45);
const OLD: &str = "67be869acea112af271ec8ba621cbc50ae9d36b7";
const NEW: &str = "2cb871895b9edd018205fc98223477e269fce2e9";

struct Endpoint {
    database: String,
    nats: String,
}

struct Harness {
    old: PathBuf,
    new: PathBuf,
    evidence: PathBuf,
    source: Endpoint,
    restored: Endpoint,
    next_process: usize,
}

impl Harness {
    #[allow(
        clippy::disallowed_methods,
        reason = "finite fixture startup verifies its owned local identity files before starting actors"
    )]
    fn from_env() -> Self {
        let harness = Self {
            old: required("LIFECYCLE_OLD_ACTOR").into(),
            new: required("LIFECYCLE_NEW_ACTOR").into(),
            evidence: required("LIFECYCLE_EVIDENCE").into(),
            source: Endpoint {
                database: required("DATABASE_URL"),
                nats: required("NATS_URL"),
            },
            restored: Endpoint {
                database: required("LIFECYCLE_RESTORE_DATABASE_URL"),
                nats: required("LIFECYCLE_RESTORE_NATS_URL"),
            },
            next_process: 0,
        };
        assert_ne!(harness.source.database, harness.restored.database);
        assert_ne!(harness.source.nats, harness.restored.nats);
        let identities: Value =
            serde_json::from_slice(&std::fs::read(harness.evidence.join("actors.json")).unwrap())
                .unwrap();
        for (label, revision, binary) in [("old", OLD, &harness.old), ("new", NEW, &harness.new)] {
            assert_eq!(identities[label]["upstream"], revision);
            assert_eq!(
                identities[label]["executable_sha256"],
                digest(&std::fs::read(binary).unwrap())
            );
        }
        assert_eq!(
            identities["old"]["overlay_sha256"],
            identities["new"]["overlay_sha256"]
        );
        harness
    }

    async fn spawn(
        &mut self,
        old: bool,
        restored: bool,
        args: &[&str],
        custody_required: bool,
    ) -> Result<Actor, &'static str> {
        // Explicit operator launch admission. The old runtime has no custody
        // detector; migration compatibility must never masquerade as one.
        if old && custody_required {
            return Err("old retention owners are forbidden while failure custody is required");
        }
        let endpoint = if restored {
            &self.restored
        } else {
            &self.source
        };
        let executable = if old { &self.old } else { &self.new };
        self.next_process += 1;
        let stem = format!(
            "actor-{:02}-{}-{}",
            self.next_process,
            if old { "old" } else { "new" },
            args[0]
        );
        let stdout = self.evidence.join(format!("{stem}.stdout"));
        let stderr = self.evidence.join(format!("{stem}.stderr"));
        let out = tokio::fs::File::create(&stdout)
            .await
            .unwrap()
            .into_std()
            .await;
        let err = tokio::fs::File::create(&stderr)
            .await
            .unwrap()
            .into_std()
            .await;
        let child = Command::new(executable)
            .args(args)
            .current_dir(
                self.evidence
                    .join("consumers")
                    .join(if old { "old" } else { "new" }),
            )
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("DATABASE_URL", &endpoint.database)
            .env("NATS_URL", &endpoint.nats)
            .env("APP__APP__ENV", "local")
            .env("APP__HTTP__ADDR", "127.0.0.1:0")
            .env("APP__OBSERVABILITY__METRICS__ADDR", "127.0.0.1:0")
            .env("APP__LOG__FORMAT", "json")
            .env("APP__POSTGRES__ENABLED", "true")
            .env("APP__POSTGRES__DSN", &endpoint.database)
            .env("APP__POSTGRES__MAX_CONNECTIONS", "6")
            .env("APP__MESSAGING__URLS", &endpoint.nats)
            .env("APP__MESSAGING__SOURCE_STREAM", "LIFECYCLE_SOURCE")
            .env("APP__MESSAGING__MAX_PAYLOAD_BYTES", "1 KiB")
            .env("APP__MESSAGING__ALLOW_PLAINTEXT", "true")
            .env("APP__MESSAGING__ALLOW_UNAUTHENTICATED", "true")
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .stdin(Stdio::null())
            .spawn()
            .expect("actual historical actor process");
        write_json(
            &self.evidence.join(format!("{stem}.json")),
            &json!({
            "pid": child.id(), "executable": executable, "revision": if old { OLD } else { NEW },
            "action": args, "restored": restored, "started_at_unix_ms": wall_clock_ms(),
            }),
        );
        Ok(Actor {
            child,
            stdout,
            stderr,
            exited: false,
        })
    }

    async fn action(&mut self, old: bool, restored: bool, args: &[&str]) {
        let mut actor = self.spawn(old, restored, args, false).await.unwrap();
        assert_eq!(actor.wait().await, Some(0), "{}", actor.diagnostics());
    }

    async fn worker(&mut self, old: bool, restored: bool, custody: bool) -> Actor {
        let mut actor = self
            .spawn(old, restored, &["worker"], custody)
            .await
            .unwrap();
        actor.ready("jobs_worker_ready").await;
        actor
    }

    async fn consumer(&mut self, restored: bool) -> Actor {
        let mut actor = self
            .spawn(false, restored, &["consume"], true)
            .await
            .unwrap();
        actor.ready("consumer_lifecycle_ready").await;
        actor
    }
}

struct Actor {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
    exited: bool,
}

impl Actor {
    fn diagnostics(&self) -> String {
        format!(
            "stdout={} stderr={}",
            self.stdout.display(),
            self.stderr.display()
        )
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "bounded fixture polling observes its owned child log"
    )]
    async fn ready(&mut self, marker: &str) {
        timeout(WAIT, async {
            loop {
                assert!(
                    self.child.try_wait().unwrap().is_none(),
                    "actor exited before readiness: {}",
                    self.diagnostics()
                );
                if std::fs::read_to_string(&self.stdout)
                    .unwrap()
                    .contains(marker)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("actor readiness is bounded");
    }

    async fn wait(&mut self) -> Option<i32> {
        let code = timeout(Duration::from_secs(180), async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    break status.code();
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("actor exits within finite command bound");
        self.exited = true;
        write_json(
            &self.stdout.with_extension("exit.json"),
            &json!({
                "pid":self.child.id(), "exit_code":code, "ended_at_unix_ms":wall_clock_ms()
            }),
        );
        code
    }

    async fn stop(mut self) {
        kill(
            Pid::from_raw(i32::try_from(self.child.id()).unwrap()),
            Signal::SIGTERM,
        )
        .unwrap();
        assert_eq!(
            timeout(WAIT, self.wait()).await.unwrap(),
            Some(0),
            "{}",
            self.diagnostics()
        );
    }
}

impl Drop for Actor {
    fn drop(&mut self) {
        if !self.exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn required(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is required; use make consumer-lifecycle-check"))
}

fn digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    hex
}

fn wall_clock_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

#[allow(
    clippy::disallowed_methods,
    reason = "finite fixture emits its bounded identity manifest synchronously"
)]
fn write_json(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

async fn connect(endpoint: &Endpoint) -> PgPool {
    infra_postgres::connect(
        &infra_postgres::Dsn::admit(&endpoint.database).unwrap(),
        &infra_postgres::PoolOptions {
            max_connections: std::num::NonZeroU32::new(3).unwrap(),
            application_name: "consumer-lifecycle-inspection",
            default_isolation: infra_postgres::Isolation::ReadCommitted,
            session_budgets: infra_postgres::SessionBudgets::Startup,
        },
    )
    .await
    .unwrap()
}

async fn state(pool: &PgPool, id: &str) -> String {
    sqlx::query_scalar(
        "SELECT state FROM background_jobs WHERE unique_key = $1 OR payload->>'logical_id' = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn settled(pool: &PgPool, id: &str, expected: &str) {
    timeout(WAIT, async {
        loop {
            if state(pool, id).await == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{id} did not become {expected}"));
}

async fn effect_ids(pool: &PgPool) -> BTreeSet<String> {
    sqlx::query_scalar("SELECT logical_id FROM lifecycle_effects ORDER BY logical_id")
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .collect()
}

async fn effects(pool: &PgPool, expected: &[&str]) {
    let expected: BTreeSet<String> = expected.iter().map(|id| (*id).to_owned()).collect();
    timeout(WAIT, async {
        loop {
            if effect_ids(pool).await == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("exact logical-ID effects converge");
    let mismatches: Vec<String> = sqlx::query_scalar("SELECT e.logical_id FROM lifecycle_effects e LEFT JOIN lifecycle_intents i USING (logical_id) WHERE i.meaning IS DISTINCT FROM e.meaning")
        .fetch_all(pool).await.unwrap();
    assert!(
        mismatches.is_empty(),
        "effect meaning differs from durable intent"
    );
}

async fn database_snapshot(pool: &PgPool) -> Value {
    // Fixed SQL observes the complete finite fixture, including payload bytes
    // represented by PostgreSQL's canonical JSON, history and both sequences.
    let mut result = serde_json::Map::new();
    for (name, query) in [
        (
            "migrations",
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY version),'[]') FROM _sqlx_migrations t",
        ),
        (
            "jobs",
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY id),'[]') FROM background_jobs t",
        ),
        (
            "intents",
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY logical_id),'[]') FROM lifecycle_intents t",
        ),
        (
            "effects",
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY logical_id),'[]') FROM lifecycle_effects t",
        ),
        (
            "deliveries",
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY sequence),'[]') FROM lifecycle_deliveries t",
        ),
        (
            "claim_sequence",
            "SELECT jsonb_build_object('last_value',last_value,'is_called',is_called) FROM background_jobs_claim_generation",
        ),
        (
            "delivery_sequence",
            "SELECT jsonb_build_object('last_value',last_value,'is_called',is_called) FROM lifecycle_deliveries_sequence_seq",
        ),
    ] {
        // All statements above are fixed fixture-owned inspection SQL.
        let value: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
            .fetch_one(pool)
            .await
            .unwrap();
        result.insert(name.to_owned(), value);
    }
    Value::Object(result)
}

async fn broker_snapshot(url: &str) -> Value {
    let client = async_nats::connect(url).await.unwrap();
    let js = jetstream::new(client.clone());
    let mut result = serde_json::Map::new();
    for name in ["LIFECYCLE_SOURCE", "LIFECYCLE_DLQ"] {
        let mut stream = js.get_stream(name).await.unwrap();
        let info = stream.info().await.unwrap().clone();
        assert!(info.state.last_sequence <= 64, "finite archive bound");
        let mut messages = Vec::new();
        for sequence in info.state.first_sequence..=info.state.last_sequence {
            if sequence == 0 {
                continue;
            }
            let message = stream
                .get_raw_message(sequence)
                .await
                .expect("every retained sequence has an identity");
            let mut headers: Vec<_> = message
                .headers
                .iter()
                .map(|(name, values)| {
                    (
                        name.to_string(),
                        values.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    )
                })
                .collect();
            headers.sort();
            messages.push(
                json!({"sequence":sequence, "subject":message.subject.to_string(),
                "logical_id":message.headers.get("Message-Id").map(ToString::to_string),
                "payload_sha256":digest(&message.payload), "headers":headers}),
            );
        }
        let durable = if name == "LIFECYCLE_SOURCE" {
            let mut durable = stream
                .get_consumer::<consumer::pull::Config>("lifecycle")
                .await
                .unwrap();
            let info = durable.info().await.unwrap();
            Some(
                json!({"created":info.created.to_string(), "config":info.config,
                "delivered":{"consumer_seq":info.delivered.consumer_sequence,"stream_seq":info.delivered.stream_sequence},
                "ack_floor":{"consumer_seq":info.ack_floor.consumer_sequence,"stream_seq":info.ack_floor.stream_sequence},
                "num_ack_pending":info.num_ack_pending,"num_pending":info.num_pending,"num_redelivered":info.num_redelivered}),
            )
        } else {
            None
        };
        result.insert(
            name.to_owned(),
            json!({"config":info.config, "created":info.created.to_string(),
            "state":{"messages":info.state.messages, "first_seq":info.state.first_sequence,
                "last_seq":info.state.last_sequence,"num_deleted":info.state.deleted_count,
                "deleted":info.state.deleted}, "messages":messages, "durable":durable}),
        );
    }
    client.flush().await.unwrap();
    Value::Object(result)
}

fn compare_broker(before: &Value, after: &Value) {
    for name in ["LIFECYCLE_SOURCE", "LIFECYCLE_DLQ"] {
        for field in ["config", "messages"] {
            assert_eq!(
                before[name][field], after[name][field],
                "restored {name} {field}"
            );
        }
        for field in [
            "messages",
            "first_seq",
            "last_seq",
            "num_deleted",
            "deleted",
        ] {
            assert_eq!(
                before[name]["state"][field], after[name]["state"][field],
                "restored stream positions {field}"
            );
        }
    }
    for field in [
        "config",
        "delivered",
        "ack_floor",
        "num_ack_pending",
        "num_pending",
        "num_redelivered",
    ] {
        assert_eq!(
            before["LIFECYCLE_SOURCE"]["durable"][field],
            after["LIFECYCLE_SOURCE"]["durable"][field],
            "restored durable {field}"
        );
    }
}

async fn native(action: &str, evidence: &Path) {
    let stdout = evidence.join(format!("native-{action}.stdout"));
    let stderr = evidence.join(format!("native-{action}.stderr"));
    let out = tokio::fs::File::create(&stdout)
        .await
        .unwrap()
        .into_std()
        .await;
    let err = tokio::fs::File::create(&stderr)
        .await
        .unwrap()
        .into_std()
        .await;
    let child = Command::new("bash")
        .arg(required("LIFECYCLE_CARRIER"))
        .arg(action)
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .spawn()
        .unwrap();
    let mut process = Actor {
        child,
        stdout,
        stderr,
        exited: false,
    };
    assert_eq!(process.wait().await, Some(0), "{}", process.diagnostics());
}

#[tokio::test]
#[ignore = "requires fixed historical executables and two isolated native stores; make consumer-lifecycle-check executes this case"]
#[allow(
    clippy::too_many_lines,
    clippy::disallowed_methods,
    reason = "one finite custody/restore sequence owns every actor and reads its bounded refusal logs"
)]
async fn historical_actors_survive_native_restore() {
    let mut harness = Harness::from_env();
    harness.action(true, false, &["migrate"]).await;
    harness.action(true, false, &["setup"]).await;
    let source = connect(&harness.source).await;
    let old = harness.worker(true, false, false).await;
    harness
        .action(true, false, &["enqueue", "job", "job-old"])
        .await;
    settled(&source, "job-old", "completed").await;

    let mut refused = harness
        .spawn(false, false, &["worker"], false)
        .await
        .unwrap();
    assert_eq!(refused.wait().await, Some(1));
    let refusal = std::fs::read_to_string(&refused.stderr).unwrap();
    assert!(
        refusal.contains("embedded migrations are pending"),
        "history refusal must be causal"
    );
    assert!(
        !std::fs::read_to_string(&refused.stdout)
            .unwrap()
            .contains("jobs_worker_ready")
    );
    harness.action(false, false, &["migrate"]).await;
    harness
        .action(true, false, &["enqueue", "job", "job-old-expanded"])
        .await;
    settled(&source, "job-old-expanded", "completed").await;
    let new = harness.worker(false, false, false).await;
    harness
        .action(false, false, &["enqueue", "job", "job-overlap"])
        .await;
    settled(&source, "job-overlap", "completed").await;
    old.stop().await;
    let custody_required = true;
    assert!(
        harness
            .spawn(true, false, &["worker"], custody_required)
            .await
            .is_err()
    );
    harness
        .action(false, false, &["enqueue", "failed", "job-retained"])
        .await;
    settled(&source, "job-retained", "failed").await;
    new.stop().await;

    let failed: (String, i64) = sqlx::query_as(
        "SELECT id::text, claim_generation FROM background_jobs WHERE unique_key = 'job-retained'",
    )
    .fetch_one(&source)
    .await
    .unwrap();
    let token = infra_jobs::operator::RecoveryTarget::new(
        &failed.0,
        "test.lifecycle",
        &failed.1.to_string(),
    )
    .unwrap();
    infra_postgres::in_tx(&source, async |tx| {
        infra_jobs::operator::redrive(tx, &token).await
    })
    .await
    .unwrap();
    drop(token);
    let new = harness.worker(false, false, true).await;
    settled(&source, "job-retained", "failed").await;
    new.stop().await;
    sqlx::query("UPDATE background_jobs SET finished_at = statement_timestamp() - interval '8 days' WHERE unique_key = 'job-retained'")
        .execute(&source).await.unwrap();
    harness.action(false, false, &["retain"]).await;
    assert_eq!(state(&source, "job-retained").await, "failed");

    for id in ["event-before", "event-dead-letter"] {
        harness
            .action(false, false, &["enqueue", "event", id])
            .await;
    }
    let new = harness.worker(false, false, true).await;
    let consumer = harness.consumer(false).await;
    effects(
        &source,
        &["job-old", "job-old-expanded", "job-overlap", "event-before"],
    )
    .await;
    timeout(WAIT, async {
        loop {
            let snapshot = broker_snapshot(&harness.source.nats).await;
            if snapshot["LIFECYCLE_DLQ"]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["logical_id"] == "event-dead-letter")
                && snapshot["LIFECYCLE_SOURCE"]["durable"]["num_ack_pending"] == 0
                && snapshot["LIFECYCLE_SOURCE"]["durable"]["num_pending"] == 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("source settles and DLQ retains actual failed event");
    consumer.stop().await;
    harness
        .action(false, false, &["enqueue", "event", "event-source-pending"])
        .await;
    timeout(WAIT, async {
        loop {
            let snapshot = broker_snapshot(&harness.source.nats).await;
            if snapshot["LIFECYCLE_SOURCE"]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["logical_id"] == "event-source-pending")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("one published identity remains outstanding at the fence");
    new.stop().await;
    harness
        .action(false, false, &["enqueue", "job", "job-pending"])
        .await;
    harness
        .action(false, false, &["enqueue", "event", "event-pending"])
        .await;
    let before_db = database_snapshot(&source).await;
    assert!(
        before_db["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|job| job["unique_key"] == "job-retained"
                && job["recovery_history"].as_array().unwrap().len() == 1)
    );
    let before_broker = broker_snapshot(&harness.source.nats).await;
    assert_eq!(
        infra_postgres::close(&source, WAIT).await,
        infra_postgres::Closed::Complete
    );
    let fenced_at = Instant::now();
    write_json(
        &harness.evidence.join("fenced.json"),
        &json!({"database":before_db, "broker":before_broker,
        "fenced_at_unix_ms":wall_clock_ms(),
        "old_retention_owners_joined":true, "all_writers_joined":true, "saved_operator_tokens_invalidated":true}),
    );
    native("archive", &harness.evidence).await;
    let restore_started = Instant::now();
    write_json(
        &harness.evidence.join("restore-started.json"),
        &json!({"started_at_unix_ms":wall_clock_ms()}),
    );
    native("restore", &harness.evidence).await;

    let restored = connect(&harness.restored).await;
    let after_db = database_snapshot(&restored).await;
    let after_broker = broker_snapshot(&harness.restored.nats).await;
    write_json(
        &harness.evidence.join("restored.json"),
        &json!({"database":after_db, "broker":after_broker}),
    );
    assert_eq!(
        before_db, after_db,
        "whole restored durable database identities"
    );
    compare_broker(&before_broker, &after_broker);
    let before_generation = before_db["claim_sequence"]["last_value"].as_i64().unwrap();
    assert!(
        before_db["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|job| job["claim_generation"].as_i64().unwrap() <= before_generation)
    );
    // No pre-restore token is carried across this boundary. Re-inspect through
    // the corrected operator before readmission, even if numeric versions match.
    harness
        .action(false, true, &["worker", "inspect", &failed.0])
        .await;
    let new = harness.worker(false, true, true).await;
    let consumer = harness.consumer(true).await;
    let fence_to_ready = fenced_at.elapsed();
    effects(
        &restored,
        &[
            "job-old",
            "job-old-expanded",
            "job-overlap",
            "event-before",
            "job-pending",
            "event-pending",
            "event-source-pending",
        ],
    )
    .await;
    settled(&restored, "job-pending", "completed").await;
    let generation: i64 = sqlx::query_scalar(
        "SELECT claim_generation FROM background_jobs WHERE unique_key = 'job-pending'",
    )
    .fetch_one(&restored)
    .await
    .unwrap();
    assert!(
        generation > before_generation,
        "restored sequence advances beyond every saved generation"
    );
    // Deliberately cross the actual stream's 100 ms dedupe window. This wait
    // selects the protocol condition; effect/delivery observations supply proof.
    tokio::time::sleep(Duration::from_millis(150)).await;
    harness
        .action(false, true, &["replay", "event-before"])
        .await;
    timeout(WAIT, async {
        loop {
            let deliveries: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM lifecycle_deliveries WHERE logical_id = 'event-before'",
            )
            .fetch_one(&restored)
            .await
            .unwrap();
            if deliveries >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("replay reaches typed handler after broker dedupe expiry");
    effects(
        &restored,
        &[
            "job-old",
            "job-old-expanded",
            "job-overlap",
            "event-before",
            "job-pending",
            "event-pending",
            "event-source-pending",
        ],
    )
    .await;
    consumer.stop().await;
    new.stop().await;
    assert_eq!(state(&restored, "job-retained").await, "failed");
    let final_db = database_snapshot(&restored).await;
    assert!(
        final_db["jobs"].as_array().unwrap().iter().all(|job| {
            if job["unique_key"] == "job-retained" {
                job["state"] == "failed"
            } else {
                job["state"] == "completed"
            }
        }),
        "all restored queue identities have an explained durable disposition"
    );
    let final_broker = broker_snapshot(&harness.restored.nats).await;
    assert_eq!(
        before_broker["LIFECYCLE_DLQ"]["messages"],
        final_broker["LIFECYCLE_DLQ"]["messages"]
    );
    assert_eq!(
        final_broker["LIFECYCLE_SOURCE"]["durable"]["num_ack_pending"],
        0
    );
    assert_eq!(
        final_broker["LIFECYCLE_SOURCE"]["durable"]["num_pending"],
        0
    );
    write_json(
        &harness.evidence.join("completed.json"),
        &json!({
        "scenario":"historical_actors_survive_native_restore", "completed":true,
        "completed_at_unix_ms":wall_clock_ms(),
            "historical_pair":[OLD,NEW], "database":final_db, "broker":final_broker,
            "fence_to_ready_ms":fence_to_ready.as_millis(),
            "restore_start_to_durable_completion_ms":restore_started.elapsed().as_millis(),
            "unresolved":["event-dead-letter: retained in DLQ; no synthetic business effect authorized"],
            "old_custody_rollback":"refused", "published_A_B_rollback":"separate proof required",
            "scope":"finite synthetic single-node rehearsal; no production RPO/RTO or cluster claim"
        }),
    );
    assert_eq!(
        infra_postgres::close(&restored, WAIT).await,
        infra_postgres::Closed::Complete
    );
}
