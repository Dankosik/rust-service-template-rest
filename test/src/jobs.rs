use std::fmt;
use std::time::Duration;

use infra_jobs::{Engine, Job, JobError, JobKind, KindName, Policy, Started};
use serde::{Deserialize, Serialize};
use tokio_util::sync::{CancellationToken, DropGuard};
use tokio_util::task::TaskTracker;

pub struct EngineRun {
    pub started: Started,
    pub tracker: TaskTracker,
    pub cancel: CancellationToken,
    _cancel_guard: DropGuard,
}

impl fmt::Debug for EngineRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EngineRun")
            .field("tracker", &self.tracker)
            .field("cancel", &self.cancel)
            .finish_non_exhaustive()
    }
}

impl EngineRun {
    #[must_use]
    pub fn start(engine: Engine) -> Self {
        let tracker = TaskTracker::new();
        let cancel = CancellationToken::new();
        let started = engine.start(&tracker, &cancel);
        tracker.close();
        let cancel_guard = cancel.clone().drop_guard();
        Self {
            started,
            tracker,
            cancel,
            _cancel_guard: cancel_guard,
        }
    }

    pub async fn drain_and_join(self, bound: Duration) {
        self.started.stop_claiming();
        tokio::time::timeout(bound, self.started.drained())
            .await
            .expect("the engine drains");
        self.cancel.cancel();
        tokio::time::timeout(bound, self.tracker.wait())
            .await
            .expect("engine background tasks join");
    }

    pub async fn cancel_and_join(self, bound: Duration) {
        self.cancel.cancel();
        tokio::time::timeout(bound, self.tracker.wait())
            .await
            .expect("engine background tasks join");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub action: ProbeAction,
}

impl JobKind for Probe {
    const NAME: KindName = KindName::new("test.probe");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeAction {
    Succeed,
    FailRetryable,
    FailPermanent,
    RetryAfterAtLeast { millis: u64 },
    SnoozeOnce { millis: u64 },
    Sleep { millis: u64 },
    WaitForCancellation,
    Panic,
    DropPanic { secondary: bool },
    PanicPayloadDrop,
}

struct PendingDropPanic(bool);

impl Drop for PendingDropPanic {
    #[expect(
        clippy::panic,
        reason = "fixture exercises handler destruction unwinding"
    )]
    fn drop(&mut self) {
        if self.0 {
            std::panic::panic_any(PanicPayloadDrop);
        }
        panic!("handler-drop-secret");
    }
}

struct PanicPayloadDrop;

impl Drop for PanicPayloadDrop {
    #[expect(
        clippy::panic,
        reason = "fixture exercises secondary panic payload unwinding"
    )]
    fn drop(&mut self) {
        panic!("payload-drop-secret");
    }
}

pub const CREATE_PROBE_ATTEMPTS: &str = "CREATE TABLE probe_attempts (seq bigserial PRIMARY KEY, job_id uuid NOT NULL, attempt integer NOT NULL, started_at timestamptz NOT NULL DEFAULT clock_timestamp())";

pub async fn handle(job: Job<Probe>) -> Result<(), JobError> {
    let id = job.id().to_string();
    {
        let mut conn = job
            .pool()
            .acquire("record probe attempt")
            .await
            .map_err(|_| JobError::retryable("probe write failed"))?;
        sqlx::query("INSERT INTO probe_attempts (job_id, attempt) VALUES ($1::uuid, $2)")
            .bind(id)
            .bind(i32::from(job.attempt()))
            .execute(&mut *conn)
            .await
            .map_err(|_| JobError::retryable("probe write failed"))?;
    }
    match job.payload().action {
        ProbeAction::Succeed => Ok(()),
        ProbeAction::FailRetryable => Err(JobError::retryable("probe failed retryably")),
        ProbeAction::FailPermanent => Err(JobError::permanent("probe failed permanently")),
        ProbeAction::RetryAfterAtLeast { millis } => Err(JobError::retry_after_at_least(
            "probe requested retry delay",
            Duration::from_millis(millis),
        )
        .expect("probe delay is valid")),
        ProbeAction::SnoozeOnce { millis } => {
            let mut conn = job
                .pool()
                .acquire("read probe attempts")
                .await
                .map_err(|_| JobError::retryable("probe lookup failed"))?;
            let deliveries: i64 =
                sqlx::query_scalar("SELECT count(*) FROM probe_attempts WHERE job_id = $1::uuid")
                    .bind(job.id().to_string())
                    .fetch_one(&mut *conn)
                    .await
                    .map_err(|_| JobError::retryable("probe lookup failed"))?;
            if deliveries == 1 {
                Err(JobError::snooze(Duration::from_millis(millis)).expect("probe delay is valid"))
            } else {
                Ok(())
            }
        }
        ProbeAction::Sleep { millis } => {
            tokio::time::sleep(Duration::from_millis(millis)).await;
            Ok(())
        }
        ProbeAction::WaitForCancellation => {
            job.cancellation().cancelled().await;
            Err(JobError::retryable("probe cancelled"))
        }
        #[expect(
            clippy::panic,
            reason = "the probe action is the panic the worker must catch"
        )]
        ProbeAction::Panic => panic!("probe panicked"),
        ProbeAction::DropPanic { secondary } => {
            let _drop = PendingDropPanic(secondary);
            std::future::pending().await
        }
        ProbeAction::PanicPayloadDrop => std::panic::panic_any(PanicPayloadDrop),
    }
}

pub fn register(
    registration: &mut jobs_worker::Registration<'_>,
) -> Result<(), jobs_worker::BuildError> {
    registration.jobs.register(Policy::default(), handle);
    // template:begin outbox:test-jobs-deferred-messages-fixture
    if let Ok(mode) = std::env::var("JOBS_WORKER_FIXTURE_MESSAGE_FACTORY") {
        registration.with_postgres_messages(move |pool, _| {
            assert!(pool.size() > 0, "factory must receive the admitted pool");
            match mode.as_str() {
                "error" => Err("fixture message factory refused".into()),
                #[expect(
                    clippy::panic,
                    reason = "exercise guarded startup unwind after pool admission"
                )]
                "panic" => panic!("message factory panic payload must stay withheld"),
                _ => Ok(()),
            }
        })?;
    }
    // template:end outbox:test-jobs-deferred-messages-fixture
    Ok(())
}
