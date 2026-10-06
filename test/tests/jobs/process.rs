//! Process proof of the jobs worker on the test-only fixture binary against
//! real PostgreSQL and, in retained outbox profiles, a real `JetStream` source
//! stream. Refusals exit 1, a ready worker runs a committed job and exits 0 on
//! SIGTERM with its attempt metrics on the diagnostics listener, and an attempt
//! that outlives a short drain exits 3 with its job released for an immediate
//! claim. Each case gets its own database from `#[sqlx::test]`.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
// template:begin outbox:test-jobs-process-nats-imports
use std::sync::atomic::{AtomicU64, Ordering};
// template:end outbox:test-jobs-process-nats-imports
use std::sync::mpsc;
use std::time::{Duration, Instant};

// template:begin outbox:test-jobs-process-nats-imports-2
use async_nats::jetstream::{self, stream};
// template:end outbox:test-jobs-process-nats-imports-2
use infra_jobs::{EnqueueOptions, Enqueued, JobKind, enqueue};
use infra_postgres::{PgPool, TxError, in_tx};
use integration_tests::jobs::{CREATE_PROBE_ATTEMPTS, Probe, ProbeAction};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use sqlx::Row;
use url::Url;

const RECORD_BOUND: Duration = Duration::from_secs(20);
const EXIT_BOUND: Duration = Duration::from_secs(20);
const READY_BOUND: Duration = Duration::from_secs(5);
const METRICS_BOUND: Duration = Duration::from_secs(5);
const JOB_BOUND: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(20);
const DB_POLL: Duration = Duration::from_millis(50);

// template:begin outbox:test-jobs-process-nats-fixture
static NEXT_NATS_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct NatsFixture {
    jetstream: jetstream::Context,
    stream: String,
    url: String,
}

impl NatsFixture {
    async fn create() -> Self {
        let id = NEXT_NATS_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let suffix = format!("{}_{}", std::process::id(), id);
        let stream = format!("TEST_OUTBOX_PROCESS_{suffix}");
        let subject = format!("test.outbox.process.{suffix}.>");
        let url = nats_url();
        let client = async_nats::connect(&url)
            .await
            .expect("NATS_URL must point to the JetStream broker selected for this suite");
        let jetstream = jetstream::new(client);
        jetstream
            .create_stream(stream::Config {
                name: stream.clone(),
                subjects: vec![subject],
                max_messages: 10,
                max_message_size: 1024 + 8 * 1024,
                discard: stream::DiscardPolicy::New,
                ..Default::default()
            })
            .await
            .expect("test fixture source stream must be created by the NATS test administrator");
        Self {
            jetstream,
            stream,
            url,
        }
    }

    async fn cleanup(self) {
        self.jetstream
            .delete_stream(&self.stream)
            .await
            .expect("test fixture source stream must be removable");
    }
}

fn nats_url() -> String {
    std::env::var("NATS_URL").expect(
        "NATS_URL is required; run this suite through the selected messaging integration runner",
    )
}
// template:end outbox:test-jobs-process-nats-fixture

