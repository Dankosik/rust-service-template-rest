use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use infra_jobs::{
    DEFAULT_TIMEOUT, DrainEnd, Engine, EnqueueError, EnqueueOptions, Enqueued, Job, JobError,
    JobKind, Kinds, LEASE_RESERVE, MIN_TIMEOUT, POLL_INTERVAL, Policy, StartupError, enqueue,
};
use infra_postgres::{Dsn, PgPool, TxError, connection, in_tx};
use integration_tests::DATABASE_URL;
use integration_tests::dsn_for;
use integration_tests::jobs::{self, Probe, ProbeAction};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use url::Url;

use super::commit_proxy::{CommitProxy, Fault};

const POLL: Duration = Duration::from_millis(50);
const RELEASE_BUDGET: Duration = Duration::from_secs(2);
const RACE_JOBS: i64 = 20;

#[derive(Debug, Serialize, Deserialize)]
struct Gate {
    token: u32,
}

impl JobKind for Gate {
    const NAME: &'static str = "test.gate";
}

const _: () = infra_jobs::assert_valid_kind_name(Gate::NAME);

#[derive(Debug, Serialize, Deserialize)]
struct TransactionGate {
    token: u32,
}

impl JobKind for TransactionGate {
    const NAME: &'static str = "test.transaction_gate";
}

#[derive(Debug, Serialize, Deserialize)]
struct Stranded {
    token: u32,
}

impl JobKind for Stranded {
    const NAME: &'static str = "test.stranded";
}

#[derive(Debug)]
enum Step {
    Enqueue(EnqueueError),
    Query(sqlx::Error),
    Tx(TxError),
    Rejected,
}

impl From<TxError> for Step {
    fn from(err: TxError) -> Self {
        Self::Tx(err)
    }
}

impl From<EnqueueError> for Step {
    fn from(err: EnqueueError) -> Self {
        Self::Enqueue(err)
    }
}

impl From<sqlx::Error> for Step {
    fn from(err: sqlx::Error) -> Self {
        Self::Query(err)
    }
}

fn explain(err: &Step) -> String {
    match err {
        Step::Enqueue(err) => format!("enqueue failed: {err}"),
        Step::Query(err) => format!("query failed: {err}"),
        Step::Tx(err) => format!("transaction failed: {err}"),
        Step::Rejected => "the caller rejected the transaction".to_owned(),
    }
}

fn must<T>(result: Result<T, Step>, what: &str) -> T {
    result.unwrap_or_else(|err| panic!("{what}: {}", explain(&err)))
}

#[derive(Debug)]
struct JobView {
    state: String,
    attempts: i16,
    claim_generation: i64,
    failure_reason: Option<String>,
    error_summary: Option<String>,
    claim_cleared: bool,
    finished: bool,
    not_before_us: i64,
    claim_expires_us: Option<i64>,
}

struct EngineRun {
    started: infra_jobs::Started,
    tracker: TaskTracker,
    cancel: CancellationToken,
}

impl Drop for EngineRun {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.tracker.close();
    }
}

