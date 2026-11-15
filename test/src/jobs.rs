use std::time::Duration;

use infra_jobs::{Job, JobError, JobKind, Policy};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub action: ProbeAction,
}

impl JobKind for Probe {
    const NAME: &'static str = "test.probe";
}

const _: () = infra_jobs::assert_valid_kind_name(Probe::NAME);

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

/// The attempts table a jobs test creates. No key on `(job_id, attempt)`:
/// a released attempt reuses its number.
pub const CREATE_PROBE_ATTEMPTS: &str = "CREATE TABLE probe_attempts (seq bigserial PRIMARY KEY, job_id uuid NOT NULL, attempt integer NOT NULL, started_at timestamptz NOT NULL DEFAULT clock_timestamp())";

pub async fn handle(job: Job<Probe>) -> Result<(), JobError> {
    let id = job.id().to_string();
    sqlx::query("INSERT INTO probe_attempts (job_id, attempt) VALUES ($1::uuid, $2)")
        .bind(id)
        .bind(i32::from(job.attempt()))
        .execute(job.pool())
        .await?;
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
            let deliveries: i64 =
                sqlx::query_scalar("SELECT count(*) FROM probe_attempts WHERE job_id = $1::uuid")
                    .bind(job.id().to_string())
                    .fetch_one(job.pool())
                    .await?;
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

pub const BACKGROUND_TASK_RETURNS: &str = "JOBS_WORKER_FIXTURE_BACKGROUND_TASK_RETURNS";

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
    if std::env::var_os(BACKGROUND_TASK_RETURNS).is_some() {
        registration.spawn("fixture", |_cancel| async {});
    }
    Ok(())
}
