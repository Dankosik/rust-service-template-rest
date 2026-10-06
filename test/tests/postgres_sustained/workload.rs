//! Native work and independent synthetic effects. Dynamic SQL is confined to
//! disposable fixture inventory, timestamp staging and independent observations.
use super::evidence::Family;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue};
use infra_idempotency_store::{Attempted, CallerIdentity, CallerKind, Record, ScopeKey, Store};
use infra_jobs::operator::{self, Inspection, InspectionResult, RecoveryTarget};
use infra_jobs::{Engine, EnqueueOptions, Enqueued, Job, JobError, JobKind, Kinds, Policy};
use infra_postgres::{Dsn, Isolation, PgPool, PoolOptions, Tx, in_tx};
use infra_webhooks::{
    inbound::{Consumer, Consumers, Incoming, Processor, ReceiptOutcome, Receiver, async_trait},
    protocol::KeyRing,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use sqlx::Row as _;
use std::{
    num::NonZeroU32,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(super) type Error = Box<dyn std::error::Error + Send + Sync>;
pub(super) type Result<T> = std::result::Result<T, Error>;
pub(super) const ENDPOINT: &str = "sustained-lab";
const KEY: &str = "whsec_Q0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0M=";
pub(super) const WIDE_BYTES: usize = 512 * 1024;
const REPLACEMENT_BASE: u64 = 20_000_000;
const WIDE_BASE: u64 = 10_000_000;

#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct Inventory {
    pub seed: u64,
    pub generation: u64,
    pub live: u64,
    pub idempotency_rows: u64,
    pub receipt_rows: u64,
    pub jobs_rows: u64,
}

fn replacement_identities() -> impl Iterator<Item = u64> {
    (0..120_000)
        .filter(|tick| (600..=734).contains(&((tick * 137) % 1000)))
        .map(|tick| REPLACEMENT_BASE + tick)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IdempotencyOutcome {
    Committed,
    Replay,
    Mismatch,
    InProgress,
}

pub(super) fn failed(message: &str) -> Error {
    std::io::Error::other(message).into()
}

pub(super) async fn pool(dsn: &Dsn, connections: u32) -> Result<PgPool> {
    Ok(infra_postgres::connect(
        dsn,
        &PoolOptions {
            max_connections: NonZeroU32::new(connections).ok_or_else(|| failed("zero pool"))?,
            application_name: "postgres-sustained",
            default_isolation: Isolation::ReadCommitted,
            session_budgets: infra_postgres::SessionBudgets::Startup,
        },
    )
    .await?)
}

/// Seed/family/sequence/generation are the entire payload and identity input.
pub(super) fn digest(seed: u64, family: u8, sequence: u64, generation: u64) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(seed.to_le_bytes());
    hash.update([family]);
    hash.update(sequence.to_le_bytes());
    hash.update(generation.to_le_bytes());
    hash.finalize().into()
}

pub(super) fn body(seed: u64, family: u8, sequence: u64, generation: u64, size: usize) -> Bytes {
    let input = digest(seed, family, sequence, generation);
    let mut state = u64::from_le_bytes(input[..8].try_into().expect("eight bytes")) | 1;
    let compressible = (sequence + generation) % 2 == 0;
    (0..size)
        .map(|_| {
            if compressible {
                input[0]
            } else {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            }
        })
        .collect::<Vec<_>>()
        .into()
}

pub(super) const fn body_size(sequence: u64) -> usize {
    match sequence % 20 {
        0..=15 => 1024,
        16..=18 => 64 * 1024,
        _ => WIDE_BYTES,
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Payload {
    identity: u64,
    generation: u64,
    disposition: u8,
    bytes: String,
}
impl JobKind for Payload {
    const NAME: &'static str = "lab.sustained";
}
#[derive(Serialize, Deserialize)]
struct Unhandled(Payload);
impl JobKind for Unhandled {
    const NAME: &'static str = "lab.unhandled";
}

async fn handle(job: Job<Payload>) -> std::result::Result<(), JobError> {
    if job.payload().disposition >= 95 {
        return Err(JobError::permanent("declared retained failure"));
    }
    if job.payload().disposition >= 85 && job.attempt() == 1 {
        return Err(JobError::retryable("declared first-attempt retry"));
    }
    in_tx(job.pool(), async |tx| -> Result<()> {
        effect(tx, "jobs", &job.id().to_string(), job.payload().generation).await?;
        job.complete_in_tx(tx).await?;
        Ok(())
    })
    .await
    .map_err(|_| JobError::permanent("synthetic effect or fenced completion failed"))
}

struct WebhookEffect;
#[async_trait]
impl Consumer for WebhookEffect {
    async fn process(
        &self,
        tx: &mut Tx<'_>,
        incoming: &Incoming,
    ) -> std::result::Result<(), JobError> {
        // The retained message id contains the generation; the primary key is
        // an independent duplicate-effect oracle, not ON CONFLICT suppression.
        sqlx::query("INSERT INTO sustained_effects (family, identity, generation) VALUES ('webhook', $1, 0)")
            .bind(incoming.message_id()).execute(&mut *tx).await?;
        Ok(())
    }
}

async fn effect(tx: &mut Tx<'_>, family: &str, identity: &str, generation: u64) -> Result<()> {
    sqlx::query("INSERT INTO sustained_effects (family, identity, generation) VALUES ($1, $2, $3)")
        .bind(family)
        .bind(identity.as_bytes())
        .bind(i64::try_from(generation)?)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

// Admission expectations are written independently of the worker effect. They
// survive retention and span every process role in a cloned laboratory database.
#[allow(
    clippy::too_many_arguments,
    reason = "explicit synthetic admission identity"
)]
async fn expect_effect(
    tx: &mut Tx<'_>,
    family: &str,
    identity: &str,
    generation: u64,
    seed: u64,
    logical_id: u64,
    body_bytes: usize,
    terminal: &str,
    payload_hash: &[u8],
) -> Result<()> {
    sqlx::query("INSERT INTO sustained_admissions (family, identity, generation, seed, logical_id, body_bytes, terminal, payload_hash) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(family).bind(identity.as_bytes()).bind(i64::try_from(generation)?)
        .bind(i64::try_from(seed)?).bind(i64::try_from(logical_id)?)
        .bind(i64::try_from(body_bytes)?).bind(terminal).bind(payload_hash).execute(&mut *tx).await?;
    Ok(())
}

pub(super) fn worker(pool: PgPool) -> Result<Engine> {
    let mut kinds = Kinds::new();
    kinds.register::<Payload>(
        Policy {
            max_attempts: 3,
            timeout: Duration::from_secs(2),
            max_running: None,
        },
        handle,
    );
    let mut consumers = Consumers::new();
    consumers.insert(ENDPOINT, Arc::new(WebhookEffect))?;
    Processor::new(consumers).register(&mut kinds);
    Ok(Engine::new(
        pool,
        kinds.validate()?,
        NonZeroU32::new(2).expect("two workers"),
    ))
}

#[derive(Clone)]
pub(super) struct Client {
    pub pool: PgPool,
    pub store: Store,
    pub receiver: Receiver,
    seed: u64,
    pub inventory: Inventory,
}
impl Client {
    pub(super) fn new(pool: PgPool, seed: u64, inventory: Inventory) -> Result<Self> {
        if inventory.live == 0
            || inventory.idempotency_rows == 0
            || inventory.receipt_rows == 0
            || (inventory.jobs_rows < 200 || inventory.jobs_rows % 200 != 0)
        {
            return Err(failed(
                "inventory must contain every native family and retained failures",
            ));
        }
        Ok(Self {
            store: Store::new(pool.clone(), Duration::from_secs(86_400)),
            receiver: Receiver::new(
                pool.clone(),
                [(ENDPOINT.to_owned(), KeyRing::from_encoded(KEY, None)?)],
            ),
            pool,
            seed,
            inventory,
        })
    }
    pub(super) fn start_maintenance(&self, tracker: &TaskTracker, cancel: &CancellationToken) {
        tracker.spawn(self.store.clone().run_cleanup(cancel.clone()));
        tracker.spawn(self.receiver.clone().run_cleanup(cancel.clone()));
    }
    async fn idempotency(
        &self,
        seed: u64,
        identity: u64,
        generation: u64,
        size: usize,
        mode: u8,
    ) -> Result<IdempotencyOutcome> {
        let scope = ScopeKey::from_digest(digest(seed, 1, identity, 0));
        let fingerprint = digest(seed, 2, identity, generation);
        let caller = CallerIdentity {
            issuer: "https://lab.invalid".into(),
            kind: CallerKind::Subject,
            value: format!("{seed}:{identity}"),
        };
        let expected = Record {
            fingerprint,
            status: 201,
            headers: vec![],
            body: body(seed, 1, identity, generation, size),
        };
        let supplied = if mode == 2 {
            digest(seed, 3, identity, generation)
        } else {
            fingerprint
        };
        let result = self
            .store
            .attempt(
                &scope,
                &caller,
                &supplied,
                async |tx| -> Result<(Record, ())> {
                    if mode == 4 {
                        // This lock lives in the real attempt transaction. A cleanup
                        // winner is an explicit failed replacement, never a new key.
                        let retained: Option<Vec<u8>> = sqlx::query_scalar("SELECT scope_key FROM http_idempotency_records WHERE scope_key=$1 AND expires_at <= statement_timestamp() FOR UPDATE")
                            .bind(digest(seed, 1, identity, 0).as_slice()).fetch_optional(&mut *tx).await?;
                        if retained.is_none() { return Err(failed("expired replacement scope was removed before native admission")); }
                    }
                    let effect_id = format!("{seed}:{identity}");
                    expect_effect(tx, "idempotency", &effect_id, generation, seed, identity, size, "committed", &Sha256::digest(&expected.body)).await?;
                    effect(tx, "idempotency", &effect_id, generation).await?;
                    Ok((expected.clone(), ()))
                },
            )
            .await?;
        match (mode, result) {
            (0 | 3 | 4, Attempted::Committed(())) => Ok(IdempotencyOutcome::Committed),
            (2, Attempted::Mismatch) => Ok(IdempotencyOutcome::Mismatch),
            (1 | 3, Attempted::Replay(record)) if record == expected => {
                Ok(IdempotencyOutcome::Replay)
            }
            (3, Attempted::InProgress) => Ok(IdempotencyOutcome::InProgress),
            (_, Attempted::RolledBack(error)) => Err(error),
            _ => Err(failed("idempotency unexpected result or replay bytes")),
        }
    }
    async fn webhook(
        &self,
        seed: u64,
        identity: u64,
        generation: u64,
        duplicate: bool,
    ) -> Result<()> {
        let id = format!("{seed}-{identity}-{generation}");
        let data = body(
            seed,
            4,
            identity,
            generation,
            body_size(identity).min(infra_webhooks::protocol::MAX_BODY_BYTES),
        );
        let payload_hash = Sha256::digest(&data);
        let now = SystemTime::now();
        let timestamp = i64::try_from(now.duration_since(UNIX_EPOCH)?.as_secs())?;
        let keys = KeyRing::from_encoded(KEY, None)?;
        let mut headers = HeaderMap::new();
        headers.insert("webhook-id", HeaderValue::from_str(&id)?);
        headers.insert(
            "webhook-timestamp",
            HeaderValue::from_str(&timestamp.to_string())?,
        );
        headers.insert(
            "webhook-signature",
            HeaderValue::from_str(&keys.signatures(id.as_bytes(), timestamp, &data)?)?,
        );
        let got = self
            .receiver
            .receive_bytes(ENDPOINT, &headers, data, now)
            .await?;
        let expected = if duplicate {
            ReceiptOutcome::Duplicate
        } else {
            ReceiptOutcome::Accepted
        };
        if got != expected {
            return Err(failed("webhook unexpected admission"));
        }
        if !duplicate {
            in_tx(&self.pool, async |tx| -> Result<()> {
                expect_effect(
                    tx,
                    "webhook",
                    &id,
                    0,
                    seed,
                    identity,
                    0,
                    "completed",
                    &payload_hash,
                )
                .await
            })
            .await?;
        }
        Ok(())
    }
    async fn enqueue(&self, identity: u64, generation: u64) -> Result<()> {
        let mut payload = Payload {
            identity,
            generation,
            disposition: u8::try_from(identity % 100)?,
            bytes: String::new(),
        };
        let overhead = serde_json::to_vec(&payload)?.len();
        let size = body_size(identity).min(infra_jobs::MAX_PAYLOAD_BYTES.saturating_sub(overhead));
        payload.bytes = body(self.seed, 5, identity, generation, size)
            .iter()
            .map(|b| char::from(b'a' + b % 26))
            .collect();
        let unhandled = generation == 0 && identity % 2000 == 1999;
        in_tx(&self.pool, async |tx| -> Result<()> {
            let enqueued = if unhandled {
                infra_jobs::enqueue(tx, &Unhandled(payload.clone()), EnqueueOptions::default()).await?
            } else {
                infra_jobs::enqueue(tx, &payload, EnqueueOptions::default()).await?
            };
            let Enqueued::Created(id) = enqueued else { return Err(failed("new job not created")); };
            if unhandled {
                // Fixture-only valid retained failure; it cannot be dispatched
                // because its kind deliberately has no registered handler.
                sqlx::query("UPDATE background_jobs SET state='failed', attempts=1, failure_reason='permanent', finished_at=now() WHERE id::text=$1")
                    .bind(id.to_string()).execute(&mut *tx).await?;
            }
            expect_effect(tx, "jobs", &id.to_string(), generation, self.seed, identity, size,
                if payload.disposition >= 95 { "failed" } else if payload.disposition >= 85 { "retried" } else { "completed" }, &Sha256::digest(serde_json::to_vec(&payload)?)).await?;
            Ok(())
        }).await
    }
    async fn inspect_or_redrive(&self, redrive: bool, selector: u64) -> Result<()> {
        let request = Inspection::failed(None, 500)?;
        let result = in_tx(&self.pool, async |tx| operator::inspect(tx, &request).await).await?;
        if redrive {
            let InspectionResult::Page { items, .. } = result else {
                return Err(failed("expected inspection page"));
            };
            let items: Vec<_> = items
                .into_iter()
                .filter(|item| item.kind == Payload::NAME)
                .collect();
            if items.is_empty() {
                return Err(failed("redrive fixture absent"));
            }
            let item = &items[usize::try_from(selector % u64::try_from(items.len())?)?];
            let target = RecoveryTarget::new(&item.id.to_string(), &item.kind, &item.version)?;
            in_tx(&self.pool, async |tx| operator::redrive(tx, &target).await).await?;
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let done: bool = sqlx::query_scalar("SELECT state='failed' AND recovery_count > $2 FROM background_jobs WHERE id::text=$1")
                        .bind(item.id.to_string()).bind(item.recovery_count).fetch_one(&self.pool).await?;
                    if done { return Ok::<_, Error>(()); }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.map_err(|_| failed("redriven job did not acknowledge re-failure"))??;
        }
        Ok(())
    }
    pub(super) async fn operation(&self, action: &Action) -> Result<()> {
        let seed = match action.kind {
            Kind::Replay | Kind::Wide | Kind::Mismatch | Kind::Replace | Kind::WebhookDuplicate => {
                self.inventory.seed
            }
            _ => self.seed,
        };
        match action.kind {
            Kind::Inspect => self.inspect_or_redrive(false, action.identity).await,
            Kind::Redrive => self.inspect_or_redrive(true, action.identity).await,
            Kind::Enqueue => self.enqueue(action.identity, action.generation).await,
            Kind::Replay | Kind::Wide | Kind::Mismatch | Kind::New | Kind::Replace => {
                let mode = match action.kind {
                    Kind::Replay | Kind::Wide => 1,
                    Kind::Mismatch => 2,
                    Kind::Replace => 4,
                    _ => 0,
                };
                self.idempotency(seed, action.identity, action.generation, action.size, mode)
                    .await?;
                Ok(())
            }
            Kind::Concurrent => {
                let (one, two) = tokio::join!(
                    self.idempotency(seed, action.identity, action.generation, action.size, 3),
                    self.idempotency(seed, action.identity, action.generation, action.size, 3)
                );
                let (one, two) = (one?, two?);
                if (one == IdempotencyOutcome::Committed) == (two == IdempotencyOutcome::Committed)
                {
                    return Err(failed(
                        "same-key arbitration did not acknowledge exactly one commit",
                    ));
                }
                let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM sustained_effects WHERE family='idempotency' AND identity=$1 AND generation=$2")
                    .bind(format!("{seed}:{}", action.identity).as_bytes()).bind(i64::try_from(action.generation)?).fetch_one(&self.pool).await?;
                if effects != 1 {
                    return Err(failed("same-key arbitration effect cardinality"));
                }
                self.idempotency(seed, action.identity, action.generation, action.size, 1)
                    .await?;
                Ok(())
            }
            Kind::WebhookNew => {
                self.webhook(seed, action.identity, action.generation, false)
                    .await
            }
            Kind::WebhookDuplicate => {
                self.webhook(seed, action.identity, action.generation, true)
                    .await
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(super) enum Kind {
    Inspect,
    Redrive,
    Enqueue,
    Replay,
    Wide,
    Mismatch,
    New,
    Replace,
    Concurrent,
    WebhookNew,
    WebhookDuplicate,
}
#[derive(Clone, Debug, Serialize)]
pub(super) struct Action {
    pub family: Family,
    pub kind: Kind,
    pub identity: u64,
    pub generation: u64,
    pub size: usize,
}

/// Two service roles take alternating global indices; their combined bound is
/// 64 and their combined rates are precisely 100 mixed + 64 wide arrivals/s.
pub(super) fn action(seed: u64, tick: u64, wide: bool, inventory: Inventory) -> Action {
    if wide {
        let mut stride = (seed % inventory.live).max(1);
        while gcd(stride, inventory.live) != 1 {
            stride += 1;
        }
        return Action {
            family: Family::WideReplay,
            kind: Kind::Wide,
            identity: WIDE_BASE + (tick * stride + seed) % inventory.live,
            generation: 0,
            size: WIDE_BYTES,
        };
    }
    // The repeat seed shuffles reads without changing the fixed operation mix.
    let slot = ((tick % 1000) * 137) % 1000;
    let identity = 1_000_000 + tick;
    let generation = inventory.generation + 1;
    let current = |identity, rows| {
        if identity < rows / 2 {
            inventory.generation
        } else {
            0
        }
    };
    let (family, kind, identity, generation) = match slot {
        0..=284 => (Family::Jobs, Kind::Inspect, identity + seed, generation),
        285..=296 => (
            Family::Jobs,
            Kind::Enqueue,
            1_000_000 + (tick / 1000) * 12 + slot - 285,
            generation,
        ),
        297..=299 => (Family::Jobs, Kind::Redrive, identity + seed, generation),
        300..=599 => {
            let id = (tick + seed) % inventory.idempotency_rows;
            (
                Family::Idempotency,
                Kind::Replay,
                id,
                current(id, inventory.idempotency_rows),
            )
        }
        600..=734 => (
            Family::Idempotency,
            Kind::Replace,
            REPLACEMENT_BASE + tick,
            generation,
        ),
        735..=749 => (Family::Idempotency, Kind::New, identity, generation),
        750..=774 => {
            let id = (tick + seed) % inventory.idempotency_rows;
            (
                Family::Idempotency,
                Kind::Mismatch,
                id,
                current(id, inventory.idempotency_rows),
            )
        }
        775..=799 => (Family::Idempotency, Kind::Concurrent, identity, generation),
        800..=989 => {
            let id = (tick + seed) % inventory.receipt_rows;
            (
                Family::Webhook,
                Kind::WebhookDuplicate,
                id,
                current(id, inventory.receipt_rows),
            )
        }
        _ => (Family::Webhook, Kind::WebhookNew, identity, generation),
    };
    Action {
        family,
        kind,
        identity,
        generation,
        size: body_size(identity + generation),
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Fixture tables are disposable independent oracles, never production schema.
pub(super) async fn create_fixture(pool: &PgPool) -> Result<()> {
    sqlx::query("CREATE TABLE sustained_effects (family text NOT NULL, identity bytea NOT NULL, generation bigint NOT NULL, PRIMARY KEY (family, identity, generation))").execute(pool).await?;
    sqlx::query("CREATE TABLE sustained_admissions (family text NOT NULL, identity bytea NOT NULL, generation bigint NOT NULL, seed bigint NOT NULL, logical_id bigint NOT NULL, body_bytes bigint NOT NULL, terminal text NOT NULL, payload_hash bytea NOT NULL, retired boolean NOT NULL DEFAULT false, eligible_at timestamptz, PRIMARY KEY (family, identity, generation))").execute(pool).await?;
    sqlx::query("CREATE TABLE sustained_seed_inventory (singleton boolean PRIMARY KEY DEFAULT true CHECK(singleton), seed bigint NOT NULL, live bigint NOT NULL, idempotency_rows bigint NOT NULL, receipt_rows bigint NOT NULL, jobs_rows bigint NOT NULL)").execute(pool).await?;
    sqlx::query("CREATE TABLE sustained_preparation (singleton boolean PRIMARY KEY DEFAULT true CHECK(singleton), seed_started_unix_ms bigint NOT NULL, seed_attempt_started_unix_ms bigint NOT NULL, seed_elapsed_before_attempt_ms bigint NOT NULL, phase_snapshots jsonb NOT NULL DEFAULT '[]'::jsonb, adjustments_used smallint NOT NULL DEFAULT 0 CHECK(adjustments_used BETWEEN 0 AND 1), state text NOT NULL, report jsonb, previous_report jsonb, seed_cohort_hash text)").execute(pool).await?;
    Ok(())
}

pub(super) async fn start_preparation(pool: &PgPool, clock: [u64; 3]) -> Result<()> {
    sqlx::query("INSERT INTO sustained_preparation(seed_started_unix_ms,seed_attempt_started_unix_ms,seed_elapsed_before_attempt_ms,state) VALUES ($1,$2,$3,'inventory_running')")
        .bind(i64::try_from(clock[0])?).bind(i64::try_from(clock[1])?).bind(i64::try_from(clock[2])?).execute(pool).await?;
    Ok(())
}

pub(super) async fn admit_preparation(client: &Client, mode: &str, clock: [u64; 3]) -> Result<()> {
    let expected = serde_json::json!({"live_bodies":client.inventory.live,"idempotency_rows":client.inventory.idempotency_rows,"receipt_rows":client.inventory.receipt_rows,"jobs_rows":client.inventory.jobs_rows});
    let sql = if mode == "inventory-adjust" {
        "UPDATE sustained_preparation SET state='adjustment_running',adjustments_used=1,previous_report=report WHERE seed_started_unix_ms=$1 AND state='inventory_ready' AND adjustments_used=0 AND report->'proposed_counts'=$2 AND seed_attempt_started_unix_ms=$3 AND seed_elapsed_before_attempt_ms=$4"
    } else {
        "UPDATE sustained_preparation SET state='churn_running' WHERE seed_started_unix_ms=$1 AND state='inventory_ready' AND report->'current_counts'=$2 AND seed_attempt_started_unix_ms=$3 AND seed_elapsed_before_attempt_ms=$4"
    };
    if sqlx::query(sql)
        .bind(i64::try_from(clock[0])?)
        .bind(expected)
        .bind(i64::try_from(clock[1])?)
        .bind(i64::try_from(clock[2])?)
        .execute(&client.pool)
        .await?
        .rows_affected()
        != 1
    {
        return Err(failed(
            "preparation state, clock, or exact admitted counts differ; no retry or second adjustment",
        ));
    }
    Ok(())
}

/// The admission CAS already committed. A failed reset consumes the adjustment.
/// Called only by the controller before any role is spawned.
pub(super) async fn reset_preparation(pool: &PgPool) -> Result<()> {
    in_tx(pool,async |tx| -> Result<()> {
        let admitted: bool=sqlx::query_scalar("SELECT state='adjustment_running' AND adjustments_used=1 FROM sustained_preparation FOR UPDATE").fetch_one(&mut *tx).await?;
        if !admitted { return Err(failed("single reset admission absent")); }
        sqlx::query("TRUNCATE background_jobs,http_idempotency_records,webhook_receipts,sustained_effects,sustained_admissions,sustained_seed_inventory").execute(&mut *tx).await?;
        sqlx::query("UPDATE sustained_preparation SET phase_snapshots='[]'::jsonb,seed_cohort_hash=NULL").execute(&mut *tx).await?;
        Ok(())
    }).await
}

async fn record_seed_phase(client: &Client, phase: &str) -> Result<()> {
    await_settled(client).await?;
    let snapshot =
        serde_json::json!([{"phase":phase,"relations":relation_snapshot(&client.pool).await?}]);
    sqlx::query("UPDATE sustained_preparation SET phase_snapshots=phase_snapshots || $1 WHERE state IN ('inventory_running','adjustment_running')")
        .bind(snapshot).execute(&client.pool).await?;
    Ok(())
}

pub(super) async fn previous_inventory_counts(pool: &PgPool) -> Result<serde_json::Value> {
    let counts: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT previous_report->'current_counts' FROM sustained_preparation")
            .fetch_one(pool)
            .await?;
    Ok(counts.unwrap_or(serde_json::Value::Null))
}

pub(super) async fn seed_phases(pool: &PgPool) -> Result<serde_json::Value> {
    Ok(
        sqlx::query_scalar("SELECT phase_snapshots FROM sustained_preparation")
            .fetch_one(pool)
            .await?,
    )
}

pub(super) async fn preparation_adjustments(pool: &PgPool, started: u64) -> Result<u64> {
    let used: i16 = sqlx::query_scalar(
        "SELECT adjustments_used FROM sustained_preparation WHERE seed_started_unix_ms=$1",
    )
    .bind(i64::try_from(started)?)
    .fetch_one(pool)
    .await?;
    Ok(u64::try_from(used)?)
}

pub(super) async fn finish_inventory(pool: &PgPool, report: &serde_json::Value) -> Result<()> {
    if sqlx::query("UPDATE sustained_preparation SET state='inventory_ready',previous_report=report,report=$1 WHERE state IN ('inventory_running','adjustment_running')")
        .bind(report).execute(pool).await?.rows_affected()!=1 {
        return Err(failed("inventory completion state missing"));
    }
    Ok(())
}

/// Hash ordered durable admissions and the payload hashes captured at admission.
/// Paged reads avoid materializing the full cohort in application memory.
pub(super) async fn seed_cohort_hash(pool: &PgPool) -> Result<String> {
    let mut hash = Sha256::new();
    let mut cursor = (String::new(), Vec::<u8>::new(), -1_i64);
    loop {
        let rows = sqlx::query("SELECT family,identity,generation,seed,logical_id,body_bytes,terminal,payload_hash,retired FROM sustained_admissions WHERE (family,identity,generation)>($1,$2,$3) ORDER BY family,identity,generation LIMIT 1024")
            .bind(&cursor.0).bind(&cursor.1).bind(cursor.2).fetch_all(pool).await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let family: String = row.try_get("family")?;
            let identity: Vec<u8> = row.try_get("identity")?;
            let generation: i64 = row.try_get("generation")?;
            let encoded = serde_json::to_vec(
                &serde_json::json!({"family":family,"identity":identity,"generation":generation,"seed":row.try_get::<i64,_>("seed")?,"logical_id":row.try_get::<i64,_>("logical_id")?,"body_bytes":row.try_get::<i64,_>("body_bytes")?,"terminal":row.try_get::<String,_>("terminal")?,"payload_hash":row.try_get::<Vec<u8>,_>("payload_hash")?,"retired":row.try_get::<bool,_>("retired")?}),
            )?;
            hash.update(u64::try_from(encoded.len())?.to_be_bytes());
            hash.update(encoded);
            cursor = (family, identity, generation);
        }
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub(super) async fn finish_seed(pool: &PgPool, hash: &str) -> Result<()> {
    if sqlx::query("UPDATE sustained_preparation SET state='seed_complete',seed_cohort_hash=$1 WHERE state='churn_running'")
        .bind(hash).execute(pool).await?.rows_affected()!=1 {
        return Err(failed("seed completion state missing"));
    }
    Ok(())
}

pub(super) async fn check_frozen_seed(pool: &PgPool, clock: [u64; 3], hash: &str) -> Result<()> {
    let matches: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sustained_preparation WHERE state='seed_complete' AND seed_started_unix_ms=$1 AND seed_cohort_hash=$2 AND seed_attempt_started_unix_ms=$3 AND seed_elapsed_before_attempt_ms=$4)")
        .bind(i64::try_from(clock[0])?).bind(hash).bind(i64::try_from(clock[1])?).bind(i64::try_from(clock[2])?).fetch_one(pool).await?;
    if !matches || hash.is_empty() {
        return Err(failed(
            "clone seed clock or durable cohort hash differs from frozen manifest",
        ));
    }
    Ok(())
}

/// Append disjoint cohorts for the one permitted measured scaling adjustment.
/// Call before preconditioning; a partial failed seed is an invalid attempt.
pub(super) async fn extend_seed(client: &Client) -> Result<()> {
    let old = sqlx::query(
        "SELECT seed,live,idempotency_rows,receipt_rows,jobs_rows FROM sustained_seed_inventory",
    )
    .fetch_optional(&client.pool)
    .await?;
    let previous = |column| -> Result<u64> {
        Ok(old
            .as_ref()
            .map(|row| row.try_get::<i64, _>(column))
            .transpose()?
            .map(u64::try_from)
            .transpose()?
            .unwrap_or(0))
    };
    let i = client.inventory;
    if client.seed != i.seed {
        return Err(failed(
            "preconditioning client must use frozen inventory seed",
        ));
    }
    if old.is_some() && previous("seed")? != i.seed {
        return Err(failed("seed inventory identity changed"));
    }
    for (name, count) in [
        ("live", i.live),
        ("idempotency_rows", i.idempotency_rows),
        ("receipt_rows", i.receipt_rows),
        ("jobs_rows", i.jobs_rows),
    ] {
        if previous(name)? > count {
            return Err(failed("scaling cannot discard admitted inventory"));
        }
    }
    record_seed_phase(client, "eligibility").await?;
    if old.is_none() {
        for identity in replacement_identities() {
            client.idempotency(i.seed, identity, 0, 1024, 0).await?;
        }
    }
    record_seed_phase(client, "replacements").await?;
    for identity in previous("live")?..i.live {
        client
            .idempotency(i.seed, WIDE_BASE + identity, 0, WIDE_BYTES, 0)
            .await?;
    }
    record_seed_phase(client, "wide").await?;
    for identity in previous("idempotency_rows")?..i.idempotency_rows {
        client
            .idempotency(i.seed, identity, 0, body_size(identity), 0)
            .await?;
    }
    record_seed_phase(client, "idempotency").await?;
    let jobs_start = 2000_u64.saturating_sub(i.jobs_rows);
    if old.is_some() && previous("jobs_rows")? != i.jobs_rows {
        return Err(failed("job cohort resizing requires the admitted reset"));
    }
    for identity in previous("jobs_rows")?..i.jobs_rows {
        client.enqueue(jobs_start + identity, 0).await?;
    }
    record_seed_phase(client, "jobs").await?;
    for identity in previous("receipt_rows")?..i.receipt_rows {
        client.webhook(i.seed, identity, 0, false).await?;
    }
    record_seed_phase(client, "webhooks").await?;
    sqlx::query("INSERT INTO sustained_seed_inventory (seed,live,idempotency_rows,receipt_rows,jobs_rows) VALUES ($1,$2,$3,$4,$5) ON CONFLICT(singleton) DO UPDATE SET live=$2,idempotency_rows=$3,receipt_rows=$4,jobs_rows=$5")
        .bind(i64::try_from(i.seed)?).bind(i64::try_from(i.live)?).bind(i64::try_from(i.idempotency_rows)?)
        .bind(i64::try_from(i.receipt_rows)?).bind(i64::try_from(i.jobs_rows)?).execute(&client.pool).await?;
    Ok(())
}

pub(super) async fn seed(client: &Client, live: u64) -> Result<()> {
    if live != client.inventory.live {
        return Err(failed("wide inventory count differs from manifest"));
    }
    extend_seed(client).await
}

/// Await persisted worker outcomes, never use elapsed time as completion proof.
pub(super) async fn await_settled(client: &Client) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM background_jobs WHERE kind IN ('lab.sustained','webhooks.process') AND state IN ('pending','running')")
                .fetch_one(&client.pool).await?;
            if pending == 0 { return verify_effects(&client.pool).await; }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.map_err(|_| failed("native jobs failed to reach acknowledged terminal state"))?
}

async fn verify_effects(pool: &PgPool) -> Result<()> {
    let wrong: i64 = sqlx::query_scalar("SELECT count(*) FROM sustained_admissions a FULL JOIN sustained_effects e USING(family,identity,generation) WHERE a.family IS NULL OR (a.terminal='failed' AND e.family IS NOT NULL) OR (a.terminal<>'failed' AND e.family IS NULL)")
        .fetch_one(pool).await?;
    if wrong != 0 {
        return Err(failed(
            "native admissions and independent durable effects differ",
        ));
    }
    let wrong_jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM sustained_admissions a LEFT JOIN background_jobs j ON j.id::text=convert_from(a.identity,'UTF8') WHERE a.family='jobs' AND NOT a.retired AND (j.id IS NULL OR j.state<>CASE WHEN a.terminal='failed' THEN 'failed' ELSE 'completed' END OR (a.terminal='retried' AND j.attempts<>2))")
        .fetch_one(pool).await?;
    let failed_webhooks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM background_jobs WHERE kind='webhooks.process' AND state='failed'",
    )
    .fetch_one(pool)
    .await?;
    if wrong_jobs != 0 || failed_webhooks != 0 {
        return Err(failed(
            "native worker final state or retry custody differs from admission",
        ));
    }
    Ok(())
}

pub(super) async fn native_churn(
    client: &Client,
    live: u64,
    generation: u64,
) -> Result<serde_json::Value> {
    if live != client.inventory.live {
        return Err(failed("wide inventory count differs from manifest"));
    }
    let i = client.inventory;
    await_settled(client).await?;
    // First remove exactly the 20% cohort through the native cleanup owner.
    for identity in 0..i.idempotency_rows / 5 {
        sqlx::query("UPDATE http_idempotency_records SET expires_at=now()-interval '1 second' WHERE scope_key=$1")
            .bind(digest(i.seed,1,identity,0).as_slice()).execute(&client.pool).await?;
    }
    for identity in replacement_identities().take(3240) {
        sqlx::query("UPDATE http_idempotency_records SET expires_at=now()-interval '1 second' WHERE scope_key=$1")
            .bind(digest(i.seed,1,identity,0).as_slice()).execute(&client.pool).await?;
    }
    let idempotency_deleted = client.store.remove_expired().await?;
    if idempotency_deleted != i.idempotency_rows / 5 + 3240 {
        return Err(failed(
            "native idempotency deletion differs from staged cohort",
        ));
    }
    for identity in 0..i.idempotency_rows / 2 {
        if identity >= i.idempotency_rows / 5 {
            sqlx::query("UPDATE http_idempotency_records SET expires_at=now()-interval '1 second' WHERE scope_key=$1")
                .bind(digest(i.seed,1,identity,0).as_slice()).execute(&client.pool).await?;
        }
        client
            .idempotency(
                i.seed,
                identity,
                generation,
                body_size(identity + generation),
                if identity < i.idempotency_rows / 5 {
                    0
                } else {
                    4
                },
            )
            .await?;
    }
    for (index, identity) in replacement_identities().take(8100).enumerate() {
        if index >= 3240 {
            sqlx::query("UPDATE http_idempotency_records SET expires_at=now()-interval '1 second' WHERE scope_key=$1")
                .bind(digest(i.seed,1,identity,0).as_slice()).execute(&client.pool).await?;
        }
        client
            .idempotency(
                i.seed,
                identity,
                generation,
                1024,
                if index < 3240 { 0 } else { 4 },
            )
            .await?;
    }
    // Retire only completed job identities; all failed identities remain held.
    let jobs_retired = in_tx(&client.pool, async |tx| -> Result<u64> {
        let retired = sqlx::query("UPDATE sustained_admissions SET retired=true WHERE family='jobs' AND identity IN (SELECT identity FROM sustained_admissions WHERE family='jobs' AND NOT retired AND terminal<>'failed' AND generation=$1 ORDER BY logical_id LIMIT $2)")
            .bind(i64::try_from(generation-1)?).bind(i64::try_from(i.jobs_rows/5)?).execute(&mut *tx).await?.rows_affected();
        sqlx::query("UPDATE background_jobs j SET finished_at=now()-interval '25 hours' FROM sustained_admissions a WHERE a.family='jobs' AND a.retired AND j.id::text=convert_from(a.identity,'UTF8') AND j.state='completed'")
            .execute(&mut *tx).await?;
        Ok(retired)
    }).await?;
    if jobs_retired != i.jobs_rows / 5 {
        return Err(failed(
            "native jobs retirement missed the declared logical cohort",
        ));
    }
    let removed = worker(client.pool.clone())?.remove_expired().await?;
    // A live worker retention loop may have deleted the same staged cohort.
    // Its absence is independently checked; confirmed counts stay separate.
    let retired_present: i64 = sqlx::query_scalar("SELECT count(*) FROM sustained_admissions a JOIN background_jobs j ON j.id::text=convert_from(a.identity,'UTF8') WHERE a.family='jobs' AND a.retired")
        .fetch_one(&client.pool).await?;
    if retired_present != 0 {
        return Err(failed("native jobs cleanup left staged completed cohort"));
    }
    for identity in 0..i.jobs_rows / 2 {
        client
            .enqueue(2000_u64.saturating_sub(i.jobs_rows) + identity, generation)
            .await?;
    }
    Ok(
        serde_json::json!({"idempotency_deletions_confirmed":idempotency_deleted,"jobs_deletions_confirmed":removed,"jobs_retired":jobs_retired,"retired_jobs_remaining":retired_present,"replacement_fixture_mutated":8100,"replacement_fixture_deleted_reinserted":3240}),
    )
}

pub(super) async fn relation_snapshot(pool: &PgPool) -> Result<serde_json::Value> {
    let rows = sqlx::query("SELECT relname, pg_total_relation_size(relid) AS total_bytes, pg_relation_size(relid) AS heap_bytes, pg_indexes_size(relid) AS index_bytes, pg_table_size(relid)-pg_relation_size(relid) AS toast_and_auxiliary_bytes, n_tup_ins, n_tup_upd, n_tup_del, n_dead_tup, vacuum_count, autovacuum_count, analyze_count, autoanalyze_count FROM pg_stat_user_tables WHERE relname IN ('background_jobs','http_idempotency_records','webhook_receipts') ORDER BY relname").fetch_all(pool).await?;
    let mut result = Vec::new();
    for row in rows {
        result.push(serde_json::json!({"relation": row.try_get::<String,_>("relname")?, "total_bytes": row.try_get::<i64,_>("total_bytes")?,
            "heap_bytes":row.try_get::<i64,_>("heap_bytes")?,"index_bytes":row.try_get::<i64,_>("index_bytes")?,"toast_and_auxiliary_bytes":row.try_get::<i64,_>("toast_and_auxiliary_bytes")?,
            "insert": row.try_get::<i64,_>("n_tup_ins")?, "update": row.try_get::<i64,_>("n_tup_upd")?, "delete": row.try_get::<i64,_>("n_tup_del")?,
            "estimated_dead": row.try_get::<i64,_>("n_dead_tup")?, "vacuum": row.try_get::<i64,_>("vacuum_count")?, "autovacuum": row.try_get::<i64,_>("autovacuum_count")?,
            "analyze": row.try_get::<i64,_>("analyze_count")?, "autoanalyze": row.try_get::<i64,_>("autoanalyze_count")?}));
    }
    Ok(serde_json::Value::Array(result))
}

/// Seed disposable retention cohorts before physical qualification. Extending
/// a clone for the composed proof inserts only additional fixture identities.
pub(super) async fn prepare_eligibility(pool: &PgPool, seconds: u64) -> Result<()> {
    if seconds == 0 || seconds > 1200 {
        return Err(failed("eligibility timeline outside accepted maximum"));
    }
    for (label, backlog) in [("steady", 0_i64), ("catchup", 10_000_i64)] {
        let count = backlog
            + i64::try_from(
                seconds
                    .checked_mul(20)
                    .ok_or_else(|| failed("eligibility count overflow"))?,
            )?;
        for statement in [
            "INSERT INTO background_jobs (id,kind,payload,unique_key,state,not_before,finished_at) SELECT md5($1 || ':' || n)::uuid,'lab.retained',jsonb_build_object('bytes',repeat('x',1024)),$1 || ':' || n,'completed',now(),now() FROM generate_series(1,$2) n ON CONFLICT(id) DO NOTHING",
            "INSERT INTO http_idempotency_records (scope_key,fingerprint,status,headers,body,issuer,caller_kind,caller_value,expires_at) SELECT sha256(convert_to($1 || ':' || n,'UTF8')),sha256(convert_to($1 || ':' || n,'UTF8')),201,ARRAY[]::http_idempotency_header_pair[],convert_to(repeat('x',1024),'UTF8'),'https://lab.invalid','subject',$1,now()+interval '1 day' FROM generate_series(1,$2) n ON CONFLICT(scope_key) DO NOTHING",
            "INSERT INTO webhook_receipts (endpoint_id,message_id,received_at) SELECT 'lab.retained',convert_to($1 || ':' || n,'UTF8'),now() FROM generate_series(1,$2) n ON CONFLICT(endpoint_id,message_id) DO NOTHING",
        ] {
            sqlx::query(statement)
                .bind(label)
                .bind(count)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}

/// Timestamp shifts are logged fixture actions at the segment boundary. There
/// is no fixture INSERT or DELETE in a measured segment; native owners delete.
pub(super) async fn stage_eligibility(pool: &PgPool, catch_up: bool) -> Result<()> {
    let backlog = if catch_up { 10_000_i64 } else { 0 };
    let label = if catch_up { "catchup" } else { "steady" };
    for statement in [
        "UPDATE background_jobs SET finished_at=statement_timestamp()-interval '24 hours'+CASE WHEN split_part(unique_key,':',2)::bigint <= $2 THEN interval '-300 seconds' ELSE ((split_part(unique_key,':',2)::bigint-$2-1)/20)::double precision*interval '1 second' END WHERE kind='lab.retained' AND unique_key LIKE $1 || ':%'",
        "UPDATE http_idempotency_records r SET expires_at=statement_timestamp()+CASE WHEN n <= $2 THEN interval '-300 seconds' ELSE ((n-$2-1)/20)::double precision*interval '1 second' END FROM generate_series(1,34000) n WHERE caller_value=$1 AND scope_key=sha256(convert_to($1 || ':' || n,'UTF8'))",
        "UPDATE webhook_receipts SET received_at=statement_timestamp()-interval '14 days'+CASE WHEN split_part(convert_from(message_id,'UTF8'),':',2)::bigint <= $2 THEN interval '-300 seconds' ELSE ((split_part(convert_from(message_id,'UTF8'),':',2)::bigint-$2-1)/20)::double precision*interval '1 second' END WHERE endpoint_id='lab.retained' AND convert_from(message_id,'UTF8') LIKE $1 || ':%'",
    ] {
        sqlx::query(statement)
            .bind(label)
            .bind(backlog)
            .execute(pool)
            .await?;
    }
    Ok(())
}

pub(super) async fn inventory(pool: &PgPool) -> Result<serde_json::Value> {
    let row = sqlx::query("SELECT (SELECT count(*) FROM background_jobs WHERE state='completed' AND finished_at <= now()-interval '24 hours') AS jobs, (SELECT count(*) FROM http_idempotency_records WHERE expires_at <= now()) AS idempotency, (SELECT count(*) FROM webhook_receipts WHERE received_at < now()-interval '14 days') AS receipts, (SELECT count(*) FROM background_jobs WHERE state='failed') AS failed, (SELECT count(*) FROM background_jobs WHERE unique_key LIKE 'catchup:%' AND split_part(unique_key,':',2)::bigint <= 10000) AS catchup_jobs, (SELECT count(*) FROM http_idempotency_records WHERE caller_value='catchup' AND scope_key IN (SELECT sha256(convert_to('catchup:' || n,'UTF8')) FROM generate_series(1,10000) n)) AS catchup_idempotency, (SELECT count(*) FROM webhook_receipts WHERE endpoint_id='lab.retained' AND convert_from(message_id,'UTF8') LIKE 'catchup:%' AND substring(convert_from(message_id,'UTF8') from '^catchup:([0-9]+)$')::bigint <=10000) AS catchup_receipts, (SELECT coalesce(extract(epoch FROM now()-min(finished_at)-interval '24 hours'),0)::float8 FROM background_jobs WHERE state='completed' AND finished_at<=now()-interval '24 hours') AS jobs_delay, (SELECT coalesce(extract(epoch FROM now()-min(expires_at)),0)::float8 FROM http_idempotency_records WHERE expires_at<=now()) AS idempotency_delay, (SELECT coalesce(extract(epoch FROM now()-min(received_at)-interval '14 days'),0)::float8 FROM webhook_receipts WHERE received_at<now()-interval '14 days') AS receipts_delay").fetch_one(pool).await?;
    Ok(serde_json::json!({
        "eligible": {"jobs":row.try_get::<i64,_>("jobs")?,"idempotency":row.try_get::<i64,_>("idempotency")?,"receipts":row.try_get::<i64,_>("receipts")?},
        "catchup": {"jobs":row.try_get::<i64,_>("catchup_jobs")?,"idempotency":row.try_get::<i64,_>("catchup_idempotency")?,"receipts":row.try_get::<i64,_>("catchup_receipts")?},
        "delay": {"jobs":row.try_get::<f64,_>("jobs_delay")?,"idempotency":row.try_get::<f64,_>("idempotency_delay")?,"receipts":row.try_get::<f64,_>("receipts_delay")?},
        "failed":row.try_get::<i64,_>("failed")?
    }))
}

/// Shared with the entry owner for identical per-clone configuration readback.
pub(super) const DATABASE_CONFIG_SQL: &str = r#"SELECT jsonb_build_object('settings',(SELECT jsonb_object_agg(name,setting) FROM pg_settings WHERE name IN ('server_version_num','max_connections','shared_buffers','effective_cache_size','work_mem','maintenance_work_mem','autovacuum','autovacuum_max_workers','autovacuum_worker_slots','autovacuum_vacuum_threshold','autovacuum_vacuum_scale_factor','autovacuum_analyze_threshold','autovacuum_analyze_scale_factor','autovacuum_vacuum_cost_delay','autovacuum_vacuum_cost_limit','autovacuum_naptime','autovacuum_freeze_max_age','vacuum_cost_delay','vacuum_cost_limit','track_counts','track_io_timing','fsync','synchronous_commit','full_page_writes','wal_level','checkpoint_timeout','checkpoint_completion_target','max_wal_size','min_wal_size','jit','default_statistics_target','random_page_cost','seq_page_cost','effective_io_concurrency','maintenance_io_concurrency')),'tables',(SELECT jsonb_object_agg(c.relname,jsonb_build_object('options',coalesce((SELECT jsonb_agg(option ORDER BY option) FROM unnest(c.reloptions) option),'[]'::jsonb),'toast_options',coalesce((SELECT jsonb_agg(option ORDER BY option) FROM unnest(t.reloptions) option),'[]'::jsonb))) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace LEFT JOIN pg_class t ON t.oid=c.reltoastrelid WHERE n.nspname='public' AND c.relname IN ('background_jobs','http_idempotency_records','webhook_receipts')),'schema_sha256',encode(sha256(convert_to((WITH owners AS (SELECT c.oid,c.relname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname IN ('background_jobs','http_idempotency_records','webhook_receipts')), definitions AS (SELECT o.relname || ':column:' || a.attnum || ':' || a.attname || ':' || format_type(a.atttypid,a.atttypmod) || ':' || a.attnotnull || ':' || coalesce(pg_get_expr(d.adbin,d.adrelid),'') AS definition FROM owners o JOIN pg_attribute a ON a.attrelid=o.oid LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum WHERE a.attnum>0 AND NOT a.attisdropped UNION ALL SELECT o.relname || ':constraint:' || c.conname || ':' || pg_get_constraintdef(c.oid,true) FROM owners o JOIN pg_constraint c ON c.conrelid=o.oid UNION ALL SELECT o.relname || ':index:' || pg_get_indexdef(i.indexrelid) FROM owners o JOIN pg_index i ON i.indrelid=o.oid) SELECT string_agg(definition,chr(10) ORDER BY definition) FROM definitions),'UTF8')),'hex'))"#;

pub(super) async fn database_sample(pool: &PgPool) -> Result<serde_json::Value> {
    // PG18-only laboratory observations remain outside the production providers.
    // Each SELECT gets a fresh short snapshot; no transaction spans sleep.
    let buffers = sqlx::query("SELECT coalesce(sum(coalesce(heap_blks_read,0)+coalesce(idx_blks_read,0)+coalesce(toast_blks_read,0)+coalesce(tidx_blks_read,0)),0)::bigint AS reads, coalesce(sum(coalesce(heap_blks_hit,0)+coalesce(idx_blks_hit,0)+coalesce(toast_blks_hit,0)+coalesce(tidx_blks_hit,0)),0)::bigint AS hits FROM pg_statio_user_tables WHERE relname IN ('background_jobs','http_idempotency_records','webhook_receipts')").fetch_one(pool).await?;
    let activity: serde_json::Value = sqlx::query_scalar("SELECT coalesce(jsonb_agg(t),'[]'::jsonb) FROM (SELECT coalesce(wait_event_type,'running') AS wait_class,count(*) AS sessions,max(extract(epoch FROM clock_timestamp()-xact_start)) AS oldest_transaction_seconds FROM pg_stat_activity WHERE datname=current_database() GROUP BY wait_event_type) t").fetch_one(pool).await?;
    // This SQL joins two source literals; no runtime value or identifier enters it.
    let progress_sql = format!(
        "SELECT jsonb_build_object('vacuum',(SELECT count(*) FROM pg_stat_progress_vacuum WHERE datname=current_database()),'analyze',(SELECT count(*) FROM pg_stat_progress_analyze WHERE datname=current_database()),'wal',(SELECT to_jsonb(w) FROM pg_stat_wal w),'checkpointer',(SELECT to_jsonb(c) FROM pg_stat_checkpointer c),'database',(SELECT to_jsonb(d)-'datname' FROM pg_stat_database d WHERE datname=current_database()),'database_config',({DATABASE_CONFIG_SQL}))"
    );
    let progress: serde_json::Value =
        sqlx::query_scalar(sqlx::AssertSqlSafe(progress_sql.as_str()))
            .fetch_one(pool)
            .await?;
    Ok(
        serde_json::json!({"buffer_reads":buffers.try_get::<i64,_>("reads")?,"buffer_hits":buffers.try_get::<i64,_>("hits")?,"activity":activity,"database_config":progress["database_config"],"progress":progress}),
    )
}

pub(super) async fn vacuum(pool: &PgPool) -> Result<()> {
    let mut connection = infra_postgres::acquire(pool, "laboratory ordinary vacuum").await?;
    sqlx::query("SET statement_timeout='60s'")
        .execute(&mut *connection)
        .await?;
    let result = async {
        for table in [
            "VACUUM (ANALYZE) background_jobs",
            "VACUUM (ANALYZE) http_idempotency_records",
            "VACUUM (ANALYZE) webhook_receipts",
        ] {
            sqlx::query(table).execute(&mut *connection).await?;
        }
        Ok::<_, sqlx::Error>(())
    }
    .await;
    // This dedicated control connection is not returned with a widened budget.
    connection.close().await?;
    result?;
    Ok(())
}

pub(super) async fn stage_replacements(pool: &PgPool, offset: u64) -> Result<()> {
    let first = i64::try_from(
        offset
            .checked_mul(100)
            .ok_or_else(|| failed("replacement offset overflow"))?,
    )?;
    // Independent expected expiry is retained even if a future live row is
    // wrongly deleted. All timestamp staging commits before the role barrier.
    in_tx(pool, async |tx| -> Result<()> {
        sqlx::query("UPDATE sustained_admissions SET eligible_at=statement_timestamp()+((logical_id-20000000-$1)::double precision/100.0)*interval '1 second' WHERE family='idempotency' AND logical_id BETWEEN 20000000+$1 AND 20119999 AND generation<=3")
            .bind(first).execute(&mut *tx).await?;
        sqlx::query("UPDATE http_idempotency_records r SET expires_at=a.eligible_at FROM (SELECT DISTINCT ON(identity) identity,eligible_at FROM sustained_admissions WHERE family='idempotency' AND logical_id BETWEEN 20000000+$1 AND 20119999 AND generation<=3 ORDER BY identity,generation DESC) a WHERE r.caller_value=convert_from(a.identity,'UTF8')")
            .bind(first).execute(&mut *tx).await?;
        Ok(())
    }).await
}

/// Existing later-batch rejection fixture, restricted to the composed proof.
/// The caller records this disturbance separately from capacity observations.
pub(super) async fn composed_failure(pool: &PgPool, armed: bool) -> Result<serde_json::Value> {
    if armed {
        in_tx(pool, async |tx| -> Result<()> {
            sqlx::query("CREATE FUNCTION sustained_reject_last_cleanup_row() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'composed cleanup rejection'; END; $$")
                .execute(&mut *tx).await?;
            sqlx::query("CREATE TRIGGER sustained_reject_last_cleanup_row BEFORE DELETE ON http_idempotency_records FOR EACH ROW WHEN (OLD.scope_key=sha256(convert_to('composed_later_failure:501','UTF8'))) EXECUTE FUNCTION sustained_reject_last_cleanup_row()")
                .execute(&mut *tx).await?;
            sqlx::query("INSERT INTO http_idempotency_records (scope_key,fingerprint,status,headers,body,issuer,caller_kind,caller_value,expires_at) SELECT sha256(convert_to('composed_later_failure:' || n,'UTF8')),sha256(convert_to('composed_later_failure:' || n,'UTF8')),201,ARRAY[]::http_idempotency_header_pair[],convert_to(repeat('x',1024),'UTF8'),'https://lab.invalid','subject','composed_later_failure',statement_timestamp()-CASE WHEN n<=500 THEN interval '100 hours' ELSE interval '1 hour' END FROM generate_series(1,501) n")
                .execute(&mut *tx).await?;
            Ok(())
        }).await?;
        return Ok(
            serde_json::json!({"armed":true,"fixture_inserted":501,"cohort":composed_failure_inventory(pool).await?}),
        );
    }
    let before = composed_failure_inventory(pool).await?;
    in_tx(pool, async |tx| -> Result<()> {
        sqlx::query("DROP TRIGGER sustained_reject_last_cleanup_row ON http_idempotency_records")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DROP FUNCTION sustained_reject_last_cleanup_row()")
            .execute(&mut *tx)
            .await?;
        Ok(())
    })
    .await?;
    Ok(
        serde_json::json!({"armed":false,"cohort_before_release":before,"cohort_after_release":composed_failure_inventory(pool).await?}),
    )
}

pub(super) async fn composed_failure_inventory(pool: &PgPool) -> Result<serde_json::Value> {
    let row = sqlx::query("SELECT count(*) FILTER (WHERE scope_key<>sha256(convert_to('composed_later_failure:501','UTF8'))) AS first_batch_remaining,count(*) FILTER (WHERE scope_key=sha256(convert_to('composed_later_failure:501','UTF8'))) AS rejected_remaining FROM http_idempotency_records WHERE caller_value='composed_later_failure'")
        .fetch_one(pool).await?;
    Ok(
        serde_json::json!({"first_batch_remaining":row.try_get::<i64,_>("first_batch_remaining")?,"rejected_remaining":row.try_get::<i64,_>("rejected_remaining")?}),
    )
}

pub(super) async fn protected_jobs(pool: &PgPool) -> Result<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT id::text FROM background_jobs WHERE state='failed' ORDER BY id")
            .fetch_all(pool)
            .await?,
    )
}

pub(super) async fn recovery(
    client: &Client,
    live: u64,
    protected_jobs: &[String],
    generation: u64,
) -> Result<()> {
    if live != client.inventory.live || generation != client.inventory.generation {
        return Err(failed("recovery inventory differs from frozen manifest"));
    }
    verify_effects(&client.pool).await?;
    // Expectations were recorded at native admission. Read full bodies through
    // the store, including new/replaced identities from every process role.
    let records = sqlx::query("SELECT DISTINCT ON (identity) identity,seed,logical_id,generation,body_bytes,coalesce(eligible_at<=statement_timestamp(),false) AS eligible FROM sustained_admissions WHERE family='idempotency' ORDER BY identity,generation DESC")
        .fetch_all(&client.pool).await?;
    for row in records {
        let identity = u64::try_from(row.try_get::<i64, _>("logical_id")?)?;
        let generation = u64::try_from(row.try_get::<i64, _>("generation")?)?;
        // Unused replacement fixtures may be legitimately retired once their
        // planned admission was missed; the operation journal retains that miss.
        if (REPLACEMENT_BASE..REPLACEMENT_BASE + 120_000).contains(&identity)
            && generation <= client.inventory.generation
            && row.try_get::<bool, _>("eligible")?
        {
            continue;
        }
        client
            .idempotency(
                u64::try_from(row.try_get::<i64, _>("seed")?)?,
                identity,
                generation,
                usize::try_from(row.try_get::<i64, _>("body_bytes")?)?,
                1,
            )
            .await?;
    }
    let missing_receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM sustained_admissions a LEFT JOIN webhook_receipts r ON r.endpoint_id=$1 AND r.message_id=a.identity WHERE a.family='webhook' AND NOT a.retired AND r.message_id IS NULL")
        .bind(ENDPOINT).fetch_one(&client.pool).await?;
    if missing_receipts != 0 {
        return Err(failed("live admitted receipt disappeared"));
    }
    for id in protected_jobs {
        let request = Inspection::one(id)?;
        let InspectionResult::One {
            item: Some(item), ..
        } = in_tx(&client.pool, async |tx| {
            operator::inspect(tx, &request).await
        })
        .await?
        else {
            return Err(failed("retained failed identity disappeared"));
        };
        if item.state != operator::JobState::Failed {
            return Err(failed("retained failed identity changed custody"));
        }
    }
    Ok(())
}

pub(super) async fn precondition(client: &Client, live: u64) -> Result<serde_json::Value> {
    let start = tokio::time::Instant::now();
    let before = relation_snapshot(&client.pool).await?;
    let churn_start = tokio::time::Instant::now();
    let i = client.inventory;
    if i.generation != 3 {
        return Err(failed(
            "accepted preconditioning requires exactly three rounds",
        ));
    }
    let mut replayed = 0_u64;
    let mut rounds = Vec::new();
    for generation in 1..=i.generation {
        let native = native_churn(client, live, generation).await?;
        for identity in 0..i.receipt_rows / 2 {
            let old = format!("{}-{identity}-{}", i.seed, generation - 1);
            in_tx(&client.pool, async |tx| -> Result<()> {
                sqlx::query("UPDATE webhook_receipts SET received_at=now()-interval '15 days' WHERE endpoint_id=$1 AND message_id=$2")
                    .bind(ENDPOINT).bind(old.as_bytes()).execute(&mut *tx).await?;
                sqlx::query("UPDATE sustained_admissions SET retired=true WHERE family='webhook' AND identity=$1")
                    .bind(old.as_bytes()).execute(&mut *tx).await?;
                Ok(())
            }).await?;
        }
        let receipts_deleted = client.receiver.remove_expired().await?;
        if receipts_deleted != i.receipt_rows / 2 {
            return Err(failed(
                "receipt churn deletion count differs from staged cohort",
            ));
        }
        for identity in 0..i.receipt_rows / 2 {
            client.webhook(i.seed, identity, generation, false).await?;
        }
        await_settled(client).await?;
        client.inspect_or_redrive(true, generation).await?;
        await_settled(client).await?;
        let until = churn_start + Duration::from_secs(generation * 300);
        let mut next = tokio::time::Instant::now();
        while tokio::time::Instant::now() < until {
            client
                .idempotency(i.seed, WIDE_BASE + (replayed % live), 0, WIDE_BYTES, 1)
                .await?;
            replayed += 1;
            next += Duration::from_nanos(15_625_000);
            tokio::time::sleep_until(next).await;
        }
        rounds.push(serde_json::json!({"generation":generation,"native":native,"relations":relation_snapshot(&client.pool).await?,"receipt_deletions_confirmed":receipts_deleted}));
    }
    await_settled(client).await?;
    Ok(
        serde_json::json!({"rounds":rounds,"before":before,"idempotency_replaced_per_round":i.idempotency_rows/2,"idempotency_deleted_reinserted_per_round":i.idempotency_rows/5,"receipt_deleted_readmitted_per_round":i.receipt_rows/2,"jobs_enqueued_per_round":i.jobs_rows/2,"wide_native_replays":replayed,"native_elapsed_ms":churn_start.elapsed().as_millis(),"total_elapsed_ms":start.elapsed().as_millis(),"frozen_generation":i.generation,"inventory":i,"fixture_actions":"timestamp shifts and unhandled seed failures; eligibility cohorts prepared separately"}),
    )
}