struct Release(Arc<Notify>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

fn created(enqueued: Enqueued) -> infra_jobs::JobId {
    match enqueued {
        Enqueued::Created(id) => id,
        Enqueued::Duplicate => panic!("expected Created, got Duplicate"),
    }
}

fn probe_registry(max_attempts: u16, timeout: Duration) -> infra_jobs::Registry {
    let mut kinds = Kinds::new();
    kinds.register::<Probe>(
        Policy {
            max_attempts,
            timeout,
        },
        jobs::handle,
    );
    kinds.validate().expect("the probe registry")
}

fn locked_multi_kind_registry() -> infra_jobs::Registry {
    let mut kinds = Kinds::new();
    kinds.register::<Probe>(Policy::default(), jobs::handle);
    kinds.register::<Stranded>(Policy::default(), |job: Job<Stranded>| async move {
        sqlx::query("INSERT INTO stranded_attempts (job_id) VALUES ($1::uuid)")
            .bind(job.id().to_string())
            .execute(job.pool())
            .await?;
        job.cancellation().cancelled().await;
        Err(JobError::retryable("stranded test cancellation"))
    });
    kinds.validate().expect("the multi-kind registry")
}

fn start(pool: &PgPool, registry: infra_jobs::Registry, workers: u32) -> EngineRun {
    let engine = Engine::new(
        pool.clone(),
        registry,
        NonZeroU32::new(workers).expect("at least one worker"),
    );
    let tracker = TaskTracker::new();
    let cancel = CancellationToken::new();
    let started = engine.start(&tracker, &cancel);
    EngineRun {
        started,
        tracker,
        cancel,
    }
}

async fn open(pool: &PgPool, workers: u32) -> PgPool {
    super::template_pool(&dsn_for(pool).await, workers + 2).await
}

async fn proxied_pool(pool: &PgPool, workers: u32) -> (CommitProxy, PgPool) {
    let dsn = dsn_for(pool).await;
    assert_eq!(
        dsn.ssl_mode_name(),
        "disable",
        "the commit proxy frames the plaintext protocol"
    );
    let host = dsn.host().trim_start_matches('[').trim_end_matches(']');
    let server = tokio::net::lookup_host((host, dsn.port()))
        .await
        .expect("the server address resolves")
        .next()
        .expect("the server has an address");
    let proxy = CommitProxy::start(server).await;
    let raw = std::env::var(DATABASE_URL).expect("DATABASE_URL is set");
    let mut url = Url::parse(&raw).expect("DATABASE_URL is a URL");
    url.set_path(dsn.database());
    url.set_ip_host(proxy.address().ip())
        .expect("the proxy address is a host");
    url.set_port(Some(proxy.address().port()))
        .expect("the proxy URL accepts a port");
    let proxied = Dsn::admit(url.as_str()).expect("the proxied DSN is admitted");
    (proxy, super::template_pool(&proxied, workers).await)
}

async fn prepare(pool: &PgPool) {
    sqlx::query(jobs::CREATE_PROBE_ATTEMPTS)
        .execute(pool)
        .await
        .expect("the probe table");
}

async fn join(run: EngineRun, pools: &[&PgPool]) {
    run.cancel.cancel();
    run.tracker.close();
    super::bounded("background tasks", run.tracker.wait()).await;
    super::close(pools).await;
}

async fn finish(run: EngineRun, pools: &[&PgPool]) {
    run.started.stop_claiming();
    super::bounded("the engine drains", run.started.drained()).await;
    join(run, pools).await;
}

async fn until<T>(what: &str, bound: Duration, mut ready: impl AsyncFnMut() -> Option<T>) -> T {
    let result = tokio::time::timeout(bound, async {
        loop {
            if let Some(found) = ready().await {
                return found;
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    match result {
        Ok(found) => found,
        Err(elapsed) => panic!("{what} did not happen within {bound:?}: {elapsed}"),
    }
}

async fn absent_for(bound: Duration, mut check: impl AsyncFnMut()) {
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        check().await;
        tokio::time::sleep(POLL).await;
    }
}

async fn enqueue_one(pool: &PgPool, action: ProbeAction) -> String {
    let id = must(
        in_tx(pool, async |tx| -> Result<_, Step> {
            Ok(created(
                enqueue(tx, &Probe { action }, EnqueueOptions::default()).await?,
            ))
        })
        .await,
        "enqueue commits",
    );
    id.to_string()
}

async fn enqueue_many(pool: &PgPool, action: ProbeAction, count: i64) -> Vec<String> {
    must(
        in_tx(pool, async |tx| -> Result<Vec<String>, Step> {
            let mut ids = Vec::new();
            for _ in 0..count {
                ids.push(
                    created(enqueue(tx, &Probe { action }, EnqueueOptions::default()).await?)
                        .to_string(),
                );
            }
            Ok(ids)
        })
        .await,
        "enqueue commits",
    )
}

async fn load(pool: &PgPool, id: &str) -> JobView {
    let row = sqlx::query(
        "SELECT state, attempts, claim_generation, failure_reason, error_summary, \
         claim_expires_at IS NULL AS claim_cleared, finished_at IS NOT NULL AS finished, \
         (EXTRACT(EPOCH FROM not_before) * 1000000)::bigint AS not_before_us, \
         (EXTRACT(EPOCH FROM claim_expires_at) * 1000000)::bigint AS claim_expires_us \
         FROM background_jobs WHERE id::text = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("the job row");
    JobView {
        state: row.try_get("state").expect("state"),
        attempts: row.try_get("attempts").expect("attempts"),
        claim_generation: row.try_get("claim_generation").expect("claim_generation"),
        failure_reason: row.try_get("failure_reason").expect("failure_reason"),
        error_summary: row.try_get("error_summary").expect("error_summary"),
        claim_cleared: row.try_get("claim_cleared").expect("claim_cleared"),
        finished: row.try_get("finished").expect("finished"),
        not_before_us: row.try_get("not_before_us").expect("not_before_us"),
        claim_expires_us: row.try_get("claim_expires_us").expect("claim_expires_us"),
    }
}

async fn attempts_of(pool: &PgPool, id: &str) -> Vec<i32> {
    sqlx::query_scalar("SELECT attempt FROM probe_attempts WHERE job_id = $1::uuid ORDER BY seq")
        .bind(id)
        .fetch_all(pool)
        .await
        .expect("probe attempts")
}

async fn attempt_started_us(pool: &PgPool, id: &str, attempt: i32) -> Option<i64> {
    sqlx::query_scalar(
        "SELECT (EXTRACT(EPOCH FROM started_at) * 1000000)::bigint \
         FROM probe_attempts WHERE job_id = $1::uuid AND attempt = $2 \
         ORDER BY seq LIMIT 1",
    )
    .bind(id)
    .bind(attempt)
    .fetch_optional(pool)
    .await
    .expect("attempt start")
}

async fn db_now_us(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT (EXTRACT(EPOCH FROM clock_timestamp()) * 1000000)::bigint")
        .fetch_one(pool)
        .await
        .expect("database clock")
}

async fn set_not_before(pool: &PgPool, id: &str, seconds_ago: i64) {
    let updated = sqlx::query(
        "UPDATE background_jobs \
         SET not_before = statement_timestamp() - ($2 * interval '1 second') \
         WHERE id::text = $1",
    )
    .bind(id)
    .bind(seconds_ago)
    .execute(pool)
    .await
    .expect("not_before moves");
    assert_eq!(updated.rows_affected(), 1);
}

async fn completed(pool: &PgPool, id: &str) -> JobView {
    until("the job completes", super::WAIT, async || {
        let view = load(pool, id).await;
        (view.state == "completed").then_some(view)
    })
    .await
}

async fn first_summary(pool: &PgPool, id: &str, bound: Duration) -> JobView {
    until("the attempt records a summary", bound, async || {
        let view = load(pool, id).await;
        view.error_summary.is_some().then_some(view)
    })
    .await
}

fn probe_json(action: ProbeAction) -> String {
    serde_json::to_string(&Probe { action }).expect("probe payload")
}

async fn stage_running(pool: &PgPool, payload: &str, expired: bool) -> (String, i64) {
    let sql = if expired {
        "INSERT INTO background_jobs \
         (kind, payload, state, attempts, claim_generation, not_before, claim_expires_at) \
         VALUES ($1, $2::jsonb, 'running', 1, nextval('background_jobs_claim_generation'), \
                 statement_timestamp() - interval '1 minute', \
                 statement_timestamp() - interval '1 second') \
         RETURNING id::text AS id, claim_generation"
    } else {
        "INSERT INTO background_jobs \
         (kind, payload, state, attempts, claim_generation, not_before, claim_expires_at) \
         VALUES ($1, $2::jsonb, 'running', 1, nextval('background_jobs_claim_generation'), \
                 statement_timestamp() - interval '1 minute', \
                 statement_timestamp() + interval '30 seconds') \
         RETURNING id::text AS id, claim_generation"
    };
    let row = sqlx::query(sql)
        .bind(Probe::NAME)
        .bind(payload)
        .fetch_one(pool)
        .await
        .expect("a staged running job");
    (
        row.try_get("id").expect("id"),
        row.try_get("claim_generation").expect("claim_generation"),
    )
}

async fn make_read_only(pool: &PgPool) {
    sqlx::query(
        "DO $$ BEGIN EXECUTE format(\
         'ALTER DATABASE %I SET default_transaction_read_only = on', current_database()); END $$",
    )
    .execute(pool)
    .await
    .expect("the database default changes");
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x1_x3_two_engines_run_each_job_once(pool: PgPool) {
    let pool_a = open(&pool, 3).await;
    let pool_b = open(&pool, 3).await;
    prepare(&pool_a).await;
    let ids = enqueue_many(&pool_a, ProbeAction::Succeed, RACE_JOBS).await;
    let run_a = start(&pool_a, probe_registry(4, DEFAULT_TIMEOUT), 3);
    let run_b = start(&pool_b, probe_registry(4, DEFAULT_TIMEOUT), 3);

    until("every raced job completes", super::WAIT, async || {
        let completed: i64 =
            sqlx::query_scalar("SELECT count(*) FROM background_jobs WHERE state = 'completed'")
                .fetch_one(&pool_a)
                .await
                .expect("completed count");
        (completed == RACE_JOBS).then_some(())
    })
    .await;

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM probe_attempts")
        .fetch_one(&pool_a)
        .await
        .expect("probe count");
    let distinct: i64 = sqlx::query_scalar("SELECT count(DISTINCT job_id) FROM probe_attempts")
        .fetch_one(&pool_a)
        .await
        .expect("distinct jobs");
    let wrong_attempt: i64 =
        sqlx::query_scalar("SELECT count(*) FROM probe_attempts WHERE attempt <> 1")
            .fetch_one(&pool_a)
            .await
            .expect("attempt numbers");
    assert_eq!(rows, RACE_JOBS, "each job ran exactly once");
    assert_eq!(distinct, RACE_JOBS);
    assert_eq!(wrong_attempt, 0);
    assert_eq!(i64::try_from(ids.len()).expect("job count"), RACE_JOBS);
    finish(run_a, &[&pool_a]).await;
    finish(run_b, &[&pool_b]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x1_unknown_committed_claim_never_dispatches_a_handler(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let (proxy, worker) = proxied_pool(&pool, 1).await;
    prepare(&jobs).await;
    let id = enqueue_one(&jobs, ProbeAction::WaitForCancellation).await;
    proxy.arm((Fault::ForwardThenDrop, "WITH policy AS"));
    let run = start(&worker, probe_registry(2, DEFAULT_TIMEOUT), 1);

    let claimed = until(
        "the lost-acknowledgement claim is visible",
        super::WAIT,
        async || {
            let view = load(&jobs, &id).await;
            (view.state == "running" && view.attempts == 1).then_some(view)
        },
    )
    .await;
    assert_eq!(proxy.fired(), Some(Fault::ForwardThenDrop));
    absent_for(Duration::from_millis(500), async || {
        assert!(
            attempts_of(&jobs, &id).await.is_empty(),
            "an unknown claim must not dispatch its handler"
        );
        let current = load(&jobs, &id).await;
        assert_eq!(current.state, "running");
        assert_eq!(current.claim_generation, claimed.claim_generation);
    })
    .await;

    finish(run, &[&jobs, &worker]).await;
    proxy.shutdown().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x1_a_locked_earliest_job_is_skipped_by_a_one_slot_worker(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let locker = open(&pool, 1).await;
    prepare(&jobs).await;
    sqlx::query("CREATE TABLE stranded_attempts (job_id uuid NOT NULL)")
        .execute(&jobs)
        .await
        .expect("the second-kind attempts table");
    let locked = enqueue_one(&jobs, ProbeAction::Succeed).await;
    set_not_before(&jobs, &locked, 10).await;
    for token in [1, 2] {
        must(
            in_tx(&jobs, async |tx| -> Result<(), Step> {
                let _ = enqueue(tx, &Stranded { token }, EnqueueOptions::default()).await?;
                Ok(())
            })
            .await,
            "the other-kind job commits",
        );
    }
    let mut hold = locker.begin().await.expect("the candidate lock begins");
    sqlx::query("SELECT 1 FROM background_jobs WHERE id::text = $1 FOR UPDATE")
        .bind(&locked)
        .execute(&mut *hold)
        .await
        .expect("the earliest candidate is locked");
    // One slot: a claim that chose its ids before locking would pick only the
    // locked earliest job and claim nothing while the lock is held.
    let run = start(&jobs, locked_multi_kind_registry(), 1);
    until(
        "the next unlocked job is dispatched",
        super::WAIT,
        async || {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM stranded_attempts")
                .fetch_one(&jobs)
                .await
                .expect("second-kind attempt count");
            (count == 1).then_some(())
        },
    )
    .await;
    let locked_view = load(&jobs, &locked).await;
    assert_eq!(locked_view.state, "pending");
    assert_eq!(locked_view.attempts, 0);
    assert_eq!(attempts_of(&jobs, &locked).await, Vec::<i32>::new());
    let dispatched: i64 = sqlx::query_scalar("SELECT count(*) FROM stranded_attempts")
        .fetch_one(&jobs)
        .await
        .expect("second-kind attempt count");
    assert_eq!(dispatched, 1, "the only slot runs the next unlocked job");

    run.started.stop_claiming();
    hold.commit().await.expect("the candidate lock releases");
    let end = run.started.cancel_and_finish(RELEASE_BUDGET).await;
    assert_eq!(end.cancelled, 1);
    assert!(!end.timed_out, "{end:?}");
    join(run, &[&jobs, &locker]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x1_due_jobs_run_in_not_before_then_id_order(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let mut ids = enqueue_many(&jobs, ProbeAction::Succeed, 5).await;
    ids.sort();
    let offsets = [10_i64, 50, 20, 40, 30];
    for (id, seconds_ago) in ids.iter().zip(offsets) {
        set_not_before(&jobs, id, seconds_ago).await;
    }
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 1);

    until("every ordered job runs", super::WAIT, async || {
        let ran: i64 = sqlx::query_scalar("SELECT count(*) FROM probe_attempts")
            .fetch_one(&jobs)
            .await
            .expect("probe count");
        (ran == 5).then_some(())
    })
    .await;

    let expected: Vec<String> =
        sqlx::query_scalar("SELECT id::text FROM background_jobs ORDER BY not_before, id")
            .fetch_all(&jobs)
            .await
            .expect("claim order");
    let actual: Vec<String> =
        sqlx::query_scalar("SELECT job_id::text FROM probe_attempts ORDER BY seq")
            .fetch_all(&jobs)
            .await
            .expect("execution order");
    let by_id: Vec<String> = sqlx::query_scalar("SELECT id::text FROM background_jobs ORDER BY id")
        .fetch_all(&jobs)
        .await
        .expect("id order");
    assert_ne!(expected, by_id, "not_before must change the order");
    assert_eq!(actual, expected);
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x1_unregistered_kind_stays_pending(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let stranded = must(
        in_tx(&jobs, async |tx| -> Result<String, Step> {
            Ok(
                created(enqueue(tx, &Stranded { token: 1 }, EnqueueOptions::default()).await?)
                    .to_string(),
            )
        })
        .await,
        "the stranded job commits",
    );
    let before = load(&jobs, &stranded).await;
    enqueue_many(&jobs, ProbeAction::Succeed, 3).await;
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 1);

    until("the registered jobs complete", super::WAIT, async || {
        let completed: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM background_jobs WHERE kind = $1 AND state = 'completed'",
        )
        .bind(Probe::NAME)
        .fetch_one(&jobs)
        .await
        .expect("completed probes");
        (completed == 3).then_some(())
    })
    .await;
    absent_for(POLL_INTERVAL + Duration::from_millis(500), async || {
        let view = load(&jobs, &stranded).await;
        assert_eq!(view.state, "pending");
        assert_eq!(view.attempts, 0);
        assert_eq!(view.claim_generation, 0);
        assert!(view.failure_reason.is_none());
        assert!(view.claim_cleared);
        assert!(!view.finished);
    })
    .await;
    let after = load(&jobs, &stranded).await;
    assert_eq!(after.state, before.state);
    assert_eq!(after.attempts, before.attempts);
    assert_eq!(after.claim_generation, before.claim_generation);
    assert!(after.failure_reason.is_none());
    assert!(after.error_summary.is_none());
    assert!(attempts_of(&jobs, &stranded).await.is_empty());
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x1_claim_takes_only_free_slots(pool: PgPool) {
    let jobs = open(&pool, 2).await;
    prepare(&jobs).await;
    enqueue_many(&jobs, ProbeAction::WaitForCancellation, 5).await;
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 2);

    until("two slots fill", super::WAIT, async || {
        let running: i64 =
            sqlx::query_scalar("SELECT count(*) FROM background_jobs WHERE state = 'running'")
                .fetch_one(&jobs)
                .await
                .expect("running count");
        (running == 2 && run.started.in_flight() == 2).then_some(())
    })
    .await;

    let window = POLL_INTERVAL * 2 + Duration::from_millis(500);
    absent_for(window, async || {
        let running: i64 =
            sqlx::query_scalar("SELECT count(*) FROM background_jobs WHERE state = 'running'")
                .fetch_one(&jobs)
                .await
                .expect("running count");
        let untouched: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM background_jobs \
             WHERE state = 'pending' AND attempts = 0 AND claim_generation = 0",
        )
        .fetch_one(&jobs)
        .await
        .expect("untouched count");
        assert_eq!(running, 2, "a claim took more than the free slots");
        assert_eq!(untouched, 3);
        assert_eq!(run.started.in_flight(), 2);
    })
    .await;

    run.started.stop_claiming();
    let end = run.started.cancel_and_finish(RELEASE_BUDGET).await;
    assert!(!end.timed_out, "{end:?}");
    assert_eq!(end.cancelled, 2);
    join(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x2_idle_engine_claims_within_poll_interval(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 1);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let committed = Instant::now();
    let id = enqueue_one(&jobs, ProbeAction::Succeed).await;
    until(
        "the idle engine claims",
        Duration::from_secs(3),
        async || {
            let view = load(&jobs, &id).await;
            (view.attempts > 0 || view.state != "pending").then_some(())
        },
    )
    .await;
    assert!(
        committed.elapsed() <= Duration::from_secs(3),
        "pickup took {:?}",
        committed.elapsed()
    );
    let view = completed(&jobs, &id).await;
    assert_eq!(view.attempts, 1);
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn e2_uncommitted_enqueue_is_not_run(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let caller = open(&pool, 1).await;
    prepare(&jobs).await;
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 1);
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let id_slot = Arc::new(Mutex::new(None));
    let holder = {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let id_slot = Arc::clone(&id_slot);
        let caller = caller.clone();
        tokio::spawn(async move {
            in_tx(&caller, async move |tx| -> Result<(), Step> {
                let id = created(
                    enqueue(
                        tx,
                        &Probe {
                            action: ProbeAction::Succeed,
                        },
                        EnqueueOptions::default(),
                    )
                    .await?,
                );
                *id_slot.lock().expect("id") = Some(id.to_string());
                entered.notify_one();
                release.notified().await;
                Ok(())
            })
            .await
        })
    };
    {
        let _guard = Release(Arc::clone(&release));
        super::bounded(
            "the enqueue is inside the open transaction",
            entered.notified(),
        )
        .await;
        absent_for(Duration::from_secs(2), async || {
            assert_eq!(super::job_count(&jobs).await, 0);
            let probes: i64 = sqlx::query_scalar("SELECT count(*) FROM probe_attempts")
                .fetch_one(&jobs)
                .await
                .expect("probe count");
            assert_eq!(probes, 0);
        })
        .await;
    }
    must(
        super::bounded("the holder commits", holder)
            .await
            .expect("the holder joins"),
        "the enqueue commits",
    );
    let id = id_slot
        .lock()
        .expect("id")
        .clone()
        .expect("the committed id");
    let view = completed(&jobs, &id).await;
    assert_eq!(view.attempts, 1);
    finish(run, &[&jobs, &caller]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x3_static_lease_is_timeout_plus_recovery_reserve(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let id = enqueue_one(&jobs, ProbeAction::WaitForCancellation).await;
    let policy_timeout = MIN_TIMEOUT;
    let run = start(&jobs, probe_registry(2, policy_timeout), 1);
    let claimed = until("the claim is running", super::WAIT, async || {
        let view = load(&jobs, &id).await;
        (view.state == "running").then_some(view)
    })
    .await;
    let remaining = claimed
        .claim_expires_us
        .expect("a running claim has an expiry")
        - db_now_us(&jobs).await;
    let expected = i64::try_from((policy_timeout + LEASE_RESERVE).as_micros())
        .expect("lease fits i64 microseconds");
    assert!(
        remaining > expected - 2_000_000 && remaining <= expected,
        "static lease remaining {remaining}us is not the {expected}us policy lease"
    );
    absent_for(Duration::from_millis(250), async || {
        let current = load(&jobs, &id).await;
        assert_eq!(current.claim_expires_us, claimed.claim_expires_us);
        assert_eq!(current.claim_generation, claimed.claim_generation);
    })
    .await;
    run.started.stop_claiming();
    let end = run.started.cancel_and_finish(RELEASE_BUDGET).await;
    assert!(!end.timed_out, "{end:?}");
    join(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x4_expired_claim_is_recovered_and_a_live_claim_is_not(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let payload = probe_json(ProbeAction::Succeed);
    let (expired, expired_generation) = stage_running(&jobs, &payload, true).await;
    let (live, live_generation) = stage_running(&jobs, &payload, false).await;
    let live_before = load(&jobs, &live).await;
    let run = start(&jobs, probe_registry(3, DEFAULT_TIMEOUT), 1);

    let recovered = completed(&jobs, &expired).await;
    assert_eq!(recovered.attempts, 2);
    assert!(recovered.claim_generation > expired_generation);
    assert!(recovered.finished);
    assert!(recovered.claim_cleared);
    assert_eq!(attempts_of(&jobs, &expired).await, vec![2]);

    let live_after = load(&jobs, &live).await;
    assert_eq!(live_after.state, "running");
    assert_eq!(live_after.attempts, 1);
    assert_eq!(live_after.claim_generation, live_generation);
    assert_eq!(live_after.claim_generation, live_before.claim_generation);
    assert_eq!(live_after.claim_expires_us, live_before.claim_expires_us);
    assert_eq!(live_after.not_before_us, live_before.not_before_us);
    assert!(live_after.failure_reason.is_none());
    assert!(!live_after.finished);
    assert!(attempts_of(&jobs, &live).await.is_empty());
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x5_superseded_attempt_does_not_change_the_row(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let gate = Arc::new(GateState {
        entered: Notify::new(),
        open: Notify::new(),
    });
    let registry = gate_registry(Arc::clone(&gate));
    let id = must(
        in_tx(&jobs, async |tx| -> Result<String, Step> {
            Ok(
                created(enqueue(tx, &Gate { token: 1 }, EnqueueOptions::default()).await?)
                    .to_string(),
            )
        })
        .await,
        "the gate job commits",
    );
    let run = start(&jobs, registry, 1);
    super::bounded("the handler is waiting", gate.entered.notified()).await;

    let updated = sqlx::query(
        "UPDATE background_jobs \
         SET attempts = attempts + 1, \
             claim_generation = nextval('background_jobs_claim_generation'), \
             claim_expires_at = statement_timestamp() + interval '30 seconds' \
         WHERE id::text = $1 AND state = 'running'",
    )
    .bind(&id)
    .execute(&jobs)
    .await
    .expect("the newer claim");
    assert_eq!(updated.rows_affected(), 1);
    let newer = load(&jobs, &id).await;
    assert_eq!(newer.state, "running");
    assert!(newer.claim_expires_us.is_some());
    gate.open.notify_one();

    until("the superseded attempt ends", super::WAIT, async || {
        (run.started.in_flight() == 0).then_some(())
    })
    .await;
    let after = load(&jobs, &id).await;
    assert_eq!(after.state, "running");
    assert_eq!(after.attempts, newer.attempts);
    assert_eq!(after.claim_generation, newer.claim_generation);
    assert_eq!(after.claim_expires_us, newer.claim_expires_us);
    assert_eq!(after.not_before_us, newer.not_before_us);
    assert!(after.failure_reason.is_none());
    assert!(after.error_summary.is_none());
    assert!(!after.finished);
    assert!(!after.claim_cleared);
    finish(run, &[&jobs]).await;
}

struct GateState {
    entered: Notify,
    open: Notify,
}

struct TransactionGateState {
    business_written: Notify,
    complete: Notify,
}

struct OutcomeGateState {
    entered: Notify,
    complete: Notify,
    runs: AtomicUsize,
}

fn gate_registry(gate: Arc<GateState>) -> infra_jobs::Registry {
    let mut kinds = Kinds::new();
    kinds.register::<Gate>(
        Policy {
            max_attempts: 2,
            timeout: DEFAULT_TIMEOUT,
        },
        move |job: Job<Gate>| {
            let gate = Arc::clone(&gate);
            async move {
                let _ = job;
                gate.entered.notify_one();
                gate.open.notified().await;
                Ok::<(), JobError>(())
            }
        },
    );
    kinds.validate().expect("the gate registry")
}

fn transactional_gate_registry(gate: Arc<TransactionGateState>) -> infra_jobs::Registry {
    let mut kinds = Kinds::new();
    kinds.register::<TransactionGate>(
        Policy {
            max_attempts: 2,
            timeout: DEFAULT_TIMEOUT,
        },
        move |job: Job<TransactionGate>| {
            let gate = Arc::clone(&gate);
            async move {
                let mut direct = job.pool().acquire().await?;
                if !matches!(
                    job.complete_in_tx(&mut direct).await,
                    Err(infra_jobs::CompleteError::NoTransaction)
                ) {
                    return Err(JobError::permanent(
                        "complete_in_tx accepted a connection without a transaction",
                    ));
                }
                drop(direct);
                let completed = in_tx(job.pool(), async |tx| -> Result<(), Step> {
                    sqlx::query("INSERT INTO job_effects (job_id) VALUES ($1::uuid)")
                        .bind(job.id().to_string())
                        .execute(&mut *connection(tx))
                        .await
                        .map_err(Step::Query)?;
                    gate.business_written.notify_one();
                    gate.complete.notified().await;
                    job.complete_in_tx(connection(tx))
                        .await
                        .map_err(|error| match error {
                            infra_jobs::CompleteError::Database(error) => Step::Query(error),
                            _ => Step::Rejected,
                        })
                })
                .await;
                completed.map_err(|error| match error {
                    Step::Tx(TxError::CommitUnknown(error)) => JobError::transaction_unknown(error),
                    error => JobError::retryable(explain(&error)),
                })
            }
        },
    );
    kinds.validate().expect("the transactional gate registry")
}

fn outcome_gate_registry(gate: Arc<OutcomeGateState>) -> infra_jobs::Registry {
    let mut kinds = Kinds::new();
    kinds.register::<Gate>(Policy::default(), move |job: Job<Gate>| {
        let gate = Arc::clone(&gate);
        async move {
            let _ = job;
            gate.runs.fetch_add(1, Ordering::Relaxed);
            gate.entered.notify_one();
            gate.complete.notified().await;
            Ok(())
        }
    });
    kinds.validate().expect("the outcome gate registry")
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x3_unknown_outcome_retries_its_fenced_write_without_rerunning_the_handler(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let (proxy, worker) = proxied_pool(&pool, 1).await;
    let gate = Arc::new(OutcomeGateState {
        entered: Notify::new(),
        complete: Notify::new(),
        runs: AtomicUsize::new(0),
    });
    let id = must(
        in_tx(&jobs, async |tx| -> Result<String, Step> {
            Ok(
                created(enqueue(tx, &Gate { token: 1 }, EnqueueOptions::default()).await?)
                    .to_string(),
            )
        })
        .await,
        "the outcome gate job commits",
    );
    let run = start(&worker, outcome_gate_registry(Arc::clone(&gate)), 1);
    super::bounded(
        "the handler reaches its result gate",
        gate.entered.notified(),
    )
    .await;
    proxy.arm((Fault::ForwardThenDrop, "SET state = 'completed'"));
    gate.complete.notify_one();

    let view = completed(&jobs, &id).await;
    assert_eq!(proxy.fired(), Some(Fault::ForwardThenDrop));
    assert_eq!(view.attempts, 1);
    assert_eq!(gate.runs.load(Ordering::Relaxed), 1);
    finish(run, &[&jobs, &worker]).await;
    proxy.shutdown().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x6_stale_transactional_completion_rolls_back_prior_business_writes(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    sqlx::query("CREATE TABLE job_effects (job_id uuid NOT NULL)")
        .execute(&jobs)
        .await
        .expect("the business-effects table");
    let gate = Arc::new(TransactionGateState {
        business_written: Notify::new(),
        complete: Notify::new(),
    });
    let id = must(
        in_tx(&jobs, async |tx| -> Result<String, Step> {
            Ok(created(
                enqueue(tx, &TransactionGate { token: 1 }, EnqueueOptions::default()).await?,
            )
            .to_string())
        })
        .await,
        "the transactional gate job commits",
    );
    let run = start(&jobs, transactional_gate_registry(Arc::clone(&gate)), 1);
    super::bounded(
        "the business write is pending",
        gate.business_written.notified(),
    )
    .await;
    let superseded = sqlx::query(
        "UPDATE background_jobs \
         SET claim_generation = nextval('background_jobs_claim_generation') \
         WHERE id::text = $1 AND state = 'running'",
    )
    .bind(&id)
    .execute(&jobs)
    .await
    .expect("the claim becomes stale");
    assert_eq!(superseded.rows_affected(), 1);
    gate.complete.notify_one();

    until("the stale handler ends", super::WAIT, async || {
        (run.started.in_flight() == 0).then_some(())
    })
    .await;
    let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM job_effects")
        .fetch_one(&jobs)
        .await
        .expect("business effects count");
    assert_eq!(
        effects, 0,
        "a stale completion must roll back its prior effect"
    );
    let view = load(&jobs, &id).await;
    assert_eq!(view.state, "running");
    assert_eq!(view.attempts, 1);
    assert!(!view.claim_cleared);
    finish(run, &[&jobs]).await;
}

async fn transaction_unknown_leaves_only_the_commit_side_effect(
    pool: &PgPool,
    fault: Fault,
    expected_effects: i64,
    expected_state: &str,
) {
    let jobs = open(pool, 1).await;
    let (proxy, worker) = proxied_pool(pool, 1).await;
    sqlx::query("CREATE TABLE job_effects (job_id uuid NOT NULL)")
        .execute(&jobs)
        .await
        .expect("the business-effects table");
    let gate = Arc::new(TransactionGateState {
        business_written: Notify::new(),
        complete: Notify::new(),
    });
    let id = must(
        in_tx(&jobs, async |tx| -> Result<String, Step> {
            Ok(created(
                enqueue(tx, &TransactionGate { token: 1 }, EnqueueOptions::default()).await?,
            )
            .to_string())
        })
        .await,
        "the transactional gate job commits",
    );
    let run = start(&worker, transactional_gate_registry(Arc::clone(&gate)), 1);
    super::bounded(
        "the transaction is ready to commit",
        gate.business_written.notified(),
    )
    .await;
    proxy.arm((fault, "SET state = 'completed'"));
    gate.complete.notify_one();
    until("the unknown-commit handler ends", super::WAIT, async || {
        (run.started.in_flight() == 0).then_some(())
    })
    .await;
    assert_eq!(proxy.fired(), Some(fault));
    let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM job_effects")
        .fetch_one(&jobs)
        .await
        .expect("business effects count");
    assert_eq!(effects, expected_effects);
    let view = load(&jobs, &id).await;
    assert_eq!(view.state, expected_state);
    assert_eq!(view.attempts, 1);
    if expected_state == "running" {
        assert!(!view.claim_cleared);
    } else {
        assert!(view.claim_cleared);
    }
    finish(run, &[&jobs, &worker]).await;
    proxy.shutdown().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x6_transaction_unknown_after_commit_does_not_replay_the_business_closure(pool: PgPool) {
    transaction_unknown_leaves_only_the_commit_side_effect(
        &pool,
        Fault::ForwardThenDrop,
        1,
        "completed",
    )
    .await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x6_transaction_unknown_before_commit_leaves_the_job_for_expiry(pool: PgPool) {
    transaction_unknown_leaves_only_the_commit_side_effect(
        &pool,
        Fault::DropBeforeForward,
        0,
        "running",
    )
    .await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn w4_known_result_beats_forced_release_while_its_write_waits(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    let locker = open(&pool, 1).await;
    let gate = Arc::new(GateState {
        entered: Notify::new(),
        open: Notify::new(),
    });
    let id = must(
        in_tx(&jobs, async |tx| -> Result<String, Step> {
            Ok(
                created(enqueue(tx, &Gate { token: 1 }, EnqueueOptions::default()).await?)
                    .to_string(),
            )
        })
        .await,
        "the gate job commits",
    );
    let run = start(&jobs, gate_registry(Arc::clone(&gate)), 1);
    super::bounded("the gate handler starts", gate.entered.notified()).await;

    let mut hold = locker
        .begin()
        .await
        .expect("the row-lock transaction begins");
    sqlx::query("SELECT 1 FROM background_jobs WHERE id::text = $1 FOR UPDATE")
        .bind(&id)
        .execute(&mut *hold)
        .await
        .expect("the row is locked");
    gate.open.notify_one();
    super::wait_for_lock_waiter(&jobs).await;

    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            run.started.cancel_and_finish(RELEASE_BUDGET),
        )
        .await
        .is_err(),
        "forced cleanup must wait for the blocked known result"
    );
    hold.commit().await.expect("the row lock releases");
    let end = run.started.cancel_and_finish(RELEASE_BUDGET).await;
    assert_eq!(
        end,
        DrainEnd {
            known_results: 1,
            cancelled: 0,
            released: 0,
            uncertain: 0,
            timed_out: false,
        }
    );
    let view = load(&jobs, &id).await;
    assert_eq!(view.state, "completed");
    assert_eq!(view.attempts, 1);
    assert!(view.claim_cleared);
    join(run, &[&jobs, &locker]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x5_rolled_back_generation_never_returns(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let drawn = Arc::new(Mutex::new(0_i64));
    let slot = Arc::clone(&drawn);
    let rolled = in_tx(&jobs, async move |tx| -> Result<(), Step> {
        let value: i64 = sqlx::query_scalar("SELECT nextval('background_jobs_claim_generation')")
            .fetch_one(&mut *connection(tx))
            .await?;
        *slot.lock().expect("generation") = value;
        Err(Step::Rejected)
    })
    .await;
    match rolled {
        Err(Step::Rejected) => {}
        Err(err) => panic!("the generation draw should roll back: {}", explain(&err)),
        Ok(()) => panic!("the generation draw committed"),
    }
    let drawn = *drawn.lock().expect("generation");
    assert!(drawn > 0);
    let id = enqueue_one(&jobs, ProbeAction::Succeed).await;
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 1);
    let view = completed(&jobs, &id).await;
    assert!(
        view.claim_generation > drawn,
        "claim generation {} reused rolled-back {drawn}",
        view.claim_generation
    );
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x6_spent_budget_fails_exhausted_without_running(pool: PgPool) {
    let jobs = open(&pool, 2).await;
    prepare(&jobs).await;
    let payload = probe_json(ProbeAction::Succeed);
    let fresh = stage_spent(&jobs, &payload, None).await;
    let kept = stage_spent(&jobs, &payload, Some("kept earlier")).await;
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 2);

    let fresh_view = until("the fresh budget is exhausted", super::WAIT, async || {
        let view = load(&jobs, &fresh).await;
        (view.state == "failed").then_some(view)
    })
    .await;
    let kept_view = until("the kept summary is exhausted", super::WAIT, async || {
        let view = load(&jobs, &kept).await;
        (view.state == "failed").then_some(view)
    })
    .await;
    assert_eq!(fresh_view.failure_reason.as_deref(), Some("exhausted"));
    assert_eq!(fresh_view.attempts, 2);
    assert_eq!(
        fresh_view.error_summary.as_deref(),
        Some("attempt budget spent")
    );
    assert!(fresh_view.finished);
    assert!(fresh_view.claim_cleared);
    assert_eq!(kept_view.failure_reason.as_deref(), Some("exhausted"));
    assert_eq!(kept_view.attempts, 2);
    assert_eq!(kept_view.error_summary.as_deref(), Some("kept earlier"));
    assert!(attempts_of(&jobs, &fresh).await.is_empty());
    assert!(attempts_of(&jobs, &kept).await.is_empty());
    finish(run, &[&jobs]).await;
}

async fn stage_spent(pool: &PgPool, payload: &str, summary: Option<&str>) -> String {
    let row = sqlx::query(
        "INSERT INTO background_jobs (kind, payload, state, attempts, not_before, error_summary) \
         VALUES ($1, $2::jsonb, 'pending', 2, statement_timestamp() - interval '1 second', $3) \
         RETURNING id::text AS id",
    )
    .bind(Probe::NAME)
    .bind(payload)
    .bind(summary)
    .fetch_one(pool)
    .await
    .expect("a spent job");
    row.try_get("id").expect("id")
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x7_success_completes_and_clears_the_claim(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let id = enqueue_one(&jobs, ProbeAction::Succeed).await;
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 1);
    let view = completed(&jobs, &id).await;
    assert_eq!(view.attempts, 1);
    assert!(view.finished);
    assert!(view.claim_cleared);
    assert!(view.failure_reason.is_none());
    assert_eq!(attempts_of(&jobs, &id).await, vec![1]);
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x7_x8_retryable_failure_waits_about_one_second_then_runs_again(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let id = enqueue_one(&jobs, ProbeAction::FailRetryable).await;
    let run = start(&jobs, probe_registry(3, DEFAULT_TIMEOUT), 1);
    let started_us = until("the first attempt starts", super::WAIT, async || {
        attempt_started_us(&jobs, &id, 1).await
    })
    .await;
    let failed = until(
        "the retry is scheduled",
        Duration::from_secs(5),
        async || {
            let view = load(&jobs, &id).await;
            let scheduled = view.state == "pending"
                && view.attempts == 1
                && view.error_summary.as_deref() == Some("probe failed retryably");
            scheduled.then_some(view)
        },
    )
    .await;
    let observed = db_now_us(&jobs).await;
    let lower = started_us + 900_000 - 50_000;
    let upper = observed + 1_100_000;
    assert!(
        failed.not_before_us >= lower && failed.not_before_us <= upper,
        "not_before {} outside {lower}..={upper} (started {started_us}, observed {observed})",
        failed.not_before_us
    );
    assert!(failed.claim_cleared);
    let second = until("the retry runs", super::WAIT, async || {
        attempt_started_us(&jobs, &id, 2).await
    })
    .await;
    assert!(
        second >= failed.not_before_us,
        "retry started at {second} before not_before {}",
        failed.not_before_us
    );
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x8_retry_after_uses_the_requested_database_delay(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let id = enqueue_one(&jobs, ProbeAction::RetryAfter { millis: 2_000 }).await;
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 1);
    let started = until("the retry-after attempt starts", super::WAIT, async || {
        attempt_started_us(&jobs, &id, 1).await
    })
    .await;
    let scheduled = until(
        "the retry-after delivery schedules",
        super::WAIT,
        async || {
            let view = load(&jobs, &id).await;
            (view.state == "pending" && view.attempts == 1).then_some(view)
        },
    )
    .await;
    let observed = db_now_us(&jobs).await;
    assert!(
        scheduled.not_before_us >= started + 1_800_000
            && scheduled.not_before_us <= observed + 2_100_000,
        "retry-after scheduled {} outside the requested two-second window from {started} to {observed}",
        scheduled.not_before_us,
    );
    assert_eq!(
        scheduled.error_summary.as_deref(),
        Some("probe requested retry delay")
    );
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x8_retry_after_still_exhausts(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let id = enqueue_one(&jobs, ProbeAction::RetryAfter { millis: 2_000 }).await;
    let run = start(&jobs, probe_registry(1, DEFAULT_TIMEOUT), 1);

    let failed = until(
        "the retry-after failure is terminal at the cap",
        super::WAIT,
        async || {
            let view = load(&jobs, &id).await;
            (view.state == "failed").then_some(view)
        },
    )
    .await;
    assert_eq!(failed.attempts, 1);
    assert_eq!(failed.failure_reason.as_deref(), Some("exhausted"));
    assert_eq!(
        failed.error_summary.as_deref(),
        Some("probe requested retry delay")
    );
    assert!(failed.finished);
    assert!(failed.claim_cleared);
    assert_eq!(attempts_of(&jobs, &id).await, vec![1]);
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x8_snooze_refunds_once_and_can_run_after_the_attempt_cap(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let id = enqueue_one(&jobs, ProbeAction::SnoozeOnce { millis: 2_000 }).await;
    let enqueued = load(&jobs, &id).await;
    let run = start(&jobs, probe_registry(1, DEFAULT_TIMEOUT), 1);
    let started = until("the snoozing attempt starts", super::WAIT, async || {
        attempt_started_us(&jobs, &id, 1).await
    })
    .await;
    let snoozed = until("the first delivery snoozes", super::WAIT, async || {
        let view = load(&jobs, &id).await;
        (view.state == "pending"
            && view.attempts == 0
            && view.claim_generation > enqueued.claim_generation
            && view.not_before_us > enqueued.not_before_us)
            .then_some(view)
    })
    .await;
    assert!(snoozed.claim_cleared);
    assert!(snoozed.not_before_us >= started + 2_000_000);
    assert!(snoozed.failure_reason.is_none());
    assert!(snoozed.error_summary.is_none());
    assert!(
        snoozed.not_before_us > db_now_us(&jobs).await,
        "snooze must not be claimable before its requested database time"
    );
    let completed = completed(&jobs, &id).await;
    assert_eq!(completed.attempts, 1);
    assert_eq!(attempts_of(&jobs, &id).await, vec![1, 1]);
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x7_retryable_on_the_last_unit_is_exhausted(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let id = enqueue_one(&jobs, ProbeAction::FailRetryable).await;
    let run = start(&jobs, probe_registry(1, DEFAULT_TIMEOUT), 1);
    let view = until("the last unit is exhausted", super::WAIT, async || {
        let view = load(&jobs, &id).await;
        (view.state == "failed").then_some(view)
    })
    .await;
    assert_eq!(view.failure_reason.as_deref(), Some("exhausted"));
    assert_eq!(
        view.error_summary.as_deref(),
        Some("probe failed retryably")
    );
    assert_eq!(view.attempts, 1);
    assert!(view.finished);
    assert!(view.claim_cleared);
    assert_eq!(attempts_of(&jobs, &id).await, vec![1]);
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x7_permanent_failure_is_terminal(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let id = enqueue_one(&jobs, ProbeAction::FailPermanent).await;
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 1);
    let view = until(
        "the permanent failure is recorded",
        super::WAIT,
        async || {
            let view = load(&jobs, &id).await;
            (view.state == "failed").then_some(view)
        },
    )
    .await;
    assert_eq!(view.failure_reason.as_deref(), Some("permanent"));
    assert_eq!(
        view.error_summary.as_deref(),
        Some("probe failed permanently")
    );
    assert_eq!(view.attempts, 1);
    assert!(view.finished);
    assert!(view.claim_cleared);
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn h3_x7_panic_is_retryable_and_the_engine_keeps_running(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let id = enqueue_one(&jobs, ProbeAction::Panic).await;
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 1);
    let failed = until(
        "the panic is recorded",
        Duration::from_secs(5),
        async || {
            let view = load(&jobs, &id).await;
            let pending = view.state == "pending"
                && view.attempts == 1
                && view.error_summary.as_deref() == Some("handler panicked");
            pending.then_some(view)
        },
    )
    .await;
    assert!(failed.claim_cleared);
    let later = enqueue_one(&jobs, ProbeAction::Succeed).await;
    let view = completed(&jobs, &later).await;
    assert_eq!(view.attempts, 1);
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x9_timeout_is_recorded(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let id = enqueue_one(&jobs, ProbeAction::Sleep { millis: 10_000 }).await;
    let run = start(&jobs, probe_registry(2, MIN_TIMEOUT), 1);
    let view = until(
        "the timeout is recorded",
        Duration::from_secs(8),
        async || {
            let view = load(&jobs, &id).await;
            let timed_out = view.state == "pending"
                && view.attempts == 1
                && view.error_summary.as_deref() == Some("attempt timed out after 1s");
            timed_out.then_some(view)
        },
    )
    .await;
    assert!(view.claim_cleared);
    assert!(view.failure_reason.is_none());
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x7_undecodable_payload_is_retryable_without_the_handler(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let row = sqlx::query(
        "INSERT INTO background_jobs (kind, payload, state, not_before) \
         VALUES ($1, '{\"action\":\"no_such_action\"}'::jsonb, 'pending', \
                 statement_timestamp()) \
         RETURNING id::text AS id",
    )
    .bind(Probe::NAME)
    .fetch_one(&jobs)
    .await
    .expect("the undecodable job");
    let id: String = row.try_get("id").expect("id");
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 1);
    let view = first_summary(&jobs, &id, Duration::from_secs(5)).await;
    assert_eq!(view.state, "pending");
    assert_eq!(view.attempts, 1);
    let summary = view.error_summary.expect("a decode summary");
    assert!(
        summary.starts_with("payload does not decode as test.probe: data error at line 1 column"),
        "{summary}"
    );
    assert!(attempts_of(&jobs, &id).await.is_empty());
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x11_retention_deletes_only_old_terminal_rows(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    sqlx::query(
        "INSERT INTO background_jobs (kind, payload, state, finished_at, not_before) \
         SELECT CASE WHEN g % 2 = 0 THEN 'test.probe' ELSE 'test.stranded' END, \
                '{}'::jsonb, 'completed', \
                statement_timestamp() - interval '25 hours', \
                statement_timestamp() - interval '25 hours' \
         FROM generate_series(1, 1201) AS g",
    )
    .execute(&jobs)
    .await
    .expect("old completed rows");
    for kind in [Probe::NAME, Stranded::NAME] {
        stage_terminal(&jobs, kind, "completed", None, 23 * 3_600).await;
        stage_terminal(&jobs, kind, "failed", Some("permanent"), 8 * 86_400).await;
        stage_terminal(&jobs, kind, "failed", Some("exhausted"), 6 * 86_400).await;
        stage_live(&jobs, kind, false).await;
        stage_live(&jobs, kind, true).await;
    }
    let before = super::job_count(&jobs).await;
    let engine = Engine::new(
        jobs.clone(),
        probe_registry(2, DEFAULT_TIMEOUT),
        NonZeroU32::new(1).expect("one worker"),
    );
    let deleted = engine.remove_expired().await.expect("retention");
    let after = super::job_count(&jobs).await;
    assert_eq!(deleted, 1203);
    assert_eq!(
        before - after,
        i64::try_from(deleted).expect("deleted count")
    );
    let old_completed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM background_jobs \
         WHERE state = 'completed' AND finished_at <= statement_timestamp() - interval '24 hours'",
    )
    .fetch_one(&jobs)
    .await
    .expect("old completed");
    let old_failed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM background_jobs \
         WHERE state = 'failed' AND finished_at <= statement_timestamp() - interval '7 days'",
    )
    .fetch_one(&jobs)
    .await
    .expect("old failed");
    assert_eq!(old_completed, 0);
    assert_eq!(old_failed, 0);
    assert_eq!(count_state(&jobs, "completed").await, 2);
    assert_eq!(count_state(&jobs, "failed").await, 2);
    assert_eq!(count_state(&jobs, "pending").await, 2);
    assert_eq!(count_state(&jobs, "running").await, 2);
    for kind in [Probe::NAME, Stranded::NAME] {
        assert_eq!(count_kind_state(&jobs, kind, "pending").await, 1);
        assert_eq!(count_kind_state(&jobs, kind, "running").await, 1);
    }
    super::close(&[&jobs]).await;
}

async fn stage_terminal(
    pool: &PgPool,
    kind: &str,
    state: &str,
    reason: Option<&str>,
    seconds_ago: i64,
) {
    let inserted = sqlx::query(
        "INSERT INTO background_jobs \
         (kind, payload, state, failure_reason, finished_at, not_before) \
         VALUES ($1, '{}'::jsonb, $2, $3, \
                 statement_timestamp() - ($4 * interval '1 second'), \
                 statement_timestamp() - ($4 * interval '1 second'))",
    )
    .bind(kind)
    .bind(state)
    .bind(reason)
    .bind(seconds_ago)
    .execute(pool)
    .await
    .expect("a terminal row");
    assert_eq!(inserted.rows_affected(), 1);
}

async fn stage_live(pool: &PgPool, kind: &str, running: bool) {
    let sql = if running {
        "INSERT INTO background_jobs \
         (kind, payload, state, attempts, claim_generation, not_before, claim_expires_at) \
         VALUES ($1, '{}'::jsonb, 'running', 1, \
                 nextval('background_jobs_claim_generation'), \
                 statement_timestamp() - interval '40 days', \
                 statement_timestamp() + interval '30 seconds')"
    } else {
        "INSERT INTO background_jobs (kind, payload, state, not_before) \
         VALUES ($1, '{}'::jsonb, 'pending', \
                 statement_timestamp() - interval '40 days')"
    };
    let inserted = sqlx::query(sql)
        .bind(kind)
        .execute(pool)
        .await
        .expect("a live row");
    assert_eq!(inserted.rows_affected(), 1);
}

async fn count_state(pool: &PgPool, state: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM background_jobs WHERE state = $1")
        .bind(state)
        .fetch_one(pool)
        .await
        .expect("a state count")
}

async fn count_kind_state(pool: &PgPool, kind: &str, state: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM background_jobs WHERE kind = $1 AND state = $2")
        .bind(kind)
        .bind(state)
        .fetch_one(pool)
        .await
        .expect("a kind count")
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn x12_future_not_before_is_not_claimed_until_it_passes(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let id = must(
        in_tx(&jobs, async |tx| -> Result<String, Step> {
            Ok(created(
                enqueue(
                    tx,
                    &Probe {
                        action: ProbeAction::Succeed,
                    },
                    EnqueueOptions {
                        delay: Duration::from_secs(3_600),
                        unique_key: None,
                    },
                )
                .await?,
            )
            .to_string())
        })
        .await,
        "the delayed job commits",
    );
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 1);
    absent_for(Duration::from_secs(2), async || {
        let view = load(&jobs, &id).await;
        assert_eq!(view.state, "pending");
        assert_eq!(view.attempts, 0);
        assert_eq!(view.claim_generation, 0);
    })
    .await;
    set_not_before(&jobs, &id, 1).await;
    let view = completed(&jobs, &id).await;
    assert_eq!(view.attempts, 1);
    finish(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn w4_x7_cancel_and_finish_returns_the_budget_unit(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let id = enqueue_one(&jobs, ProbeAction::WaitForCancellation).await;
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 1);
    let claimed = until("the claim is in flight", super::WAIT, async || {
        let view = load(&jobs, &id).await;
        (view.state == "running" && run.started.in_flight() == 1).then_some(view)
    })
    .await;
    run.started.stop_claiming();
    let end = run.started.cancel_and_finish(RELEASE_BUDGET).await;
    assert_eq!(
        end,
        DrainEnd {
            known_results: 0,
            cancelled: 1,
            released: 1,
            uncertain: 0,
            timed_out: false,
        }
    );
    let released = load(&jobs, &id).await;
    assert_eq!(released.state, "pending");
    assert_eq!(released.attempts, 0);
    assert!(released.claim_cleared);
    assert_eq!(
        released.not_before_us, claimed.not_before_us,
        "a released job keeps its place in claim order"
    );
    assert!(released.failure_reason.is_none());
    join(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn w4_drain_waits_for_the_in_flight_attempt_and_claims_nothing_new(pool: PgPool) {
    let jobs = open(&pool, 1).await;
    prepare(&jobs).await;
    let id = enqueue_one(&jobs, ProbeAction::Sleep { millis: 1_500 }).await;
    let run = start(&jobs, probe_registry(2, DEFAULT_TIMEOUT), 1);
    until("the sleep is in flight", super::WAIT, async || {
        let view = load(&jobs, &id).await;
        (view.state == "running" && run.started.in_flight() == 1).then_some(())
    })
    .await;
    run.started.stop_claiming();
    let later = enqueue_one(&jobs, ProbeAction::Succeed).await;
    super::bounded("the drain ends after the attempt", async {
        let drain = run.started.drained();
        tokio::pin!(drain);
        let mut saw_running = false;
        loop {
            tokio::select! {
                biased;
                () = &mut drain => {
                    let view = load(&jobs, &id).await;
                    assert_eq!(view.state, "completed", "drained resolved before completion");
                    assert_eq!(view.attempts, 1);
                    assert!(view.finished);
                    assert!(view.claim_cleared);
                    assert_eq!(run.started.in_flight(), 0);
                    assert!(saw_running, "the attempt was still running after stop_claiming");
                    break;
                }
                () = tokio::time::sleep(POLL) => {
                    if load(&jobs, &id).await.state != "completed" {
                        saw_running = true;
                    }
                }
            }
        }
    })
    .await;
    absent_for(POLL_INTERVAL + Duration::from_millis(500), async || {
        let view = load(&jobs, &later).await;
        assert_eq!(view.state, "pending");
        assert_eq!(view.attempts, 0);
        assert_eq!(view.claim_generation, 0);
        assert!(attempts_of(&jobs, &later).await.is_empty());
    })
    .await;
    join(run, &[&jobs]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn w2_check_startup_accepts_a_migrated_writer_and_refuses_read_only(pool: PgPool) {
    let dsn = dsn_for(&pool).await;
    let writer = super::template_pool(&dsn, 3).await;
    let engine = Engine::new(
        writer.clone(),
        probe_registry(2, DEFAULT_TIMEOUT),
        NonZeroU32::new(1).expect("one worker"),
    );
    assert_eq!(engine.check_startup().await, Ok(()));
    make_read_only(&pool).await;
    let reader = super::template_pool(&dsn, 3).await;
    let read_only = Engine::new(
        reader.clone(),
        probe_registry(2, DEFAULT_TIMEOUT),
        NonZeroU32::new(1).expect("one worker"),
    );
    assert_eq!(
        read_only.check_startup().await,
        Err(StartupError::NotWritable)
    );
    super::close(&[&writer, &reader]).await;
}
