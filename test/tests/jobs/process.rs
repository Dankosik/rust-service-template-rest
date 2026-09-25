//! Process proof of the jobs worker on the test-only fixture binary against
//! real PostgreSQL: refusals exit 1, a ready worker runs a committed job and
//! exits 0 on SIGTERM with its attempt metrics on the diagnostics listener,
//! and an attempt that outlives a short drain exits 3 with its job released
//! for an immediate claim. Each case gets its own database from `#[sqlx::test]`.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use infra_jobs::{EnqueueOptions, Enqueued, enqueue};
use infra_postgres::PgPool;
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

struct Worker {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Worker {
    fn spawn(database_url: &str, env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jobs-worker-fixture"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("APP__HTTP__ADDR", "127.0.0.1:0")
            .env("APP__OBSERVABILITY__METRICS__ADDR", "127.0.0.1:0")
            .env("APP__LOG__FORMAT", "json")
            .env("APP__POSTGRES__ENABLED", "true")
            .env("APP__POSTGRES__DSN", database_url)
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
        let deadline = Instant::now() + RECORD_BOUND;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(remaining).unwrap_or_else(|_| {
                panic!("no {message:?} record before the deadline");
            });
            let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
                panic!("stdout must be one JSON object per line, got {line:?}");
            };
            if record["message"] == message {
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
    attempts && duration
}

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
    let mut tx = pool.begin().await.expect("begin the enqueue transaction");
    let enqueued = enqueue(&mut tx, &Probe { action }, EnqueueOptions::default())
        .await
        .expect("enqueue");
    let Enqueued::Created(id) = enqueued else {
        panic!("duplicate enqueue");
    };
    tx.commit().await.expect("commit the enqueue");
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
    let (code, stderr) = worker.wait();
    assert_eq!(code, Some(1), "stderr: {stderr}");
    assert!(stderr.contains(needle), "stderr: {stderr}");
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn postgres_disabled_exits_1(pool: PgPool) {
    let database_url = child_database_url(&pool).await;
    let worker = Worker::spawn(&database_url, &[("APP__POSTGRES__ENABLED", "false")]);
    assert_refused(
        worker,
        "postgres.enabled must be true to run the jobs worker",
    );
}

#[sqlx::test(migrations = false)]
async fn missing_jobs_schema_exits_1(pool: PgPool) {
    let database_url = child_database_url(&pool).await;
    let worker = Worker::spawn(&database_url, &[]);
    assert_refused(worker, "jobs startup check: the jobs schema is missing");
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn ready_worker_runs_a_job_and_exits_0_on_sigterm(pool: PgPool) {
    prepare(&pool).await;
    let id = enqueue_committed(&pool, ProbeAction::Succeed).await;
    let database_url = child_database_url(&pool).await;
    let worker = Worker::spawn(&database_url, &[]);
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

    wait_completed(&pool, &id).await;
    await_completed_probe_metrics(&format!("http://{diagnostics}/metrics"));

    worker.terminate();
    let (code, stderr) = worker.wait();
    assert_eq!(code, Some(0), "stderr: {stderr}");
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn attempt_that_outlives_a_short_drain_exits_3_and_is_claimable(pool: PgPool) {
    prepare(&pool).await;
    let id = enqueue_committed(&pool, ProbeAction::WaitForCancellation).await;
    let database_url = child_database_url(&pool).await;
    let worker = Worker::spawn(
        &database_url,
        &[
            ("APP__HTTP__DRAIN_TIMEOUT", "1s"),
            ("APP__HTTP__READINESS_PROPAGATION_DELAY", "0s"),
            ("APP__HTTP__REQUEST_TIMEOUT", "500ms"),
        ],
    );
    worker.await_record("jobs_worker_ready");
    wait_running(&pool, &id).await;
    worker.terminate();
    let forced = worker.await_record("drain_forced");
    assert_eq!(forced["reason"], "budget", "{forced}");
    let released = worker.await_record("attempts_released");
    assert_eq!(released["cancelled"], 1, "{released}");
    assert_eq!(released["released"], 1, "{released}");
    assert_eq!(released["timed_out"], false, "{released}");
    let (code, stderr) = worker.wait();
    assert_eq!(code, Some(3), "stderr: {stderr}");
    assert_claimable(&pool, &id).await;
}
