use std::process::Stdio;
// template:begin outbox:test-jobs-process-nats-imports
use std::sync::atomic::{AtomicU64, Ordering};
// template:end outbox:test-jobs-process-nats-imports
use std::sync::LazyLock;
use std::time::Duration;

// template:begin outbox:test-jobs-process-nats-imports-2
use async_nats::jetstream::{self, stream};
// template:end outbox:test-jobs-process-nats-imports-2
use infra_jobs::{EnqueueOptions, Enqueued, JobKind, enqueue};
use infra_postgres::{PgPool, TxError, in_tx};
use integration_tests::jobs::{CREATE_PROBE_ATTEMPTS, Probe, ProbeAction};
use integration_tests::poll_until;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use sqlx::Row;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_util::task::AbortOnDropHandle;

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
    pid: u32,
    lines: mpsc::UnboundedReceiver<String>,
    stdout: AbortOnDropHandle<std::io::Result<String>>,
    stderr: AbortOnDropHandle<std::io::Result<String>>,
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
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().expect("spawn jobs-worker-fixture");
        let pid = child.id().expect("spawned worker has PID");
        let stdout_pipe = child.stdout.take().expect("piped stdout");
        let stderr_pipe = child.stderr.take().expect("piped stderr");
        let (tx, lines) = mpsc::unbounded_channel();
        let stdout = tokio::spawn(async move {
            let mut captured = String::new();
            let mut reader = BufReader::new(stdout_pipe).lines();
            while let Some(line) = reader.next_line().await? {
                captured.push_str(&line);
                captured.push('\n');
                let _ = tx.send(line);
            }
            Ok(captured)
        });
        let stderr = tokio::spawn(async move {
            let mut captured = String::new();
            let mut reader = stderr_pipe;
            reader.read_to_string(&mut captured).await?;
            Ok(captured)
        });
        Self {
            child,
            pid,
            lines,
            stdout: AbortOnDropHandle::new(stdout),
            stderr: AbortOnDropHandle::new(stderr),
        }
    }

    async fn await_record(&mut self, message: &str) -> serde_json::Value {
        self.await_record_matching(message, |_| true).await
    }

    async fn await_record_matching(
        &mut self,
        message: &str,
        matches: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        timeout(RECORD_BOUND, async {
            loop {
                let Some(line) = self.lines.recv().await else {
                    panic!("worker stdout closed before {message:?} record");
                };
                let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
                    panic!("stdout must be one JSON object per line, got {line:?}");
                };
                if record["message"] == message && matches(&record) {
                    return record;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no {message:?} record before the deadline"))
    }

    fn terminate(&self) {
        kill(Pid::from_raw(self.pid.cast_signed()), Signal::SIGTERM).expect("send SIGTERM");
    }

    #[expect(
        clippy::print_stderr,
        reason = "bounded process fixture reports captured shutdown records on nonzero exit"
    )]
    async fn wait(mut self) -> (Option<i32>, String, String) {
        let deadline = Instant::now() + EXIT_BOUND;
        let status = if let Ok(result) = timeout(EXIT_BOUND, self.child.wait()).await {
            result.expect("wait for the worker")
        } else {
            let _ = self.child.kill().await;
            let stderr = timeout(Duration::from_secs(2), &mut self.stderr)
                .await
                .ok()
                .and_then(Result::ok)
                .and_then(Result::ok)
                .unwrap_or_default();
            let mut remaining = Vec::new();
            while let Ok(line) = self.lines.try_recv() {
                remaining.push(line);
                if remaining.len() == 256 {
                    break;
                }
            }
            let stdout = remaining.join("\n");
            panic!(
                "worker did not exit within {EXIT_BOUND:?}; stderr: {stderr}; remaining stdout:\n{stdout}"
            );
        };
        let code = status.code();
        let stderr = timeout_at(deadline, &mut self.stderr)
            .await
            .expect("worker stderr reaches EOF within bound")
            .expect("stderr collector joins")
            .expect("stderr pipe is read completely");
        let stdout = timeout_at(deadline, &mut self.stdout)
            .await
            .unwrap_or_else(|_| {
                panic!("worker stdout did not reach EOF within {EXIT_BOUND:?}; stderr: {stderr}")
            })
            .expect("stdout collector joins")
            .expect("stdout pipe is read completely");
        if code != Some(0) {
            eprintln!("worker exited with {code:?}; stdout:\n{stdout}");
        }
        (code, stderr, stdout)
    }
}

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .pool_max_idle_per_host(0)
        .build()
        .expect("HTTP fixture client")
});

