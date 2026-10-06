//! Explicitly selected, release-only PostgreSQL laboratory entry.
//!
//! The canonical shell owner admits the target, migrates it, clones the frozen
//! seed before each cell, and supplies the immutable manifest plus fresh resource
//! readback. No policy environment variable changes the production adapters.
//! `DATABASE_URL` is never copied to evidence or diagnostics.
#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "ignored laboratory fixture"
)]

#[allow(
    dead_code,
    reason = "shared native transport exposes additional faults to sibling suites"
)]
#[path = "../support/commit_proxy.rs"]
mod commit_proxy;
mod evidence;
mod workload;

use evidence::{Evidence, Family, Manifest, OperationEvent, OperationRecord, Outcome, Segment};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    task::JoinSet,
    time::{Instant, sleep_until, timeout},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use workload::{Client, Result, failed};

type Journal = Arc<Mutex<Evidence>>;
const DEADLINE: Duration = Duration::from_secs(2);
const STOP_BUDGET: Duration = Duration::from_secs(15);

async fn role_connection(
    mode: &str,
    role: &str,
) -> Result<(infra_postgres::Dsn, Option<Arc<commit_proxy::CommitProxy>>)> {
    let raw = std::env::var("DATABASE_URL").map_err(|_| failed("admitted DATABASE_URL missing"))?;
    let original = infra_postgres::Dsn::admit(&raw)?;
    if mode != "composed" || role != "service1" {
        return Ok((original, None));
    }
    let mut target =
        url::Url::parse(&raw).map_err(|_| failed("invalid admitted laboratory URL"))?;
    if !target
        .query_pairs()
        .any(|(key, value)| key == "sslmode" && value == "disable")
    {
        return Err(failed(
            "native fault transport requires plaintext task-local PostgreSQL",
        ));
    }
    let address = tokio::net::lookup_host((
        target
            .host_str()
            .ok_or_else(|| failed("laboratory host missing"))?,
        target
            .port()
            .ok_or_else(|| failed("laboratory port missing"))?,
    ))
    .await?
    .next()
    .ok_or_else(|| failed("laboratory address missing"))?;
    let proxy = Arc::new(commit_proxy::CommitProxy::start(address).await);
    target
        .set_host(Some("127.0.0.1"))
        .map_err(|_| failed("fault transport host failed"))?;
    target
        .set_port(Some(proxy.address().port()))
        .map_err(|()| failed("fault transport port failed"))?;
    Ok((infra_postgres::Dsn::admit(target.as_str())?, Some(proxy)))
}

async fn fixture_time(start: Instant, seconds: u64, cancel: &CancellationToken) -> Result<()> {
    tokio::select! {
        biased;
        ()=cancel.cancelled()=>Err(failed("composed disturbance cancelled")),
        ()=sleep_until(start+Duration::from_secs(seconds))=>Ok(()),
    }
}

async fn await_boundary(path: &Path, deadline: Instant, cancel: &CancellationToken) -> Result<()> {
    tokio::time::timeout_at(deadline, async {
        while !path.is_file() {
            tokio::select! {
                biased;
                ()=cancel.cancelled()=>return Err(failed("role boundary cancelled")),
                ()=tokio::time::sleep(Duration::from_millis(10))=>{},
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| failed("role boundary did not finish within shutdown budget"))?
}

async fn join_task(mut task: tokio::task::JoinHandle<Result<()>>, deadline: Instant) -> Result<()> {
    match tokio::time::timeout_at(deadline, &mut task).await {
        Ok(result) => result?,
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(failed(
                "role auxiliary task exceeded the shared shutdown deadline",
            ))
        }
    }
}

async fn composed_sampler_fault(
    proxy: Arc<commit_proxy::CommitProxy>,
    start: Instant,
    cancel: CancellationToken,
    journal: Journal,
) -> Result<()> {
    for second in (180..=300).step_by(20) {
        fixture_time(start, second, &cancel).await?;
        // Only the dated population read in this one service owner is selected;
        // ordinary transactions, receipt observations and other owners relay.
        proxy.arm((
            commit_proxy::Fault::DropBeforeForward,
            "SELECT expires_at FROM http_idempotency_records",
        ));
        append(
            &journal,
            &json!({"event":"composed_fault","kind":"sampler_commit","stage":"armed","elapsed_ns":ns(start.elapsed())}),
        )?;
    }
    fixture_time(start, 340, &cancel).await?;
    if proxy.fired() != Some(commit_proxy::Fault::DropBeforeForward) {
        return Err(failed(
            "composed sampler fault did not reach native read acknowledgement",
        ));
    }
    append(
        &journal,
        &json!({"event":"composed_fault","kind":"sampler_commit","stage":"released","elapsed_ns":ns(start.elapsed())}),
    )?;
    Ok(())
}

async fn composed_cleanup_fault(
    pool: infra_postgres::PgPool,
    start: Instant,
    cancel: CancellationToken,
    journal: Journal,
) -> Result<()> {
    fixture_time(start, 120, &cancel).await?;
    let before = workload::composed_failure(&pool, true).await?;
    append(
        &journal,
        &json!({"event":"composed_fault","kind":"later_batch","stage":"armed","elapsed_ns":ns(start.elapsed()),"cohort":before}),
    )?;
    fixture_time(start, 240, &cancel).await?;
    let after = workload::composed_failure(&pool, false).await?;
    append(
        &journal,
        &json!({"event":"composed_fault","kind":"later_batch","stage":"released","elapsed_ns":ns(start.elapsed()),"cohort":after}),
    )?;
    Ok(())
}

/// Input written atomically by the existing task lifecycle owner every 5s.
/// The owner observes the registered target and *all four* process roles.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resources {
    target_identity: String,
    observed_unix_ms: u64,
    free_disk_bytes: u64,
    database_bytes: u64,
    evidence_bytes: u64,
    total_task_memory_bytes: u64,
    application_rss_bytes: u64,
    container_block_read_bytes: u64,
    driver_cpu_fraction: f64,
    oom: bool,
    unexpected_resources: bool,
    inputs_hash: String,
    uncontended_host: bool,
    clock_valid: bool,
}

fn env_path(key: &str) -> Result<PathBuf> {
    std::env::var_os(key)
        .map(PathBuf::from)
        .ok_or_else(|| failed("required laboratory path absent"))
}
fn unix_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}
fn ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn inventory_input(manifest: &Manifest) -> Result<workload::Inventory> {
    let positive = |name: &str| {
        manifest.effective_inputs[name]
            .as_u64()
            .filter(|value| *value > 0)
            .ok_or_else(|| failed("positive frozen inventory input missing"))
    };
    let inventory = workload::Inventory {
        seed: positive("inventory_seed")?,
        generation: positive("inventory_generation")?,
        live: positive("live_bodies")?,
        idempotency_rows: positive("idempotency_rows")?,
        receipt_rows: positive("receipt_rows")?,
        jobs_rows: positive("jobs_rows")?,
    };
    if inventory.seed != 41001
        || inventory.generation != 3
        || (inventory.jobs_rows < 200 || inventory.jobs_rows % 200 != 0)
    {
        return Err(failed(
            "frozen inventory seed or generation differs from contract",
        ));
    }
    Ok(inventory)
}