struct Worker {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Worker {
    fn spawn(
        database_url: &str,
        // template:begin outbox:test-jobs-process-nats-fixture-parameter
        nats: &NatsFixture,
        // template:end outbox:test-jobs-process-nats-fixture-parameter
        env: &[(&str, &str)],
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jobs-worker-fixture"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("APP__HTTP__ADDR", "127.0.0.1:0")
            .env("APP__OBSERVABILITY__METRICS__ADDR", "127.0.0.1:0")
            .env("APP__LOG__FORMAT", "json")
            .env("APP__POSTGRES__ENABLED", "true")
            .env("APP__POSTGRES__DSN", database_url)
            // template:begin outbox:test-jobs-process-nats-environment
            .env("APP__POSTGRES__MAX_CONNECTIONS", "6")
            .env("APP__APP__ENV", "local")
            .env("APP__MESSAGING__URLS", &nats.url)
            .env("APP__MESSAGING__SOURCE_STREAM", &nats.stream)
            .env("APP__MESSAGING__MAX_PAYLOAD_BYTES", "1 KiB")
            .env("APP__MESSAGING__ALLOW_PLAINTEXT", "true")
            .env("APP__MESSAGING__ALLOW_UNAUTHENTICATED", "true")
            // template:end outbox:test-jobs-process-nats-environment
            .envs(env.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn jobs-worker-fixture");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
    }

    fn await_record(&self, message: &str) -> serde_json::Value {
        self.await_record_matching(message, |_| true)
    }

    fn await_record_matching(
        &self,
        message: &str,
        matches: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + RECORD_BOUND;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(remaining).unwrap_or_else(|_| {
                panic!("no {message:?} record before the deadline");
            });
            let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
                panic!("stdout must be one JSON object per line, got {line:?}");
            };
            if record["message"] == message && matches(&record) {
                return record;
            }
        }
    }

    fn terminate(&self) {
        kill(
            Pid::from_raw(self.child.id().cast_signed()),
            Signal::SIGTERM,
        )
        .expect("send SIGTERM");
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
    )]
    fn wait(mut self) -> (Option<i32>, String) {
        let deadline = Instant::now() + EXIT_BOUND;
        let code = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) if Instant::now() >= deadline => {
                    reap(&mut self.child);
                    let stderr = read_stderr(&mut self.child);
                    panic!("worker did not exit within {EXIT_BOUND:?}; stderr: {stderr}");
                }
                Ok(None) => std::thread::sleep(POLL),
                Err(err) => panic!("wait for the worker: {err}"),
            }
        };
        (code, read_stderr(&mut self.child))
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            reap(&mut self.child);
        }
    }
}

fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_stderr(child: &mut Child) -> String {
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
    }
    stderr
}

fn get(url: &str) -> Result<(u16, String), ureq::Error> {
    match ureq::get(url).call() {
        Ok(mut response) => {
            let status = response.status().as_u16();
            let body = response.body_mut().read_to_string().unwrap_or_default();
            Ok((status, body))
        }
        Err(ureq::Error::StatusCode(status)) => Ok((status, String::new())),
        Err(err) => Err(err),
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
)]
fn poll_until(url: &str, want_status: u16, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Ok((status, _)) = get(url)
            && status == want_status
        {
            return true;
        }
        std::thread::sleep(POLL);
    }
    false
}

fn listener_addr(worker: &Worker, message: &str) -> String {
    worker.await_record(message)["addr"]
        .as_str()
        .unwrap_or_else(|| panic!("{message} record has no addr"))
        .to_owned()
}

fn completed_probe_metrics(body: &str) -> bool {
    let mut attempts = false;
    let mut duration = false;
    for line in body.lines() {
        if line.starts_with("jobs_attempts_total{")
            && line.contains("kind=\"test.probe\"")
            && line.contains("outcome=\"completed\"")
        {
            attempts = true;
        }
        if line.starts_with("jobs_attempt_duration_seconds_bucket{")
            && line.contains("kind=\"test.probe\"")
            && line.contains("le=\"3600\"")
        {
            duration = true;
        }
    }
    attempts
        && duration
        && ["jobs_owned_attempts 0", "jobs_completion_memberships 0"]
            .iter()
            .all(|expected| body.lines().any(|line| line == *expected))
}

#[allow(
    clippy::disallowed_methods,
    reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
)]
fn await_completed_probe_metrics(url: &str) {
    let deadline = Instant::now() + METRICS_BOUND;
    let mut scraped = String::new();
    while Instant::now() < deadline {
        if let Ok((status, body)) = get(url) {
            scraped = body;
            if status == 200 && completed_probe_metrics(&scraped) {
                return;
            }
        }
        std::thread::sleep(POLL);
    }
    panic!("metrics never showed the completed probe attempt:\n{scraped}");
}