async fn get(url: &str) -> Result<(u16, String), reqwest::Error> {
    let response = HTTP.get(url).send().await?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    Ok((status, body))
}

async fn listener_addr(worker: &mut Worker, message: &str) -> String {
    worker.await_record(message).await["addr"]
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

async fn await_completed_probe_metrics(url: &str) {
    poll_until(
        "metrics show the completed probe attempt",
        METRICS_BOUND,
        POLL,
        async || {
            let (status, scraped) = get(url).await.ok()?;
            (status == 200 && completed_probe_metrics(&scraped)).then_some(())
        },
    )
    .await;
}

fn population_gauge(body: &str, metric: &str, population: &str) -> Option<f64> {
    let name = format!("{metric}{{population=\"{population}\"}} ");
    body.lines()
        .find_map(|line| line.strip_prefix(&name))
        .and_then(|value| value.parse().ok())
}

const OLDEST_DUE_UNIX_SECONDS: f64 = 946_684_800.0;

fn capped_probe_sample(body: &str, failed_kinds: &[&str]) -> bool {
    let mut scheduled = false;
    let mut available = false;
    let mut observed_at = None;
    let mut oldest_age = None;
    for line in body.lines() {
        let value = line
            .split_ascii_whitespace()
            .last()
            .and_then(|text| text.parse::<f64>().ok());
        if line.starts_with("jobs_live_jobs{")
            && line.contains("kind=\"test.probe\"")
            && value == Some(1_000.0)
        {
            scheduled |= line.contains("state=\"scheduled\"");
            available |= line.contains("state=\"available\"");
        }
        if line.starts_with("jobs_observation_timestamp_seconds ") {
            observed_at = value;
        }
        if line.starts_with("jobs_oldest_available_age_seconds{")
            && line.contains("kind=\"test.probe\"")
        {
            oldest_age = value;
        }
    }
    let failed: Vec<_> = body
        .lines()
        .filter(|line| line.starts_with("jobs_failed_jobs{"))
        .collect();
    scheduled
        && available
        && observed_at.zip(oldest_age).is_some_and(|(time, age)| {
            age > 0.0 && (time - age - OLDEST_DUE_UNIX_SECONDS).abs() < 0.001
        })
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

async fn await_capped_probe_sample(
    url: &str,
    failed_kinds: &[&str],
    completed_eligible_at: f64,
    unknown_failed_at: f64,
) {
    let populations = [
        ("jobs", completed_eligible_at),
        ("failed_jobs", unknown_failed_at),
    ];
    poll_until(
        "metrics show the capped queue sample",
        METRICS_BOUND,
        POLL,
        async || {
            let (status, scraped) = get(url).await.ok()?;
            (status == 200
                && capped_probe_sample(&scraped, failed_kinds)
                && populations.iter().all(|(population, oldest)| {
                    population_gauge(
                        &scraped,
                        "postgres_maintenance_last_attempt_success",
                        population,
                    ) == Some(1.0)
                        && population_gauge(&scraped, "postgres_maintenance_present", population)
                            == Some(1.0)
                        && population_gauge(
                            &scraped,
                            "postgres_maintenance_oldest_timestamp_seconds",
                            population,
                        )
                        .is_some_and(|value| (value - oldest).abs() < 0.01)
                }))
            .then_some(())
        },
    )
    .await;
}

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

async fn await_failed_operation(url: &str, operation: &str) {
    poll_until(
        &format!("metrics count a failed {operation}"),
        METRICS_BOUND,
        POLL,
        async || {
            let (status, scraped) = get(url).await.ok()?;
            (status == 200 && operation_failed(&scraped, operation)).then_some(())
        },
    )
    .await;
}

async fn wake_listener(pool: &PgPool, except: Option<i32>) -> i32 {
    poll_until(
        "the worker listens for wake notifications",
        JOB_BOUND,
        DB_POLL,
        async || {
            sqlx::query_scalar(
                "SELECT pid FROM pg_stat_activity \
                 WHERE datname = current_database() AND query LIKE 'LISTEN%' \
                   AND pid IS DISTINCT FROM $1",
            )
            .bind(except)
            .fetch_optional(pool)
            .await
            .expect("the listener observation")
        },
    )
    .await
}

fn queue_observation(body: &str) -> std::collections::BTreeMap<String, u64> {
    body.lines()
        .filter(|line| {
            line.starts_with("jobs_live_jobs{")
                || line.starts_with("jobs_failed_jobs{")
                || line.starts_with("jobs_oldest_available_age_seconds{")
                || line.starts_with("jobs_observation_timestamp_seconds ")
                || line.starts_with("postgres_maintenance_last_success_timestamp_seconds{")
                || line.starts_with("postgres_maintenance_database_observed_timestamp_seconds{")
                || line.starts_with("postgres_maintenance_present{")
                || line.starts_with("postgres_maintenance_oldest_timestamp_seconds{")
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

async fn await_failed_sample_with_last_good_values(
    url: &str,
    before: &std::collections::BTreeMap<String, u64>,
) {
    poll_until(
        "metrics retain the last good sample after a failure",
        Duration::from_secs(45),
        POLL,
        async || {
            let (status, scraped) = get(url).await.ok()?;
            (status == 200
                && operation_failed(&scraped, "sample")
                && ["jobs", "failed_jobs"].iter().all(|population| {
                    population_gauge(
                        &scraped,
                        "postgres_maintenance_last_attempt_success",
                        population,
                    ) == Some(0.0)
                })
                && queue_observation(&scraped) == *before)
                .then_some(())
        },
    )
    .await;
}

async fn child_database_url(pool: &PgPool) -> String {
    integration_tests::url_for(pool, integration_tests::DATABASE_URL)
        .await
        .into()
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
    poll_until(
        &format!("job {id} completes"),
        JOB_BOUND,
        DB_POLL,
        async || (job_state(pool, id).await == "completed").then_some(()),
    )
    .await;
    let attempts = probe_attempts(pool, id).await;
    assert_eq!(attempts, [1], "probe attempts for {id}");
}

async fn wait_running(pool: &PgPool, id: &str) {
    poll_until("attempt is in flight", JOB_BOUND, DB_POLL, async || {
        (job_state(pool, id).await == "running" && probe_attempts(pool, id).await == [1])
            .then_some(())
    })
    .await;
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

async fn assert_refused(mut worker: Worker, needle: &str) {
    let failure = worker.await_record("jobs worker failed").await;
    assert!(
        failure["error"]
            .as_str()
            .is_some_and(|error| error.contains(needle)),
        "{failure}"
    );
    let finishing = worker.await_record("shutdown_finishing").await;
    assert_eq!(finishing["logger_pending"], true);
    let (code, stderr, stdout) = worker.wait().await;
    assert_eq!(code, Some(1), "stderr: {stderr}; stdout:\n{stdout}");
    assert!(
        stderr.is_empty(),
        "no post-install fallback: {stderr}; stdout:\n{stdout}"
    );
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
    )
    .await;
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
    )
    .await;
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
        "must be at least jobs.max_workers + 5 (6) for the outbox worker, got 5",
    )
    .await;
    nats.cleanup().await;
}
// template:end outbox:test-jobs-process-outbox-capacity

// template:begin outbox:test-jobs-process-deferred-messages
#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn deferred_message_failure_closes_the_admitted_worker_pool(pool: PgPool) {
    let database_url = child_database_url(&pool).await;
    let nats = NatsFixture::create().await;
    for (mode, expected) in [
        ("error", "fixture message factory refused"),
        ("panic", "worker lifecycle panicked; payload withheld"),
        ("empty", "typed message handlers are invalid"),
    ] {
        let mut worker = Worker::spawn(
            &database_url,
            &nats,
            &[
                ("JOBS_WORKER_FIXTURE_MESSAGE_FACTORY", mode),
                ("APP__MESSAGING__CONSUMER_DURABLE", "factory-fixture"),
                ("APP__MESSAGING__CONSUMER_FILTER_SUBJECT", "test.factory.>"),
                ("APP__MESSAGING__DLQ_SUBJECT", "test.factory.dlq"),
            ],
        );
        worker.await_record("postgres_pool_opened").await;
        let failure = worker.await_record("jobs worker failed").await;
        assert!(
            failure["error"].as_str().unwrap().contains(expected),
            "{failure}"
        );
        worker.await_record("postgres_pool_closed").await;
        worker.await_record("shutdown_finishing").await;
        let (code, stderr, stdout) = worker.wait().await;
        assert_eq!(code, Some(1), "stderr: {stderr}; stdout:\n{stdout}");
        assert!(
            stderr.is_empty(),
            "panic payload must be withheld: {stderr}"
        );
    }
    nats.cleanup().await;
}
// template:end outbox:test-jobs-process-deferred-messages

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn ready_worker_runs_a_job_and_exits_0_on_sigterm(pool: PgPool) {
    prepare(&pool).await;
    let id = enqueue_committed(&pool, ProbeAction::Succeed).await;
    let database_url = child_database_url(&pool).await;
    // template:begin outbox:test-jobs-process-nats-fixture-use-3
    let nats = NatsFixture::create().await;
    // template:end outbox:test-jobs-process-nats-fixture-use-3
    let mut worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument-3
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument-3
        &[],
    );
    let api = listener_addr(&mut worker, "http listener bound").await;
    let diagnostics = listener_addr(&mut worker, "diagnostics listener bound").await;
    worker.await_record("jobs_worker_ready").await;

    let ready = format!("http://{api}/health/ready");
    poll_until(
        &format!("worker becomes ready at {ready}"),
        READY_BOUND,
        POLL,
        async || {
            let (status, _) = get(&ready).await.ok()?;
            (status == 200).then_some(())
        },
    )
    .await;
    let (status, body) = get(&format!("http://{api}/health/live"))
        .await
        .expect("GET /health/live");
    assert_eq!((status, body.as_str()), (200, "ok"));
    let (status, _) = get(&format!("http://{api}/api")).await.expect("GET /api");
    assert_eq!(status, 404);
    let (status, body) = get(&format!("http://{diagnostics}/health/live"))
        .await
        .expect("GET diagnostics /health/live");
    assert_eq!((status, body.as_str()), (200, "ok"));

    wait_completed(&pool, &id).await;
    await_completed_probe_metrics(&format!("http://{diagnostics}/metrics")).await;

    worker.terminate();
    let (code, stderr, stdout) = worker.wait().await;
    assert_eq!(code, Some(0), "stderr: {stderr}; stdout:\n{stdout}");
    // template:begin outbox:test-jobs-process-nats-fixture-cleanup-3
    nats.cleanup().await;
    // template:end outbox:test-jobs-process-nats-fixture-cleanup-3
}

#[cfg(unix)]
#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn resumed_worker_exits_one_after_progress_loss_without_diagnostics(pool: PgPool) {
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};

    prepare(&pool).await;
    let database_url = child_database_url(&pool).await;
    // template:begin outbox:test-jobs-process-progress-nats-create
    let nats = NatsFixture::create().await;
    // template:end outbox:test-jobs-process-progress-nats-create
    let mut worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-progress-nats-argument
        &nats,
        // template:end outbox:test-jobs-process-progress-nats-argument
        &[
            ("APP__OBSERVABILITY__METRICS__ADDR", ""),
            ("APP__HEALTH__REFRESH_INTERVAL", "500ms"),
            ("APP__HEALTH__PROBE_BUDGET", "500ms"),
        ],
    );
    worker.await_record("jobs_worker_ready").await;
    let pid = Pid::from_raw(worker.pid.cast_signed());
    kill(pid, Signal::SIGSTOP).expect("suspend admitted worker");
    poll_until(
        "worker stops on SIGSTOP",
        Duration::from_secs(2),
        Duration::from_millis(10),
        async || match waitpid(pid, Some(WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG)).unwrap() {
            WaitStatus::Stopped(_, Signal::SIGSTOP) => Some(()),
            WaitStatus::StillAlive => None,
            other => panic!("unexpected child state: {other:?}"),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    kill(pid, Signal::SIGCONT).expect("resume admitted worker");
    let (code, stderr, stdout) = worker.wait().await;
    assert_eq!(code, Some(1), "{stdout}\n{stderr}");
    let records: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let losses: Vec<_> = records
        .iter()
        .filter(|record| record["message"] == "readiness_progress_lost")
        .collect();
    assert_eq!(losses.len(), 1, "{stdout}");
    let lost = losses[0];
    assert_eq!(lost["task"], "readiness_progress");
    assert!(lost["age_seconds"].as_f64().unwrap() > lost["bound_seconds"].as_f64().unwrap());
    let failure = records
        .iter()
        .find(|record| record["message"] == "jobs worker failed")
        .expect("primary failure is logged");
    assert!(
        failure["error"]
            .as_str()
            .unwrap()
            .contains("readiness_progress")
    );
    assert!(stdout.contains("shutdown_finishing"), "{stdout}");
    assert!(!stdout.contains("readiness_recovered"), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
    // template:begin outbox:test-jobs-process-progress-nats-cleanup
    nats.cleanup().await;
    // template:end outbox:test-jobs-process-progress-nats-cleanup
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn background_task_that_ends_stops_the_worker_with_exit_1(pool: PgPool) {
    prepare(&pool).await;
    let database_url = child_database_url(&pool).await;
    // template:begin outbox:test-jobs-process-nats-fixture-use-6
    let nats = NatsFixture::create().await;
    // template:end outbox:test-jobs-process-nats-fixture-use-6
    let mut worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument-6
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument-6
        &[(integration_tests::jobs::BACKGROUND_TASK_RETURNS, "1")],
    );
    let stopped = worker.await_record("background_task_stopped").await;
    assert_eq!(stopped["task"], "fixture", "{stopped}");
    assert_eq!(stopped["panicked"], false, "{stopped}");
    assert_refused(
        worker,
        "background task fixture stopped without a stop signal",
    )
    .await;
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
    let mut worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument-7
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument-7
        &[],
    );
    let panicked = worker.await_record("panicked").await;
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

    worker.terminate();
    let (code, stderr, stdout) = worker.wait().await;
    assert_eq!(code, Some(0), "stderr: {stderr}; stdout:\n{stdout}");
    assert!(
        !stderr.contains("probe panicked"),
        "stderr: {stderr}; stdout:\n{stdout}"
    );
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
        let mut worker = Worker::spawn(
            &database_url,
            // template:begin outbox:test-jobs-process-supervisor-nats-argument
            &nats,
            // template:end outbox:test-jobs-process-supervisor-nats-argument
            &[("APP__HTTP__DRAIN_TIMEOUT", "1s")],
        );
        worker.await_record("jobs_worker_ready").await;
        let action = if cleanup {
            ProbeAction::DropPanic { secondary: true }
        } else {
            ProbeAction::PanicPayloadDrop
        };
        let id = enqueue_committed(&pool, action).await;
        if cleanup {
            wait_running(&pool, &id).await;
            worker.terminate();
            worker.await_record("job_handler_drop_panicked").await;
        }
        let stopped = worker.await_record("jobs_engine_task_stopped").await;
        assert_eq!(stopped["task"], "attempt", "{stopped}");
        assert_eq!(stopped["panicked"], true, "{stopped}");
        if !cleanup {
            let failed = worker.await_record("jobs worker failed").await;
            assert!(
                failed["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("engine")),
                "{failed}"
            );
        }
        let (code, stderr, stdout) = worker.wait().await;
        assert_eq!(
            code,
            Some(if cleanup { 3 } else { 1 }),
            "stderr: {stderr}; stdout: {stdout}"
        );
        assert!(
            !stderr.contains("drop-secret"),
            "stderr: {stderr}; stdout: {stdout}"
        );
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
                CASE WHEN seq <= 1001 THEN statement_timestamp() + interval '1 hour' \
                     WHEN seq <= 2001 THEN statement_timestamp() - interval '1 hour' \
                     ELSE to_timestamp($2) END \
         FROM generate_series(1, 2003) AS seq",
    )
    .bind(Probe::NAME)
    .bind(OLDEST_DUE_UNIX_SECONDS)
    .execute(&pool)
    .await
    .expect("scheduled and due jobs with two tied oldest entries");
    assert_eq!(inserted.rows_affected(), 2_003);
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
    let unknown_failed_at: f64 = sqlx::query_scalar(
        "WITH changed AS ( \
             UPDATE background_jobs SET finished_at = statement_timestamp() - interval '1 hour' \
             WHERE kind = 'test.unregistered' RETURNING finished_at) \
         SELECT EXTRACT(EPOCH FROM min(finished_at))::double precision FROM changed",
    )
    .fetch_one(&pool)
    .await
    .expect("oldest unknown-kind failure");
    let (completed_id, completed_eligible_at): (uuid::Uuid, f64) = sqlx::query_as(
        "INSERT INTO background_jobs (id, kind, payload, state, not_before, finished_at) \
         VALUES (gen_random_uuid(), 'test.unregistered', '{}'::jsonb, 'completed', \
                 statement_timestamp() - interval '25 hours', statement_timestamp() - interval '25 hours') \
         RETURNING id, EXTRACT(EPOCH FROM finished_at + interval '24 hours')::double precision",
    )
    .fetch_one(&pool)
    .await
    .expect("completed backlog");
    #[expect(
        clippy::disallowed_methods,
        reason = "uncommitted deletes hold row locks through both worker samples"
    )]
    let mut locked = pool.begin().await.expect("hold sampled jobs");
    let deleted = sqlx::query(
        "DELETE FROM background_jobs WHERE id = $1 \
         OR (kind = $2 AND state = 'pending' AND not_before <= statement_timestamp())",
    )
    .bind(completed_id)
    .bind(Probe::NAME)
    .execute(&mut *locked)
    .await
    .expect("claim and cleanup skip these rows while observation still sees them");
    assert_eq!(deleted.rows_affected(), 1_003);
    let mut worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument-4
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument-4
        &[],
    );
    let diagnostics = listener_addr(&mut worker, "diagnostics listener bound").await;
    worker.await_record("jobs_worker_ready").await;
    let metrics = format!("http://{diagnostics}/metrics");
    await_capped_probe_sample(
        &metrics,
        &failed_kinds,
        completed_eligible_at,
        unknown_failed_at,
    )
    .await;
    locked.commit().await.expect("release the sampled jobs");
    sqlx::query("ALTER TABLE background_jobs RENAME TO unavailable_background_jobs")
        .execute(&pool)
        .await
        .expect("the disposable fixture table becomes unavailable");
    let (_, before) = get(&metrics).await.expect("the last good metrics scrape");
    let before = queue_observation(&before);
    await_failed_sample_with_last_good_values(&metrics, &before).await;
    sqlx::query("ALTER TABLE unavailable_background_jobs RENAME TO background_jobs")
        .execute(&pool)
        .await
        .expect("the disposable fixture table is restored");

    worker.terminate();
    let (code, stderr, stdout) = worker.wait().await;
    assert_eq!(code, Some(0), "stderr: {stderr}; stdout:\n{stdout}");
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
    let mut worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument-8
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument-8
        &[],
    );
    let diagnostics = listener_addr(&mut worker, "diagnostics listener bound").await;
    worker.await_record("jobs_worker_ready").await;
    let metrics = format!("http://{diagnostics}/metrics");
    let first = wake_listener(&pool, None).await;
    let (_, before) = get(&metrics).await.expect("a metrics scrape");
    assert!(!operation_failed(&before, "listen"), "{before}");

    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(first)
        .fetch_one(&pool)
        .await
        .expect("the listener's backend is terminated");
    assert!(terminated);
    await_failed_operation(&metrics, "listen").await;
    wake_listener(&pool, Some(first)).await;

    worker.terminate();
    let (code, stderr, stdout) = worker.wait().await;
    assert_eq!(code, Some(0), "stderr: {stderr}; stdout:\n{stdout}");
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
    let mut worker = Worker::spawn(
        &database_url,
        // template:begin outbox:test-jobs-process-nats-fixture-argument-5
        &nats,
        // template:end outbox:test-jobs-process-nats-fixture-argument-5
        &[("APP__HTTP__DRAIN_TIMEOUT", "1s")],
    );
    // template:begin outbox:test-jobs-process-shared-engines
    let started = worker.await_record("jobs_claiming_started").await;
    assert_eq!(started["engines"], 2, "{started}");
    // template:end outbox:test-jobs-process-shared-engines
    worker.await_record("jobs_worker_ready").await;
    wait_running(&pool, &id).await;
    worker.terminate();
    let forced = worker.await_record("drain_forced").await;
    assert_eq!(forced["reason"], "budget", "{forced}");
    let destroyed = worker.await_record("job_handler_drop_panicked").await;
    assert!(!destroyed.to_string().contains("handler-drop-secret"));
    let released = worker
        .await_record_matching("attempts_finished", |record| {
            record["cancelled"] == 1 && record["released"] == 1
        })
        .await;
    assert_eq!(released["cancelled"], 1, "{released}");
    assert_eq!(released["released"], 1, "{released}");
    assert_eq!(released["timed_out"], false, "{released}");
    let (code, stderr, stdout) = worker.wait().await;
    assert_eq!(code, Some(3), "stderr: {stderr}; stdout:\n{stdout}");
    assert!(
        !stderr.contains("handler-drop-secret"),
        "stderr: {stderr}; stdout:\n{stdout}"
    );
    assert_claimable(&pool, &id).await;
    // template:begin outbox:test-jobs-process-nats-fixture-cleanup-5
    nats.cleanup().await;
    // template:end outbox:test-jobs-process-nats-fixture-cleanup-5
}