fn preparation_mode(mode: &str) -> bool {
    matches!(mode, "inventory" | "inventory-adjust" | "seed")
}

fn seed_started(manifest: &Manifest) -> Result<u64> {
    manifest.effective_inputs["seed_started_unix_ms"]
        .as_u64()
        .filter(|started| *started > 0)
        .ok_or_else(|| failed("cumulative per-regime seed start missing"))
}

fn seed_clock(manifest: &Manifest) -> Result<[u64; 3]> {
    let started = seed_started(manifest)?;
    let attempt = manifest.effective_inputs["seed_attempt_started_unix_ms"]
        .as_u64()
        .ok_or_else(|| failed("seed attempt start missing"))?;
    let prior = manifest.effective_inputs["seed_elapsed_before_attempt_ms"]
        .as_u64()
        .ok_or_else(|| failed("prior active seed debit missing"))?;
    if attempt < started || prior >= 1_500_000 {
        return Err(failed("invalid seed lineage or prior debit"));
    }
    Ok([started, attempt, prior])
}

fn seed_elapsed(manifest: &Manifest) -> Result<u64> {
    seed_elapsed_at(manifest, unix_ms()?)
}

fn seed_elapsed_at(manifest: &Manifest, now: u64) -> Result<u64> {
    let [_, attempt, prior] = seed_clock(manifest)?;
    let active = now
        .checked_sub(attempt)
        .ok_or_else(|| failed("seed attempt clock moved backwards"))?;
    prior
        .checked_add(active)
        .ok_or_else(|| failed("seed active time overflow"))
}

#[test]
fn active_seed_time_retains_failed_work_without_charging_verified_absence() {
    let manifest: Manifest = serde_json::from_value(json!({
        "config":{"attempt_id":"clock","policy":"P0","regime":"Resident","repeat":1,"seed":41001},
        "source_tree_hash":"source","executable_sha256":"executable","policy_patch_sha256":"patch",
        "image_digest":"image","toolchain":"pinned","features":["integration"],"target_identity":"task",
        "effective_inputs":{"seed_started_unix_ms":1000,"seed_attempt_started_unix_ms":100000,"seed_elapsed_before_attempt_ms":45457}
    })).unwrap();
    assert_eq!(seed_elapsed_at(&manifest, 100500).unwrap(), 45_957);
    assert!(seed_elapsed_at(&manifest, 99_999).is_err());
}

fn seed_remaining(manifest: &Manifest) -> Result<Duration> {
    let elapsed = seed_elapsed(manifest)?;
    if elapsed >= 1_500_000 {
        return Err(failed(
            "cumulative 25-minute active seed envelope exhausted",
        ));
    }
    Ok(Duration::from_millis(1_500_000 - elapsed))
}

/// Closed seconds have passed every operation's two-second client deadline.
/// Each service owns 82 predetermined arrivals per second. The controller sums
/// both writers, so one writer's fraction is never mistaken for the whole load.
#[derive(Deserialize, Serialize)]
struct OrdinaryWindow {
    closed_seconds: usize,
    expected: Vec<u64>,
}

fn ordinary_window(
    directory: &Path,
    owner: u64,
    expected: &[u64],
    elapsed: Duration,
) -> Result<()> {
    let closed_seconds = usize::try_from(elapsed.as_secs().saturating_sub(3))?.min(expected.len());
    let temporary = directory.join(format!("service{owner}.ordinary.tmp"));
    let destination = directory.join(format!("service{owner}.ordinary.json"));
    fs::write(
        &temporary,
        serde_json::to_vec(&OrdinaryWindow {
            closed_seconds,
            expected: expected[..closed_seconds].to_vec(),
        })?,
    )?;
    fs::rename(temporary, destination)?;
    Ok(())
}

fn excessive_errors(directory: &Path) -> Result<bool> {
    let mut windows = Vec::new();
    for owner in 0..2 {
        let path = directory.join(format!("service{owner}.ordinary.json"));
        if !path.is_file() {
            return Ok(false);
        }
        let window: OrdinaryWindow = serde_json::from_slice(&fs::read(path)?)?;
        if window.closed_seconds != window.expected.len()
            || window.expected.iter().any(|count| *count > 82)
        {
            return Err(failed("ordinary failure window malformed"));
        }
        windows.push(window);
    }
    let end = windows
        .iter()
        .map(|window| window.closed_seconds)
        .min()
        .unwrap_or(0);
    if end < 30 {
        return Ok(false);
    }
    let expected: u64 = windows
        .iter()
        .flat_map(|window| window.expected[end - 30..end].iter())
        .sum();
    Ok(expected * 100 < 30 * 164 * 95)
}

fn record_operation(
    record: OperationRecord,
    records: &mut Vec<OperationRecord>,
    expected: &mut [u64],
    journal: &Journal,
) -> Result<()> {
    if record.outcome == Outcome::Expected
        && record.completed_ns.saturating_sub(record.planned_ns) <= ns(DEADLINE)
    {
        let second = usize::try_from(record.planned_ns / 1_000_000_000)?;
        *expected
            .get_mut(second)
            .ok_or_else(|| failed("operation outside fixed schedule"))? += 1;
    }
    append(journal, &OperationEvent::Completed { record: &record })?;
    records.push(record);
    Ok(())
}
static CONTEXT: Mutex<Option<(String, Segment)>> = Mutex::new(None);
fn append(journal: &Journal, event: &impl serde::Serialize) -> Result<()> {
    let mut event = serde_json::to_value(event)?;
    if let Some((role, segment)) = CONTEXT
        .lock()
        .map_err(|_| failed("event context poisoned"))?
        .as_ref()
    {
        event["role"] = json!(role);
        event["segment"] = json!(segment);
    }
    journal
        .lock()
        .map_err(|_| failed("evidence writer poisoned"))?
        .append(&event)?;
    Ok(())
}

fn resource_sample(path: &Path, manifest: &Manifest) -> Result<Resources> {
    let sample: Resources = serde_json::from_slice(&fs::read(path)?)?;
    let now = unix_ms()?;
    if sample.target_identity != manifest.target_identity
        || sample.observed_unix_ms > now
        || now - sample.observed_unix_ms > 30_000
    {
        return Err(failed(
            "target identity, collector freshness or clock failure",
        ));
    }
    if !sample.clock_valid
        || sample.inputs_hash
            != manifest.effective_inputs["inputs_hash"]
                .as_str()
                .unwrap_or("")
    {
        return Err(failed("clock or effective inputs changed"));
    }
    if sample.oom
        || sample.unexpected_resources
        || sample.free_disk_bytes < 12 * 1024_u64.pow(3)
        || sample.database_bytes > 20 * 1024_u64.pow(3)
        || sample.evidence_bytes > 2 * 1024_u64.pow(3)
        || sample.application_rss_bytes > 2 * 1024_u64.pow(3)
    {
        return Err(failed("laboratory resource stop"));
    }
    Ok(sample)
}