fn capped_scheduled_probe_sample(body: &str, failed_kinds: &[&str]) -> bool {
    let mut scheduled = false;
    let mut timestamp = false;
    for line in body.lines() {
        let value = line
            .split_ascii_whitespace()
            .last()
            .and_then(|text| text.parse::<f64>().ok());
        if line.starts_with("jobs_live_jobs{")
            && line.contains("kind=\"test.probe\"")
            && line.contains("state=\"scheduled\"")
            && value == Some(1_000.0)
        {
            scheduled = true;
        }
        if line.starts_with("jobs_observation_timestamp_seconds ")
            && value.is_some_and(|seen| seen > 0.0)
        {
            timestamp = true;
        }
    }
    let failed: Vec<_> = body
        .lines()
        .filter(|line| line.starts_with("jobs_failed_jobs{"))
        .collect();
    scheduled
        && timestamp
        && failed.len() == failed_kinds.len()
        && failed_kinds.iter().all(|kind| {
            let label = format!("kind=\"{kind}\"");
            failed.iter().any(|line| {
                line.contains(&label)
                    && line
                        .split_ascii_whitespace()
                        .last()
                        .and_then(|value| value.parse::<f64>().ok())
                        == Some(1_000.0)
            })
        })
}

#[allow(
    clippy::disallowed_methods,
    reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
)]
fn await_capped_scheduled_probe_sample(url: &str, failed_kinds: &[&str]) {
    let deadline = Instant::now() + METRICS_BOUND;
    let mut scraped = String::new();
    while Instant::now() < deadline {
        if let Ok((status, body)) = get(url) {
            scraped = body;
            if status == 200 && capped_scheduled_probe_sample(&scraped, failed_kinds) {
                return;
            }
        }
        std::thread::sleep(POLL);
    }
    panic!("metrics never showed the capped scheduled sample:\n{scraped}");
}

/// Whether the worker counted a failure of `operation`.
fn operation_failed(body: &str, operation: &str) -> bool {
    let label = format!("operation=\"{operation}\"");
    body.lines().any(|line| {
        line.starts_with("jobs_worker_operation_failures_total{")
            && line.contains(&label)
            && line
                .split_ascii_whitespace()
                .last()
                .and_then(|value| value.parse::<f64>().ok())
                .is_some_and(|value| value >= 1.0)
    })
}

#[allow(
    clippy::disallowed_methods,
    reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
)]
fn await_failed_operation(url: &str, operation: &str) {
    let deadline = Instant::now() + METRICS_BOUND;
    let mut scraped = String::new();
    while Instant::now() < deadline {
        if let Ok((status, body)) = get(url) {
            scraped = body;
            if status == 200 && operation_failed(&scraped, operation) {
                return;
            }
        }
        std::thread::sleep(POLL);
    }
    panic!("metrics never counted a failed {operation}:\n{scraped}");
}

/// The worker's wake `LISTEN` session in this test's database, once it listens.
async fn wake_listener(pool: &PgPool, except: Option<i32>) -> i32 {
    tokio::time::timeout(JOB_BOUND, async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_stat_activity \
                 WHERE datname = current_database() AND query LIKE 'LISTEN%' \
                   AND pid IS DISTINCT FROM $1",
            )
            .bind(except)
            .fetch_optional(pool)
            .await
            .expect("the listener observation");
            if let Some(pid) = pid {
                return pid;
            }
            tokio::time::sleep(DB_POLL).await;
        }
    })
    .await
    .expect("the worker listens for wake notifications")
}

fn queue_observation(body: &str) -> std::collections::BTreeMap<String, u64> {
    body.lines()
        .filter(|line| {
            line.starts_with("jobs_live_jobs{")
                || line.starts_with("jobs_failed_jobs{")
                || line.starts_with("jobs_oldest_available_age_seconds{")
                || line.starts_with("jobs_observation_timestamp_seconds ")
        })
        .map(|line| {
            let (key, value) = line.rsplit_once(' ').expect("metric name and value");
            (
                key.to_owned(),
                value.parse::<f64>().expect("gauge value").to_bits(),
            )
        })
        .collect()
}

