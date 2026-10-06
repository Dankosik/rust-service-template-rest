//! The one test-only job kind.
//!
//! Shared by the jobs database suite and the fixture worker. The fixture
//! binary registers it through [`REGISTER`]. The shipped binary never contains
//! this kind.

use std::time::Duration;

use infra_jobs::{Job, JobError, JobKind, Policy};
use serde::{Deserialize, Serialize};

/// A probe payload. The action tells the handler what to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    /// What this attempt should do.
    pub action: ProbeAction,
}

impl JobKind for Probe {
    const NAME: &'static str = "test.probe";
}

const _: () = infra_jobs::assert_valid_kind_name(Probe::NAME);

/// What one probe attempt does after it records itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeAction {
    /// Finish successfully.
    Succeed,
    /// Fail with a retryable error.
    FailRetryable,
    /// Fail with a permanent error.
    FailPermanent,
    /// Retry no sooner than a caller-selected delay.
    RetryAfterAtLeast {
        /// The floor, in milliseconds.
        millis: u64,
    },
    /// Defer once, then complete on the next delivery.
    SnoozeOnce {
        /// The requested delay in milliseconds.
        millis: u64,
    },
    /// Sleep, ignoring cancellation, so a timeout must abort the attempt.
    Sleep {
        /// How long to sleep.
        millis: u64,
    },
    /// Wait until the attempt is cancelled, then fail retryably.
    WaitForCancellation,
    /// Panic. The worker must catch it.
    Panic,
    /// Remain pending and panic when cancellation destroys the handler.
    DropPanic {
        /// Also unwind while disposing the first panic's payload.
        secondary: bool,
    },
    /// Panic with a payload whose disposal fails the supervisor.
    PanicPayloadDrop,
}

struct PendingDropPanic(bool);

impl Drop for PendingDropPanic {
    #[allow(
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
    #[allow(
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

/// Record the attempt, then follow [`Probe::action`].
///
/// # Errors
///
/// A retryable [`JobError`] when the attempt row cannot be written, or when
/// the action fails retryably or is cancelled. A permanent [`JobError`] when
/// the action fails permanently.
///
/// # Panics
///
/// When the action is [`ProbeAction::Panic`] or [`ProbeAction::PanicPayloadDrop`].
/// [`ProbeAction::DropPanic`] panics when its pending future is destroyed.
#[allow(
    clippy::needless_pass_by_value,
    reason = "handlers take the job by value"
)]
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
        #[allow(
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

/// Set in the fixture worker's environment, makes its registration spawn a
/// background task named `fixture` that returns at once.
pub const BACKGROUND_TASK_RETURNS: &str = "JOBS_WORKER_FIXTURE_BACKGROUND_TASK_RETURNS";

/// The fixture worker's registration, the one test-only kind with the default policy.
///
/// # Errors
/// Never; the worker's registration contract is fallible.
pub fn register(
    registration: &mut jobs_worker::Registration<'_>,
) -> Result<(), jobs_worker::BuildError> {
    registration.jobs.register(Policy::default(), handle);
    if std::env::var_os(BACKGROUND_TASK_RETURNS).is_some() {
        registration.spawn("fixture", |_cancel| async {});
    }
    Ok(())
}