async fn checked_database_sample(
    pool: &infra_postgres::PgPool,
    manifest: &Manifest,
) -> Result<serde_json::Value> {
    let sample = workload::database_sample(pool).await?;
    let expected = &manifest.effective_inputs["database_config"];
    if !expected.is_object() || sample["database_config"] != *expected {
        return Err(failed(
            "database settings, table options, or schema changed from frozen input",
        ));
    }
    Ok(sample)
}

struct Children(Vec<Child>);
impl Children {
    fn start(directory: &Path, segment: Segment, seconds: u64, offset: u64) -> Result<Self> {
        let executable = std::env::current_exe()?;
        let mut children = Self(Vec::new());
        for role in ["service1", "worker0", "worker1"] {
            children.0.push(
                Command::new(&executable)
                    .args([
                        "sustained_postgres",
                        "--exact",
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env("POSTGRES_SUSTAINED_ROLE", role)
                    .env("POSTGRES_SUSTAINED_BARRIER", directory)
                    .env(
                        "POSTGRES_SUSTAINED_SEGMENT",
                        serde_json::to_string(&segment)?,
                    )
                    .env("POSTGRES_SUSTAINED_SECONDS", seconds.to_string())
                    .env("POSTGRES_SUSTAINED_OFFSET", offset.to_string())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .spawn()?,
            );
        }
        Ok(children)
    }
    async fn join(&mut self) -> Result<()> {
        let deadline = Instant::now() + STOP_BUDGET;
        for child in &mut self.0 {
            loop {
                if let Some(status) = child.try_wait()? {
                    if !status.success() {
                        return Err(failed("laboratory role failed; retain attempt"));
                    }
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(failed("role did not stop within join budget"));
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        self.0.clear();
        Ok(())
    }
}
impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

async fn barrier(directory: &Path, role: &str, controller: bool) -> Result<Instant> {
    fs::write(directory.join(format!("{role}.ready")), b"ready")?;
    timeout(Duration::from_secs(30), async {
        if controller {
            while !["service1", "worker0", "worker1"]
                .iter()
                .all(|role| directory.join(format!("{role}.ready")).is_file())
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            // Future absolute wall time is used only to align the initial barrier;
            // each role converts once to a monotonic deadline and records lag.
            fs::write(directory.join("start"), (unix_ms()? + 250).to_string())?;
        }
        while !directory.join("start").is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let start: u64 = fs::read_to_string(directory.join("start"))?.parse()?;
        Ok::<_, workload::Error>(
            Instant::now() + Duration::from_millis(start.saturating_sub(unix_ms()?)),
        )
    })
    .await
    .map_err(|_| failed("role readiness barrier timed out"))?
}

async fn ordinary(
    client: Client,
    owner: u64,
    inventory: workload::Inventory,
    segment: Segment,
    seconds: u64,
    offset: u64,
    seed: u64,
    start: Instant,
    stop: CancellationToken,
    journal: Journal,
    directory: &Path,
) -> Result<Vec<OperationRecord>> {
    let mut work: JoinSet<Result<OperationRecord>> = JoinSet::new();
    let mut records = Vec::new();
    let mut mixed = owner;
    let mut wide = owner;
    let mut expected_per_second = vec![0; usize::try_from(seconds)?];
    let mut last_window = None;
    loop {
        let mixed_at = Duration::from_nanos(mixed * 10_000_000);
        let wide_at = Duration::from_nanos(wide * 15_625_000);
        let is_wide = wide_at < mixed_at;
        let planned = if is_wide { wide_at } else { mixed_at };
        if planned >= Duration::from_secs(seconds) {
            break;
        }
        if !stop.is_cancelled() {
            tokio::select! {
              biased;
              () = stop.cancelled() => {},
              joined = work.join_next(), if !work.is_empty() => {
                  let record = joined.ok_or_else(|| failed("missing joined operation"))???;
                  record_operation(record, &mut records, &mut expected_per_second, &journal)?;
                  continue;
              }
              () = sleep_until(start + planned) => {}
            }
        }
        let sequence = if is_wide {
            let n = wide;
            wide += 2;
            n
        } else {
            let n = mixed;
            mixed += 2;
            n
        };
        let action = workload::action(
            seed,
            sequence + offset * if is_wide { 64 } else { 100 },
            is_wide,
            inventory,
        );
        let id = (sequence + offset * 164) * 2 + u64::from(is_wide);
        let expected = format!("{:?}", action.kind);
        append(
            &journal,
            &json!({"event":"operation_action","id":id,"action":action}),
        )?;
        let planned_ns = ns(planned);
        append(
            &journal,
            &OperationEvent::Planned {
                id,
                family: action.family,
                segment,
                planned_ns,
                expected: &expected,
            },
        )?;
        let elapsed = start.elapsed();
        if last_window != Some(elapsed.as_secs()) {
            ordinary_window(directory, owner, &expected_per_second, elapsed)?;
            last_window = Some(elapsed.as_secs());
        }
        if stop.is_cancelled() || work.len() >= 32 || Instant::now() >= start + planned + DEADLINE {
            let record = OperationRecord {
                id,
                family: action.family,
                segment,
                planned_ns,
                started_ns: None,
                completed_ns: ns(start.elapsed()),
                outcome: Outcome::NotStarted,
                expected,
                actual: if stop.is_cancelled() {
                    "cancelled_before_admission"
                } else {
                    "inflight_or_schedule_deadline"
                }
                .into(),
            };
            record_operation(record, &mut records, &mut expected_per_second, &journal)?;
            continue;
        }
        let client = client.clone();
        let journal = journal.clone();
        let cancel = stop.clone();
        work.spawn(async move {
            let started_ns = ns(start.elapsed());
            append(&journal, &OperationEvent::Started { id, started_ns })?;
            let result = tokio::select! {
                biased;
                () = cancel.cancelled() => None,
                result = tokio::time::timeout_at(start + planned + DEADLINE, client.operation(&action)) => Some(result)
            };
            let (outcome, actual) = match result {
                Some(Ok(Ok(()))) => (Outcome::Expected, "expected"),
                Some(Ok(Err(error))) => {
                    if error.downcast_ref::<std::io::Error>().is_some() { cancel.cancel(); (Outcome::Unexpected, "oracle_failure") }
                    else { (Outcome::Unknown, "adapter_failure") }
                }
                Some(Err(_)) => (Outcome::TimedOut, "deadline_unknown"),
                None => (Outcome::Unknown, "cancelled_unknown"),
            };
            Ok::<_, workload::Error>(OperationRecord { id, family: action.family, segment, planned_ns, started_ns: Some(started_ns), completed_ns: ns(start.elapsed()), outcome, expected, actual: actual.into() })
        });
    }
    while let Some(joined) = work.join_next().await {
        let record = joined??;
        record_operation(record, &mut records, &mut expected_per_second, &journal)?;
    }
    ordinary_window(directory, owner, &expected_per_second, start.elapsed())?;
    for family in [
        Family::Jobs,
        Family::Idempotency,
        Family::Webhook,
        Family::WideReplay,
    ] {
        append(
            &journal,
            &json!({"event":"ordinary_distribution", "family":family, "segment":segment,
            "distribution": evidence::summarize_operations(records.iter().filter(|r| r.family==family))}),
        )?;
    }
    Ok(records)
}

async fn observe(
    pool: infra_postgres::PgPool,
    path: PathBuf,
    manifest: Manifest,
    journal: Journal,
    cancel: CancellationToken,
    start: Instant,
    seconds: u64,
    directory: PathBuf,
    ordinary_traffic: bool,
    composed: bool,
) -> Result<()> {
    let mut high_memory = false;
    let mut sample = 0_u64;
    loop {
        tokio::select! { biased; ()=cancel.cancelled()=>return Ok(()), ()=sleep_until(start+Duration::from_secs(sample*5))=>{} }
        if sample * 5 > seconds {
            return Ok(());
        }
        let resources = resource_sample(&path, &manifest)?;
        let excessive = resources.total_task_memory_bytes > 4 * 1024_u64.pow(3);
        if high_memory && excessive {
            return Err(failed("two consecutive task memory violations"));
        }
        high_memory = excessive;
        // The declared composed disturbances have their own retained outcomes.
        // Resume the ordinary stop rule once their final 30-second window closes.
        let disturbance_window = composed && (120..430).contains(&start.elapsed().as_secs());
        if ordinary_traffic && !disturbance_window && excessive_errors(&directory)? {
            return Err(failed(
                "ordinary error fraction exceeded five percent for thirty seconds",
            ));
        }
        append(
            &journal,
            &json!({"event":"resource_sample", "elapsed_ns":ns(start.elapsed()), "container_block_read_bytes":resources.container_block_read_bytes,"driver_cpu_fraction":resources.driver_cpu_fraction,"total_task_memory_bytes":resources.total_task_memory_bytes,"inputs_hash":resources.inputs_hash,"uncontended_host":resources.uncontended_host,"clock_valid":resources.clock_valid}),
        )?;
        if sample % 6 == 0 {
            let relations = timeout(Duration::from_secs(8), workload::relation_snapshot(&pool))
                .await
                .map_err(|_| failed("relation sample exceeded collector budget"))??;
            append(
                &journal,
                &json!({"event":"database_sample", "elapsed_ns":ns(start.elapsed()), "relations":relations}),
            )?;
        }
        let counters = timeout(
            Duration::from_secs(8),
            checked_database_sample(&pool, &manifest),
        )
        .await
        .map_err(|_| failed("database counters exceeded collector budget"))??;
        append(
            &journal,
            &json!({"event":"database_counters","elapsed_ns":ns(start.elapsed()),"sample":counters}),
        )?;
        let inventory = timeout(Duration::from_secs(8), workload::inventory(&pool))
            .await
            .map_err(|_| failed("cohort inventory exceeded collector budget"))??;
        append(
            &journal,
            &json!({"event":if sample*5==seconds {"final_inventory"} else {"inventory"},"elapsed_ns":ns(start.elapsed()),"inventory":inventory}),
        )?;
        if sample * 5 == seconds {
            return Ok(());
        }
        sample += 1;
    }
}

async fn run_role(
    manifest: &Manifest,
    role: &str,
    directory: &Path,
    segment: Segment,
    seconds: u64,
    offset: u64,
    journal: Journal,
) -> Result<()> {
    *CONTEXT
        .lock()
        .map_err(|_| failed("event context poisoned"))? = Some((role.to_owned(), segment));
    let mode = std::env::var("POSTGRES_SUSTAINED_MODE").unwrap_or_else(|_| "cell".into());
    let (dsn, proxy) = role_connection(&mode, role).await?;
    let pool = workload::pool(&dsn, 4).await?;
    let tracker = TaskTracker::new();
    let cancel = CancellationToken::new();
    let _scope = RoleStop {
        tracker: tracker.clone(),
        cancel: cancel.clone(),
    };
    let inventory = inventory_input(manifest)?;
    let live = inventory.live;
    let client = Client::new(pool.clone(), manifest.config.seed, inventory)?;
    let engine = if role.starts_with("worker") {
        Some(workload::worker(pool.clone())?)
    } else {
        None
    };
    if let Some(engine) = &engine {
        engine.check_startup().await?;
    } else {
        client.store.check_startup().await?;
    }
    let observer_pool = if role == "service0" {
        Some(workload::pool(&dsn, 2).await?)
    } else {
        None
    };
    if let Some(control) = &observer_pool {
        let resources =
            resource_sample(&env_path("POSTGRES_SUSTAINED_RESOURCE_SAMPLE")?, manifest)?;
        append(
            &journal,
            &json!({"event":"resource_sample","elapsed_ns":0,"container_block_read_bytes":resources.container_block_read_bytes,"driver_cpu_fraction":resources.driver_cpu_fraction,"inputs_hash":resources.inputs_hash,"uncontended_host":resources.uncontended_host,"clock_valid":resources.clock_valid}),
        )?;
        append(
            &journal,
            &json!({"event":"native_dml_provenance","fixture_dml_during_measurement":preparation_mode(&mode)}),
        )?;
        append(
            &journal,
            &json!({"event":"inventory","elapsed_ns":0,"inventory":workload::inventory(control).await?}),
        )?;
        append(
            &journal,
            &json!({"event":"database_sample","elapsed_ns":0,"relations":workload::relation_snapshot(control).await?}),
        )?;
        append(
            &journal,
            &json!({"event":"database_counters","elapsed_ns":0,"sample":checked_database_sample(control, manifest).await?}),
        )?;
    }
    append(
        &journal,
        &json!({"event":"metrics_baseline","elapsed_ns":0,"observed_unix_ms":unix_ms()?,"text":METRICS.get().ok_or_else(||failed("metrics recorder unavailable"))?.render()}),
    )?;
    let start = barrier(directory, role, role == "service0").await?;
    *CLOCK.lock().map_err(|_| failed("clock poisoned"))? = Some(start);
    sleep_until(start).await;
    append(
        &journal,
        &json!({"event":"role_started","role":role,"barrier_lag_ns":ns(start.elapsed())}),
    )?;
    let role_directory = directory.to_owned();
    let watch_cancel = cancel.clone();
    tracker.spawn(async move {
        while !watch_cancel.is_cancelled() {
            if role_directory.join("stop").exists()
                || TRACE_FAILED.load(std::sync::atomic::Ordering::Relaxed)
            {
                watch_cancel.cancel();
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    let recorder = METRICS
        .get()
        .ok_or_else(|| failed("metrics recorder unavailable"))?
        .clone();
    let metrics_journal = journal.clone();
    let metrics_cancel = cancel.clone();
    tracker.spawn(async move {
        while !metrics_cancel.is_cancelled() {
            if append(&metrics_journal, &json!({"event":"metrics", "elapsed_ns":ns(start.elapsed()), "text":recorder.render(), "observed_unix_ms":unix_ms().unwrap_or_default()})).is_err() {
                metrics_cancel.cancel(); break;
            }
            tokio::select! { ()=metrics_cancel.cancelled()=>break, ()=tokio::time::sleep(Duration::from_secs(5))=>{} }
        }
    });

    let mut observer = None;
    let mut vacuum = None;
    let mut composed = None;
    if mode == "composed" {
        let stop = cancel.clone();
        let journal = journal.clone();
        if let Some(proxy) = proxy.as_ref() {
            let proxy = proxy.clone();
            composed = Some(tokio::spawn(async move {
                let result = composed_sampler_fault(proxy, start, stop.clone(), journal).await;
                if result.is_err() {
                    stop.cancel();
                }
                result
            }));
        } else if let Some(pool) = observer_pool.as_ref() {
            let pool = pool.clone();
            composed = Some(tokio::spawn(async move {
                let result = composed_cleanup_fault(pool, start, stop.clone(), journal).await;
                if result.is_err() {
                    stop.cancel();
                }
                result
            }));
        }
    }
    if let Some(pool) = observer_pool.as_ref() {
        let pool = pool.clone();
        let input = manifest.clone();
        let observer_journal = journal.clone();
        let stop = cancel.clone();
        let directory = directory.to_owned();
        let resource_path = env_path("POSTGRES_SUSTAINED_RESOURCE_SAMPLE")?;
        let ordinary_traffic = !preparation_mode(&mode);
        let composed_mode = mode == "composed";
        observer = Some(tokio::spawn(async move {
            let result = observe(
                pool,
                resource_path,
                input,
                observer_journal,
                stop.clone(),
                start,
                seconds,
                directory.clone(),
                ordinary_traffic,
                composed_mode,
            )
            .await;
            if result.is_err() {
                stop.cancel();
                let _ = fs::write(directory.join("stop"), b"observer stop");
            }
            result
        }));
        if segment != Segment::Warmup && !preparation_mode(&mode) {
            let pool = observer_pool.as_ref().expect("observer pool").clone();
            let journal = journal.clone();
            let stop = cancel.clone();
            vacuum = Some(tokio::spawn(async move {
                tokio::select! { ()=stop.cancelled()=>return Err(failed("vacuum cancelled")), ()=sleep_until(start+Duration::from_secs(60))=>{} }
                append(
                    &journal,
                    &json!({"event":"vacuum_started","elapsed_ns":ns(start.elapsed())}),
                )?;
                let result = tokio::select! { ()=stop.cancelled()=>Err(failed("vacuum cancelled")), result=workload::vacuum(&pool)=>result };
                append(
                    &journal,
                    &json!({"event":"vacuum_finished","elapsed_ns":ns(start.elapsed()),"complete":result.is_ok()}),
                )?;
                result
            }));
        }
    }
    let started = engine
        .as_ref()
        .map(|engine| engine.start(&tracker, &cancel));
    let mut result: Result<()> = async {
    let result = if preparation_mode(&mode) && role == "service0" {
        let remaining = seed_remaining(manifest)?.saturating_sub(STOP_BUDGET);
        let prepared = async {
            if mode == "seed" {
                let mut receipt = workload::precondition(&client,live).await?;
                receipt["total_elapsed_ms"] = json!(seed_elapsed(manifest)?);
                append(&journal,&json!({"event":"preconditioning","receipt":receipt}))?;
            } else {
                workload::seed(&client,live).await?;
                workload::await_settled(&client).await?;
            }
            Ok(())
        };
        let result: Result<()> = tokio::select! {
            ()=cancel.cancelled()=>Err(failed("preparation stopped")),
            result=timeout(remaining,prepared)=>result.map_err(|_| failed("cumulative 25-minute preparation bound"))?
        };
        fs::write(directory.join("stop"), b"preparation stopped")?;
        result
    } else if preparation_mode(&mode) && role == "service1" {
        tokio::select! { ()=cancel.cancelled()=>{}, ()=sleep_until(start+Duration::from_secs(seconds))=>{} }
        Ok(())
    } else if role.starts_with("service") {
        client.start_maintenance(&tracker, &cancel);
        let owner = u64::from(role == "service1");
        ordinary(
            client.clone(),
            owner,
            inventory,
            segment,
            seconds,
            offset,
            manifest.config.seed,
            start,
            cancel.clone(),
            journal.clone(),
            directory,
        )
        .await
        .and_then(|records| {
            if cancel.is_cancelled() {
                Err(failed("ordinary segment cancelled; all remaining arrivals retained"))
            } else if records
                .iter()
                .any(|record| record.outcome == Outcome::Unexpected)
            {
                Err(failed("correctness oracle failed"))
            } else if records.len() as u64 != seconds * 82 {
                Err(failed("arrivals stopped before complete segment"))
            } else {
                Ok(())
            }
        })
    } else {
        tokio::select! { ()=cancel.cancelled()=>{}, ()=sleep_until(start + Duration::from_secs(seconds))=>{} };
        Ok(())
    };
    if result.is_ok() && !preparation_mode(&mode) {
        sleep_until(start + Duration::from_secs(seconds)).await;
    }
    result
    }.await;
    let shutdown_deadline = if preparation_mode(&mode) {
        Instant::now() + STOP_BUDGET
    } else {
        (start + Duration::from_secs(seconds) + STOP_BUDGET).min(Instant::now() + STOP_BUDGET)
    };
    if result.is_ok() && !preparation_mode(&mode) {
        let settling: Result<()> = async {
            if role.starts_with("service") {
                fs::write(directory.join(format!("{role}.ordinary_done")), b"done")?;
            }
            if role == "service0" {
                let settle_deadline = shutdown_deadline - Duration::from_secs(5);
                await_boundary(
                    &directory.join("service1.ordinary_done"),
                    settle_deadline,
                    &cancel,
                )
                .await?;
                tokio::time::timeout_at(settle_deadline, workload::await_settled(&client))
                    .await
                    .map_err(|_| {
                        failed("native queued effects did not settle before shutdown")
                    })??;
                fs::write(directory.join("ordinary.drained"), b"settled")?;
            } else {
                await_boundary(
                    &directory.join("ordinary.drained"),
                    shutdown_deadline,
                    &cancel,
                )
                .await?;
            }
            Ok(())
        }
        .await;
        retain_error(&mut result, settling);
    }
    cancel.cancel();
    if result.is_err() {
        retain_error(
            &mut result,
            fs::write(directory.join("stop"), b"role stop").map_err(Into::into),
        );
    }
    if let Some(observer) = observer {
        retain_error(&mut result, join_task(observer, shutdown_deadline).await);
    }
    if let Some(vacuum) = vacuum {
        retain_error(&mut result, join_task(vacuum, shutdown_deadline).await);
    }
    if let Some(composed) = composed {
        retain_error(&mut result, join_task(composed, shutdown_deadline).await);
    }
    if let Some(control) = observer_pool {
        let final_samples: Result<()> = tokio::time::timeout_at(shutdown_deadline, async {
            let resources=resource_sample(&env_path("POSTGRES_SUSTAINED_RESOURCE_SAMPLE")?,manifest)?;
            append(&journal,&json!({"event":"resource_sample","elapsed_ns":ns(start.elapsed()),"container_block_read_bytes":resources.container_block_read_bytes,"driver_cpu_fraction":resources.driver_cpu_fraction,"inputs_hash":resources.inputs_hash,"uncontended_host":resources.uncontended_host,"clock_valid":resources.clock_valid}))?;
            append(&journal,&json!({"event":"database_sample","elapsed_ns":ns(start.elapsed()),"relations":workload::relation_snapshot(&control).await?}))?;
            append(&journal,&json!({"event":"database_counters","elapsed_ns":ns(start.elapsed()),"sample":checked_database_sample(&control, manifest).await?}))?;
            append(&journal,&json!({"event":"post_stop_inventory","elapsed_ns":ns(start.elapsed()),"inventory":workload::inventory(&control).await?}))?;
            Ok(())
        }).await.unwrap_or_else(|_|Err(failed("final collector exceeded shared shutdown deadline")));
        retain_error(&mut result, final_samples);
        if infra_postgres::close(
            &control,
            shutdown_deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(5)),
        )
        .await
            != infra_postgres::Closed::Complete
        {
            retain_error(&mut result, Err(failed("observer pool remains")));
        }
    }
    if let Some(started) = started {
        started.stop_claiming();
        let drain = started
            .cancel_and_finish(
                shutdown_deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(5)),
            )
            .await;
        retain_error(
            &mut result,
            append(
                &journal,
                &json!({"event":"worker_drained","uncertain":drain.uncertain,"timed_out":drain.timed_out}),
            ),
        );
        if drain.timed_out || drain.uncertain != 0 {
            retain_error(
                &mut result,
                Err(failed("worker drain incomplete or uncertain")),
            );
        }
    }
    tracker.close();
    let joined = tokio::time::timeout_at(shutdown_deadline, tracker.wait())
        .await
        .is_ok();
    let closed = infra_postgres::close(
        &pool,
        shutdown_deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(5)),
    )
    .await
        == infra_postgres::Closed::Complete;
    if !joined || !closed {
        retain_error(
            &mut result,
            Err(failed("native role did not stop completely")),
        );
    }
    if let Some(proxy) = proxy {
        match Arc::try_unwrap(proxy) {
            Ok(proxy) => proxy.shutdown().await,
            Err(_) => retain_error(&mut result, Err(failed("fault transport still owned"))),
        }
    }
    retain_error(
        &mut result,
        append(
            &journal,
            &json!({"event":"metrics","elapsed_ns":ns(start.elapsed()),"observed_unix_ms":unix_ms().unwrap_or_default(),"text":METRICS.get().expect("recorder").render()}),
        ),
    );
    if joined && closed {
        retain_error(
            &mut result,
            append(&journal, &json!({"event":"role_stopped","role":role})),
        );
    }
    result
}

fn retain_error(result: &mut Result<()>, next: Result<()>) {
    if result.is_ok() {
        *result = next;
    }
}

static METRICS: std::sync::OnceLock<metrics_exporter_prometheus::PrometheusHandle> =
    std::sync::OnceLock::new();

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit release laboratory only; canonical PostgreSQL suite owns correctness"]
async fn sustained_postgres() {
    if entry().await.is_err() {
        panic!(
            "sustained PostgreSQL attempt incomplete; retain its evidence and lifecycle receipts"
        );
    }
}

async fn entry() -> Result<()> {
    let mode = std::env::var("POSTGRES_SUSTAINED_MODE").unwrap_or_else(|_| "cell".into());
    if preparation_mode(&mode) {
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(env_path("POSTGRES_SUSTAINED_MANIFEST")?)?)?;
        timeout(seed_remaining(&manifest)?, entry_run())
            .await
            .map_err(|_| failed("cumulative 25-minute preparation lifecycle bound"))?
    } else {
        entry_run().await
    }
}

async fn entry_run() -> Result<()> {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new()
        .set_buckets_for_metric(
            metrics_exporter_prometheus::Matcher::Full(
                "postgres_cleanup_pass_duration_seconds".to_owned(),
            ),
            &[
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 15.0, 30.0, 60.0,
                120.0, 300.0, 600.0, 1200.0, 1800.0, 3600.0,
            ],
        )?
        .build_recorder();
    METRICS
        .set(recorder.handle())
        .map_err(|_| failed("metrics already installed"))?;
    metrics::set_global_recorder(recorder).map_err(|_| failed("global recorder unavailable"))?;
    if cfg!(debug_assertions) {
        return Err(failed("laboratory requires the release executable"));
    }
    let mut manifest: Manifest =
        serde_json::from_slice(&fs::read(env_path("POSTGRES_SUSTAINED_MANIFEST")?)?)?;
    let root = env_path("POSTGRES_SUSTAINED_EVIDENCE")?;
    let role = std::env::var("POSTGRES_SUSTAINED_ROLE").unwrap_or_else(|_| "service0".into());
    let child = role != "service0";
    if child {
        let segment: Segment = serde_json::from_str(&std::env::var("POSTGRES_SUSTAINED_SEGMENT")?)?;
        let seconds: u64 = std::env::var("POSTGRES_SUSTAINED_SECONDS")?.parse()?;
        let offset: u64 = std::env::var("POSTGRES_SUSTAINED_OFFSET")?.parse()?;
        manifest.config.attempt_id = format!("{}-{:?}-{role}", manifest.config.attempt_id, segment);
        let journal = Arc::new(Mutex::new(Evidence::create(&root, &manifest)?));
        install_events(&journal)?;
        let result = run_role(
            &manifest,
            &role,
            &env_path("POSTGRES_SUSTAINED_BARRIER")?,
            segment,
            seconds,
            offset,
            journal.clone(),
        )
        .await;
        append(
            &journal,
            &json!({"event":"role_result","complete":result.is_ok()}),
        )?;
        Arc::try_unwrap(journal)
            .map_err(|_| failed("evidence writer still held"))?
            .into_inner()
            .map_err(|_| failed("evidence poisoned"))?
            .finish()?;
        return result;
    }
    let mode = std::env::var("POSTGRES_SUSTAINED_MODE").unwrap_or_else(|_| "cell".into());
    if mode == "select" || mode == "calibration-report" {
        let mut evidence = Evidence::create(&root, &manifest)?;
        let bytes = fs::read(env_path("POSTGRES_SUSTAINED_REPORTS")?)?;
        if mode == "select" {
            let reports: Vec<evidence::CellReport> = serde_json::from_slice(&bytes)?;
            evidence.append(&evidence::selection_report(&reports))?;
        } else {
            let reports: Vec<serde_json::Value> = serde_json::from_slice(&bytes)?;
            evidence.append(&evidence::calibration_report(&reports))?;
        }
        return Ok(evidence.finish()?);
    }
    let resource_path = env_path("POSTGRES_SUSTAINED_RESOURCE_SAMPLE")?;
    resource_sample(&resource_path, &manifest)?;
    if manifest.effective_inputs["preflight_free_disk_bytes"]
        .as_u64()
        .unwrap_or_default()
        < 35 * 1024_u64.pow(3)
    {
        return Err(failed("35 GiB free required after release builds"));
    }
    let journal = Arc::new(Mutex::new(Evidence::create(&root, &manifest)?));
    install_events(&journal)?;
    let directory = journal
        .lock()
        .map_err(|_| failed("evidence poisoned"))?
        .directory()
        .to_owned();
    let plan = match mode.as_str() {
        "inventory" | "inventory-adjust" | "seed" => {
            let remaining = seed_remaining(&manifest)?;
            if mode == "seed" && remaining < Duration::from_secs(920) {
                return Err(failed(
                    "seed envelope cannot fit 15-minute churn plus recovery",
                ));
            }
            vec![(Segment::Warmup, remaining.as_secs(), 0)]
        }
        "qualify" => {
            if manifest.config.policy != evidence::Policy::P0 {
                return Err(failed("qualification requires P0"));
            }
            vec![(Segment::Warmup, 60, 0)]
        }
        "calibrate" => {
            if manifest.config.policy != evidence::Policy::P0 {
                return Err(failed("observer calibration requires P0"));
            }
            vec![(Segment::Warmup, 120, 0)]
        }
        "cell" => vec![
            (Segment::Warmup, 60, 0),
            (Segment::Steady, 150, 60),
            (Segment::CatchUp, 150, 210),
        ],
        "composed" => {
            if manifest.config.regime != evidence::Regime::Pressured {
                return Err(failed(
                    "composed proof requires qualified pressured inventory",
                ));
            }
            vec![(Segment::Steady, 1200, 0)]
        }
        _ => return Err(failed("unknown laboratory mode")),
    };
    if preparation_mode(&mode) {
        timeout(seed_remaining(&manifest)?, async {
            let dsn = infra_postgres::Dsn::admit(&std::env::var("DATABASE_URL")?)?;
            let control = workload::pool(&dsn, 2).await?;
            if mode == "inventory" {
                workload::create_fixture(&control).await?;
                workload::start_preparation(&control, seed_clock(&manifest)?).await?;
                workload::prepare_eligibility(&control, 150).await?;
            } else {
                let client = Client::new(
                    control.clone(),
                    manifest.config.seed,
                    inventory_input(&manifest)?,
                )?;
                workload::admit_preparation(&client, &mode, seed_clock(&manifest)?).await?;
                if mode == "inventory-adjust" {
                    timeout(Duration::from_secs(60), async {
                        workload::reset_preparation(&control).await?;
                        workload::prepare_eligibility(&control, 150).await
                    })
                    .await
                    .map_err(|_| failed("single inventory reset exceeded 60 seconds"))??;
                }
            }
            if infra_postgres::close(&control, Duration::from_secs(5)).await
                != infra_postgres::Closed::Complete
            {
                return Err(failed("seed setup pool remains"));
            }
            Ok(())
        })
        .await
        .map_err(|_| failed("cumulative seed setup bound"))??;
    }
    for (segment, seconds, offset) in plan {
        let created = manifest.effective_inputs["target_created_unix_ms"]
            .as_u64()
            .ok_or_else(|| failed("cumulative target creation time missing"))?;
        let required = (seconds + 60 + 1200) * 1000;
        let now = unix_ms()?;
        if created > now || now.saturating_add(required) > created.saturating_add(16_200_000) {
            return Err(failed(
                "cumulative 4h30 envelope cannot admit step plus cleanup reserve",
            ));
        }
        let boundary = directory.join(format!("{segment:?}"));
        fs::create_dir(&boundary)?;
        *CONTEXT
            .lock()
            .map_err(|_| failed("event context poisoned"))? = Some(("service0".into(), segment));
        let dsn = infra_postgres::Dsn::admit(&std::env::var("DATABASE_URL")?)?;
        let control = workload::pool(&dsn, 2).await?;
        let protected = workload::protected_jobs(&control).await?;
        if !preparation_mode(&mode) {
            workload::check_frozen_seed(
                &control,
                seed_clock(&manifest)?,
                manifest.effective_inputs["seed_cohort_hash"]
                    .as_str()
                    .ok_or_else(|| failed("frozen seed cohort hash missing"))?,
            )
            .await?;
            if mode == "composed" {
                workload::prepare_eligibility(&control, 1200).await?;
            }
            append(
                &journal,
                &json!({"event":"fixture_timestamp_staging", "segment":segment, "arrival_rows_per_second":20,"initial_cohort":if segment==Segment::CatchUp {10000} else {0}}),
            )?;
            workload::stage_replacements(&control, offset).await?;
            if segment != Segment::Warmup {
                workload::stage_eligibility(&control, segment == Segment::CatchUp).await?;
            }
        }
        if infra_postgres::close(&control, Duration::from_secs(5)).await
            != infra_postgres::Closed::Complete
        {
            return Err(failed("staging pool did not close"));
        }
        let mut children = Children::start(&boundary, segment, seconds, offset)?;
        let result = run_role(
            &manifest,
            "service0",
            &boundary,
            segment,
            seconds,
            offset,
            journal.clone(),
        )
        .await;
        let joined = children.join().await;
        append(
            &journal,
            &json!({"event":"segment_closed","segment":segment,"owners_joined":joined.is_ok(),"ordinary_complete":result.is_ok()}),
        )?;
        append(
            &journal,
            &json!({"event":"owners_stopped","complete":joined.is_ok()}),
        )?;
        result?;
        joined?;
        {
            let pool = workload::pool(&dsn, 4).await?;
            let inventory = inventory_input(&manifest)?;
            let client = Client::new(pool.clone(), manifest.config.seed, inventory)?;
            let recovery =
                workload::recovery(&client, inventory.live, &protected, inventory.generation);
            let checked = if preparation_mode(&mode) {
                timeout(seed_remaining(&manifest)?, recovery)
                    .await
                    .unwrap_or_else(|_| {
                        Err(failed("25-minute seed envelope exhausted during recovery"))
                    })
            } else {
                timeout(Duration::from_secs(60), recovery)
                    .await
                    .unwrap_or_else(|_| {
                        Err(failed("60-second cell boundary recovery bound exhausted"))
                    })
            };
            append(
                &journal,
                &json!({"event":"recovery_checked","readable":checked.is_ok(),"correctness_violations":u64::from(checked.is_err()),"hidden_retries":0}),
            )?;
            if mode == "composed" {
                append(
                    &journal,
                    &json!({"event":"composed_final_cohort","inventory":workload::composed_failure_inventory(&pool).await?}),
                )?;
            }
            checked?;
            if preparation_mode(&mode) {
                timeout(seed_remaining(&manifest)?, async {
                    let hash=workload::seed_cohort_hash(&pool).await?;
                    let elapsed=seed_elapsed(&manifest)?;
                    if mode=="seed" {
                        workload::finish_seed(&pool,&hash).await?;
                        append(&journal,&json!({"event":"seed_identity","seed_cohort_hash":hash,"seed_started_unix_ms":seed_started(&manifest)?,"seed_attempt_started_unix_ms":seed_clock(&manifest)?[1],"seed_elapsed_before_attempt_ms":seed_clock(&manifest)?[2],"seed_elapsed_ms":elapsed}))?;
                    } else {
                        let relations=workload::relation_snapshot(&pool).await?;
                        let used=workload::preparation_adjustments(&pool,seed_started(&manifest)?).await?;
                        let phases=workload::seed_phases(&pool).await?;
                        let mut report=evidence::inventory_report(&manifest,&relations,&phases,used,elapsed,&hash).map_err(std::io::Error::other)?;
                        report["before_counts"]=workload::previous_inventory_counts(&pool).await?;
                        report["after_counts"]=report["current_counts"].clone();
                        workload::finish_inventory(&pool,&report).await?;
                        append(&journal,&json!({"event":"inventory_report","report":report}))?;
                    }
                    Ok::<(),workload::Error>(())
                }).await.map_err(|_|failed("cumulative seed identity/report bound"))??;
            }
            let closed = infra_postgres::close(&pool, Duration::from_secs(5)).await;
            if closed != infra_postgres::Closed::Complete {
                return Err(failed("recovery pool did not close"));
            }
        }
    }
    let events = read_attempt_events(&root, &manifest.config.attempt_id)?;
    match mode.as_str() {
        "cell" => {
            let report = evidence::assemble_cell(&manifest, &events)
                .map_err(|gap| std::io::Error::other(gap))?;
            append(&journal, &json!({"event":"cell_report","report":report}))?;
            for segment in [Segment::Steady, Segment::CatchUp] {
                let series = evidence::segment_series(&events, segment, 150)
                    .map_err(std::io::Error::other)?;
                append(
                    &journal,
                    &json!({"event":"cell_series","segment":segment,"series":series}),
                )?;
            }
        }
        "qualify" => {
            let report =
                evidence::assemble_window(&manifest, &events, 60).map_err(std::io::Error::other)?;
            let gaps = report["ordinary_gaps"]
                .as_array()
                .ok_or_else(|| failed("qualification report missing gaps"))?;
            append(
                &journal,
                &json!({"event":"fixed_rate_qualification","report":report}),
            )?;
            if !gaps.is_empty() {
                return Err(failed(
                    "fixed-rate qualification failed; do not lower arrivals",
                ));
            }
        }
        "calibrate" => {
            let report = evidence::assemble_window(&manifest, &events, 120)
                .map_err(std::io::Error::other)?;
            append(
                &journal,
                &json!({"event":"calibration_complete","report":report}),
            )?;
        }
        "composed" => {
            let report =
                evidence::composed_report(&manifest, &events).map_err(std::io::Error::other)?;
            append(
                &journal,
                &json!({"event":"composed_report","report":report}),
            )?;
            if report["status"] != "composed_observations_complete" {
                return Err(failed("composed behavior proof incomplete; preserve gaps"));
            }
        }
        "seed" => {
            let report =
                evidence::seed_report(&manifest, &events).map_err(std::io::Error::other)?;
            append(
                &journal,
                &json!({"event":"seed_preconditioned","report":report,"lifecycle_elapsed_ms":seed_elapsed(&manifest)?}),
            )?;
            if report["status"] != "seed_qualified" || seed_remaining(&manifest).is_err() {
                return Err(failed(
                    "physical seed unqualified; preserve measured sizing input",
                ));
            }
        }
        _ => {}
    }
    if preparation_mode(&mode) {
        seed_remaining(&manifest)?;
    }
    Arc::try_unwrap(journal)
        .map_err(|_| failed("evidence writer still held"))?
        .into_inner()
        .map_err(|_| failed("evidence poisoned"))?
        .finish()?;
    Ok(())
}

fn read_attempt_events(root: &Path, attempt: &str) -> Result<Vec<serde_json::Value>> {
    use std::io::BufRead as _;
    let mut events = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name != attempt && !name.starts_with(&format!("{attempt}-")) {
            continue;
        }
        let file = entry.path().join("events.jsonl");
        for line in std::io::BufReader::new(fs::File::open(file)?).lines() {
            let line = line?;
            // The 20-minute composed proof retains four event records for
            // each of 196,800 arrivals, plus bounded telemetry and lifecycle.
            if line.len() > 1024 * 1024 || events.len() >= 900_000 {
                return Err(failed("attempt event assembly bound"));
            }
            events.push(serde_json::from_str(&line)?);
        }
    }
    Ok(events)
}
static TRACE_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static CLOCK: Mutex<Option<Instant>> = Mutex::new(None);
struct NativeEvents(std::sync::Weak<Mutex<Evidence>>);
impl tracing::Subscriber for NativeEvents {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == "infra_postgres::observe"
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(serde_json::Map<String, serde_json::Value>);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if matches!(
                    field.name(),
                    "message"
                        | "cleanup"
                        | "outcome"
                        | "elapsed_seconds"
                        | "committed_batches"
                        | "removed_rows"
                        | "offset_milliseconds"
                        | "population"
                        | "failure_class"
                ) {
                    self.0
                        .insert(field.name().into(), json!(format!("{value:?}")));
                }
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if matches!(
                    field.name(),
                    "message" | "cleanup" | "outcome" | "population" | "failure_class"
                ) {
                    self.0.insert(field.name().into(), json!(value));
                }
            }
            fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
                self.0.insert(field.name().into(), json!(value));
            }
            fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
                self.0.insert(field.name().into(), json!(value));
            }
        }
        let mut fields = Fields(serde_json::Map::new());
        event.record(&mut fields);
        let Some(name) = fields.0.get("message").and_then(serde_json::Value::as_str) else {
            return;
        };
        if !matches!(
            name,
            "postgres_cleanup_scheduled"
                | "postgres_cleanup_pass_finished"
                | "postgres_maintenance_observation_failed"
                | "postgres_maintenance_observation_resumed"
                | "postgres_maintenance_observation_finished"
        ) {
            return;
        }
        if let Some(journal) = self.0.upgrade() {
            let elapsed = CLOCK
                .lock()
                .ok()
                .and_then(|clock| clock.map(|start| ns(start.elapsed())));
            if append(
                &journal,
                &json!({"event":"native_trace","elapsed_ns":elapsed,"fields":fields.0}),
            )
            .is_err()
            {
                TRACE_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}
fn install_events(journal: &Journal) -> Result<()> {
    tracing::subscriber::set_global_default(NativeEvents(Arc::downgrade(journal)))
        .map_err(|_| failed("native tracing capture unavailable"))
}

struct RoleStop {
    tracker: TaskTracker,
    cancel: CancellationToken,
}
impl Drop for RoleStop {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.tracker.close();
    }
}
