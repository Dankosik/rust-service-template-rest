//! The one test-only job kind.
//!
//! Shared by the jobs database suite and the fixture worker. The fixture
//! binary registers it through `register`. The shipped binary never contains
//! this kind.

use std::time::Duration;

use infra_jobs::{Job, JobError, JobKind, Kinds, Policy};
use jobs_worker::{BuildError, Support};
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
    /// Sleep, ignoring cancellation, so a timeout must abort the attempt.
    Sleep {
        /// How long to sleep.
        millis: u64,
    },
    /// Wait until the attempt is cancelled, then fail retryably.
    WaitForCancellation,
    /// Panic. The worker must catch it.
    Panic,
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
/// When the action is [`ProbeAction::Panic`].
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
    }
}

/// The fixture worker's registration, the one test-only kind with the default policy.
///
/// # Errors
///
/// Never fails today. The signature is the worker's registration contract.
pub fn register(kinds: &mut Kinds, _support: &Support<'_>) -> Result<(), BuildError> {
    kinds.register(Policy::default(), handle);
    Ok(())
}