#[allow(
    clippy::disallowed_methods,
    reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
)]
fn await_failed_sample_with_last_good_values(
    url: &str,
    before: &std::collections::BTreeMap<String, u64>,
) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut scraped = String::new();
    while Instant::now() < deadline {
        if let Ok((status, body)) = get(url) {
            scraped = body;
            if status == 200
                && operation_failed(&scraped, "sample")
                && queue_observation(&scraped) == *before
            {
                return;
            }
        }
        std::thread::sleep(POLL);
    }
    panic!("metrics never retained the last good sample after a failure:\n{scraped}");
}

async fn child_database_url(pool: &PgPool) -> String {
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await
        .expect("current_database()");
    let raw = std::env::var(integration_tests::DATABASE_URL)
        .expect("DATABASE_URL must be set for integration tests");
    let mut url = Url::parse(&raw).expect("DATABASE_URL is a URL");
    url.set_path(&database);
    url.as_str().to_owned()
}

async fn prepare(pool: &PgPool) {
    sqlx::query(CREATE_PROBE_ATTEMPTS)
        .execute(pool)
        .await
        .expect("the probe table");
}

async fn enqueue_committed(pool: &PgPool, action: ProbeAction) -> String {
    let enqueued = in_tx(pool, async |tx| -> Result<_, TxError> {
        Ok(enqueue(tx, &Probe { action }, EnqueueOptions::default())
            .await
            .expect("enqueue"))
    })
    .await
    .expect("enqueue");
    let Enqueued::Created(id) = enqueued else {
        panic!("duplicate enqueue");
    };
    id.to_string()
}

async fn job_state(pool: &PgPool, id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM background_jobs WHERE id::text = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("job state")
}

async fn probe_attempts(pool: &PgPool, id: &str) -> Vec<i32> {
    sqlx::query_scalar("SELECT attempt FROM probe_attempts WHERE job_id = $1::uuid ORDER BY seq")
        .bind(id)
        .fetch_all(pool)
        .await
        .expect("probe attempts")
}

async fn wait_completed(pool: &PgPool, id: &str) {
    let finished = tokio::time::timeout(JOB_BOUND, async {
        loop {
            if job_state(pool, id).await == "completed" {
                return;
            }
            tokio::time::sleep(DB_POLL).await;
        }
    })
    .await;
    if finished.is_err() {
        let state = job_state(pool, id).await;
        panic!("job {id} did not complete within {JOB_BOUND:?}; state={state}");
    }
    let attempts = probe_attempts(pool, id).await;
    assert_eq!(attempts, [1], "probe attempts for {id}");
}

async fn wait_running(pool: &PgPool, id: &str) {
    let started = tokio::time::timeout(JOB_BOUND, async {
        loop {
            let running = job_state(pool, id).await == "running";
            let attempts = probe_attempts(pool, id).await;
            if running && attempts == [1] {
                return;
            }
            tokio::time::sleep(DB_POLL).await;
        }
    })
    .await;
    if started.is_err() {
        let state = job_state(pool, id).await;
        let attempts = probe_attempts(pool, id).await;
        panic!(
            "attempt was not in flight within {JOB_BOUND:?}; state={state} attempts={attempts:?}"
        );
    }
}

async fn assert_claimable(pool: &PgPool, id: &str) {
    let row = sqlx::query(
        "SELECT state, attempts, claim_expires_at IS NULL AS claim_cleared, \
         not_before <= now() AS due \
         FROM background_jobs WHERE id::text = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("the released job");
    let state: String = row.try_get("state").expect("state");
    let attempts: i16 = row.try_get("attempts").expect("attempts");
    let claim_cleared: bool = row.try_get("claim_cleared").expect("claim_expires_at");
    let due: bool = row.try_get("due").expect("not_before");
    assert_eq!(state, "pending", "a released job returns to pending");
    assert_eq!(attempts, 0, "a release gives the attempt unit back");
    assert!(claim_cleared, "claim_expires_at must be null");
    assert!(due, "not_before must be due on the database clock");
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM probe_attempts WHERE job_id = $1::uuid")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("probe attempt count");
    assert_eq!(rows, 1, "the in-flight attempt row remains");
}

fn assert_refused(worker: Worker, needle: &str) {
    let failure = worker.await_record("jobs worker failed");
    assert!(
        failure["error"]
            .as_str()
            .is_some_and(|error| error.contains(needle)),
        "{failure}"
    );
    let finishing = worker.await_record("shutdown_finishing");
    assert_eq!(finishing["logger_pending"], true);
    let (code, stderr) = worker.wait();
    assert_eq!(code, Some(1), "stderr: {stderr}");
    assert!(stderr.is_empty(), "no post-install fallback: {stderr}");
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn postgres_disabled_exits_1(pool: PgPool) {
    let database_url = child_database_url(&pool).await;
    // template:begin outbox:test-jobs-process-nats-fixture-use
    let nats = NatsFixture::create().await;
    // template:end outbox:test-jobs-process-nats-fixture-use
    let worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument
        &[("APP__POSTGRES__ENABLED", "false")],
    );
    assert_refused(
        worker,
        "postgres.enabled must be true to run the jobs worker",
    );
    // template:begin outbox:test-jobs-process-nats-fixture-cleanup
    nats.cleanup().await;
    // template:end outbox:test-jobs-process-nats-fixture-cleanup
}

#[sqlx::test(migrations = false)]
async fn missing_migration_history_exits_1_before_jobs_admission(pool: PgPool) {
    let database_url = child_database_url(&pool).await;
    // template:begin outbox:test-jobs-process-nats-fixture-use-2
    let nats = NatsFixture::create().await;
    // template:end outbox:test-jobs-process-nats-fixture-use-2
    let worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument-2
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument-2
        &[],
    );
    assert_refused(
        worker,
        "postgres migration history: embedded migrations are pending",
    );
    // template:begin outbox:test-jobs-process-nats-fixture-cleanup-2
    nats.cleanup().await;
    // template:end outbox:test-jobs-process-nats-fixture-cleanup-2
}

// template:begin outbox:test-jobs-process-outbox-capacity
#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn ordinary_jobs_and_outbox_require_n_plus_five_connections(pool: PgPool) {
    let database_url = child_database_url(&pool).await;
    let nats = NatsFixture::create().await;
    let worker = Worker::spawn(
        &database_url,
        &nats,
        &[("APP__POSTGRES__MAX_CONNECTIONS", "5")],
    );
    assert_refused(
        worker,
        "must be at least jobs.max_workers + 5 (6) for the outbox worker",
    );
    nats.cleanup().await;
}
// template:end outbox:test-jobs-process-outbox-capacity

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn ready_worker_runs_a_job_and_exits_0_on_sigterm(pool: PgPool) {
    prepare(&pool).await;
    let id = enqueue_committed(&pool, ProbeAction::Succeed).await;
    let database_url = child_database_url(&pool).await;
    // template:begin outbox:test-jobs-process-nats-fixture-use-3
    let nats = NatsFixture::create().await;
    // template:end outbox:test-jobs-process-nats-fixture-use-3
    let worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument-3
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument-3
        &[],
    );
    let api = listener_addr(&worker, "http listener bound");
    let diagnostics = listener_addr(&worker, "diagnostics listener bound");
    worker.await_record("jobs_worker_ready");

    let ready = format!("http://{api}/health/ready");
    assert!(
        poll_until(&ready, 200, READY_BOUND),
        "worker never became ready at {ready}"
    );
    let (status, body) = get(&format!("http://{api}/health/live")).expect("GET /health/live");
    assert_eq!((status, body.as_str()), (200, "ok"));
    let (status, _) = get(&format!("http://{api}/api")).expect("GET /api");
    assert_eq!(status, 404);
    let (status, body) =
        get(&format!("http://{diagnostics}/health/live")).expect("GET diagnostics /health/live");
    assert_eq!((status, body.as_str()), (200, "ok"));

    wait_completed(&pool, &id).await;
    await_completed_probe_metrics(&format!("http://{diagnostics}/metrics"));

    worker.terminate();
    let (code, stderr) = worker.wait();
    assert_eq!(code, Some(0), "stderr: {stderr}");
    // template:begin outbox:test-jobs-process-nats-fixture-cleanup-3
    nats.cleanup().await;
    // template:end outbox:test-jobs-process-nats-fixture-cleanup-3
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn background_task_that_ends_stops_the_worker_with_exit_1(pool: PgPool) {
    prepare(&pool).await;
    let database_url = child_database_url(&pool).await;
    // template:begin outbox:test-jobs-process-nats-fixture-use-6
    let nats = NatsFixture::create().await;
    // template:end outbox:test-jobs-process-nats-fixture-use-6
    let worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument-6
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument-6
        &[(integration_tests::jobs::BACKGROUND_TASK_RETURNS, "1")],
    );
    let stopped = worker.await_record("background_task_stopped");
    assert_eq!(stopped["task"], "fixture", "{stopped}");
    assert_eq!(stopped["panicked"], false, "{stopped}");
    // The refusal assertion observes the final staged record before exit.
    assert_refused(
        worker,
        "background task fixture stopped without a stop signal",
    );
    // template:begin outbox:test-jobs-process-nats-fixture-cleanup-6
    nats.cleanup().await;
    // template:end outbox:test-jobs-process-nats-fixture-cleanup-6
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn handler_panic_is_recorded_by_location_and_never_by_message(pool: PgPool) {
    prepare(&pool).await;
    let id = enqueue_committed(&pool, ProbeAction::Panic).await;
    let database_url = child_database_url(&pool).await;
    // template:begin outbox:test-jobs-process-nats-fixture-use-7
    let nats = NatsFixture::create().await;
    // template:end outbox:test-jobs-process-nats-fixture-use-7
    let worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument-7
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument-7
        &[],
    );
    let panicked = worker.await_record("panicked");
    assert!(
        panicked["panic.file"]
            .as_str()
            .is_some_and(|file| file.ends_with("jobs.rs")),
        "{panicked}"
    );
    assert!(panicked["panic.line"].is_u64(), "{panicked}");
    assert!(
        !panicked.to_string().contains("probe panicked"),
        "{panicked}"
    );
    assert_eq!(probe_attempts(&pool, &id).await.first(), Some(&1));

    // The panic is a retried attempt: the worker keeps running and stops cleanly.
    worker.terminate();
    let (code, stderr) = worker.wait();
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(!stderr.contains("probe panicked"), "stderr: {stderr}");
    // template:begin outbox:test-jobs-process-nats-fixture-cleanup-7
    nats.cleanup().await;
    // template:end outbox:test-jobs-process-nats-fixture-cleanup-7
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn supervisor_payload_drop_failure_preserves_live_and_cleanup_exit_policy(pool: PgPool) {
    prepare(&pool).await;
    let database_url = child_database_url(&pool).await;
    // template:begin outbox:test-jobs-process-supervisor-nats-fixture
    let nats = NatsFixture::create().await;
    // template:end outbox:test-jobs-process-supervisor-nats-fixture
    for cleanup in [false, true] {
        let worker = Worker::spawn(
            &database_url,
            // template:begin outbox:test-jobs-process-supervisor-nats-argument
            &nats,
            // template:end outbox:test-jobs-process-supervisor-nats-argument
            &[
                ("APP__HTTP__DRAIN_TIMEOUT", "1s"),
                ("APP__HTTP__READINESS_PROPAGATION_DELAY", "0s"),
                ("APP__HTTP__REQUEST_TIMEOUT", "500ms"),
            ],
        );
        worker.await_record("jobs_worker_ready");
        let action = if cleanup {
            ProbeAction::DropPanic { secondary: true }
        } else {
            ProbeAction::PanicPayloadDrop
        };
        let id = enqueue_committed(&pool, action).await;
        if cleanup {
            wait_running(&pool, &id).await;
            worker.terminate();
            worker.await_record("job_handler_drop_panicked");
        }
        let stopped = worker.await_record("jobs_engine_task_stopped");
        assert_eq!(stopped["task"], "attempt", "{stopped}");
        assert_eq!(stopped["panicked"], true, "{stopped}");
        if !cleanup {
            let failed = worker.await_record("jobs worker failed");
            assert!(
                failed["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("engine")),
                "{failed}"
            );
        }
        let (code, stderr) = worker.wait();
        assert_eq!(code, Some(if cleanup { 3 } else { 1 }), "stderr: {stderr}");
        assert!(!stderr.contains("drop-secret"), "stderr: {stderr}");
        assert_eq!(
            job_state(&pool, &id).await,
            "running",
            "supervisor failure retains lease recovery"
        );
        assert_eq!(probe_attempts(&pool, &id).await, [1]);
    }
    // template:begin outbox:test-jobs-process-supervisor-nats-cleanup
    nats.cleanup().await;
    // template:end outbox:test-jobs-process-supervisor-nats-cleanup
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn worker_metrics_publish_a_capped_fresh_registered_sample(pool: PgPool) {
    let inserted = sqlx::query(
        "INSERT INTO background_jobs (id, kind, payload, state, not_before) \
         SELECT gen_random_uuid(), $1, jsonb_build_object('action', 'succeed'), 'pending', \
                statement_timestamp() + interval '1 hour' \
         FROM generate_series(1, 1001)",
    )
    .bind(Probe::NAME)
    .execute(&pool)
    .await
    .expect("the scheduled jobs");
    assert_eq!(inserted.rows_affected(), 1_001);
    let database_url = child_database_url(&pool).await;
    let failed_kinds = vec![Probe::NAME];
    // template:begin outbox:test-jobs-process-nats-fixture-use-4
    let nats = NatsFixture::create().await;
    let failed_kinds = {
        let mut kinds = failed_kinds;
        kinds.push("publish_domain_event");
        kinds
    };
    // template:end outbox:test-jobs-process-nats-fixture-use-4
    for kind in failed_kinds.iter().copied().chain(["test.unregistered"]) {
        sqlx::query(
            "INSERT INTO background_jobs \
             (id, kind, payload, state, failure_reason, not_before, finished_at) \
             SELECT gen_random_uuid(), $1, '{}'::jsonb, 'failed', 'exhausted', \
                    statement_timestamp(), statement_timestamp() \
             FROM generate_series(1, 1001)",
        )
        .bind(kind)
        .execute(&pool)
        .await
        .expect("retained failures for registered and unknown kinds");
    }
    let worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument-4
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument-4
        &[],
    );
    let diagnostics = listener_addr(&worker, "diagnostics listener bound");
    worker.await_record("jobs_worker_ready");
    let metrics = format!("http://{diagnostics}/metrics");
    await_capped_scheduled_probe_sample(&metrics, &failed_kinds);
    // Make the data statement fail immediately; a table lock could instead
    // stall a claim holding the shared engine permit before the sample runs.
    sqlx::query("ALTER TABLE background_jobs RENAME TO unavailable_background_jobs")
        .execute(&pool)
        .await
        .expect("the disposable fixture table becomes unavailable");
    let (_, before) = get(&metrics).expect("the last good metrics scrape");
    let before = queue_observation(&before);
    await_failed_sample_with_last_good_values(&metrics, &before);
    sqlx::query("ALTER TABLE unavailable_background_jobs RENAME TO background_jobs")
        .execute(&pool)
        .await
        .expect("the disposable fixture table is restored");

    worker.terminate();
    let (code, stderr) = worker.wait();
    assert_eq!(code, Some(0), "stderr: {stderr}");
    // template:begin outbox:test-jobs-process-nats-fixture-cleanup-4
    nats.cleanup().await;
    // template:end outbox:test-jobs-process-nats-fixture-cleanup-4
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn worker_counts_a_lost_wake_listener_and_listens_again(pool: PgPool) {
    let database_url = child_database_url(&pool).await;
    // template:begin outbox:test-jobs-process-nats-fixture-use-8
    let nats = NatsFixture::create().await;
    // template:end outbox:test-jobs-process-nats-fixture-use-8
    let worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument-8
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument-8
        &[],
    );
    let diagnostics = listener_addr(&worker, "diagnostics listener bound");
    worker.await_record("jobs_worker_ready");
    let metrics = format!("http://{diagnostics}/metrics");
    let first = wake_listener(&pool, None).await;
    let (_, before) = get(&metrics).expect("a metrics scrape");
    assert!(!operation_failed(&before, "listen"), "{before}");

    // The server's closing error is not a notification, so the driver
    // reports the terminated session only as a lost connection.
    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(first)
        .fetch_one(&pool)
        .await
        .expect("the listener's backend is terminated");
    assert!(terminated);
    await_failed_operation(&metrics, "listen");
    wake_listener(&pool, Some(first)).await;

    worker.terminate();
    let (code, stderr) = worker.wait();
    assert_eq!(code, Some(0), "stderr: {stderr}");
    // template:begin outbox:test-jobs-process-nats-fixture-cleanup-8
    nats.cleanup().await;
    // template:end outbox:test-jobs-process-nats-fixture-cleanup-8
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn attempt_that_outlives_a_short_drain_exits_3_and_is_claimable(pool: PgPool) {
    prepare(&pool).await;
    let id = enqueue_committed(&pool, ProbeAction::DropPanic { secondary: false }).await;
    let database_url = child_database_url(&pool).await;
    // template:begin outbox:test-jobs-process-nats-fixture-use-5
    let nats = NatsFixture::create().await;
    // template:end outbox:test-jobs-process-nats-fixture-use-5
    let worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument-5
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument-5
        &[
            ("APP__HTTP__DRAIN_TIMEOUT", "1s"),
            ("APP__HTTP__READINESS_PROPAGATION_DELAY", "0s"),
            ("APP__HTTP__REQUEST_TIMEOUT", "500ms"),
        ],
    );
    // template:begin outbox:test-jobs-process-shared-engines
    let started = worker.await_record("jobs_claiming_started");
    assert_eq!(started["engines"], 2, "{started}");
    // template:end outbox:test-jobs-process-shared-engines
    worker.await_record("jobs_worker_ready");
    wait_running(&pool, &id).await;
    worker.terminate();
    let forced = worker.await_record("drain_forced");
    assert_eq!(forced["reason"], "budget", "{forced}");
    let destroyed = worker.await_record("job_handler_drop_panicked");
    assert!(!destroyed.to_string().contains("handler-drop-secret"));
    let released = worker.await_record_matching("attempts_finished", |record| {
        record["cancelled"] == 1 && record["released"] == 1
    });
    assert_eq!(released["cancelled"], 1, "{released}");
    assert_eq!(released["released"], 1, "{released}");
    assert_eq!(released["timed_out"], false, "{released}");
    let (code, stderr) = worker.wait();
    assert_eq!(code, Some(3), "stderr: {stderr}");
    assert!(!stderr.contains("handler-drop-secret"), "{stderr}");
    assert_claimable(&pool, &id).await;
    // template:begin outbox:test-jobs-process-nats-fixture-cleanup-5
    nats.cleanup().await;
    // template:end outbox:test-jobs-process-nats-fixture-cleanup-5
}
