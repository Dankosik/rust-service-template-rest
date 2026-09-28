//! One supervisor owns its handler and fixed queue outcome through cleanup.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use sqlx::postgres::PgConnection;
use tokio::task::JoinError;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::engine::{
    Operation, OperationError, RECORD_RETRY_INTERVAL, Shared, backstop, observe_failure,
    observe_recovery,
};
use crate::kind::{Attempt, Disposition, HandlerFuture, JobError, JobId, Policy};

const COOPERATIVE_GRACE: Duration = Duration::from_millis(100);

/// The longest stored failure summary, in bytes.
pub(crate) const ERROR_SUMMARY_MAX_BYTES: usize = 1024;
/// Observed handler results and claim-time exhaustions. Labels `kind`, `outcome`.
pub(crate) const ATTEMPTS_METRIC: &str = "jobs_attempts_total";
/// Queue-write acknowledgement, not attribution. Labels `kind`, `disposition`.
pub(crate) const PERSISTENCE_METRIC: &str = "jobs_persistence_total";
/// Handler run time. Label `kind`.
pub const ATTEMPT_DURATION_METRIC: &str = "jobs_attempt_duration_seconds";
/// Histogram buckets for [`ATTEMPT_DURATION_METRIC`], in seconds.
pub const ATTEMPT_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0,
    1800.0, 3600.0,
];

pub(crate) const COMPLETE: &str = "UPDATE background_jobs \
     SET state = 'completed', finished_at = statement_timestamp(), claim_expires_at = NULL \
     WHERE id = $1::uuid AND claim_generation = $2 AND state = 'running'";
const RETRY: &str = "UPDATE background_jobs \
     SET state = 'pending', not_before = statement_timestamp() + \
         GREATEST($3::double precision * (0.9 + 0.2 * random()), $4::bigint) * interval '1 microsecond', \
         claim_expires_at = NULL, error_summary = $5 \
     WHERE id = $1::uuid AND claim_generation = $2 AND state = 'running'";
const FAIL: &str = "UPDATE background_jobs \
     SET state = 'failed', failure_reason = $3, finished_at = statement_timestamp(), \
         claim_expires_at = NULL, error_summary = $4 \
     WHERE id = $1::uuid AND claim_generation = $2 AND state = 'running'";
const SNOOZE: &str = "UPDATE background_jobs \
     SET state = 'pending', not_before = statement_timestamp() + $3, claim_expires_at = NULL, \
         attempts = attempts - 1 \
     WHERE id = $1::uuid AND claim_generation = $2 AND state = 'running'";
/// A cancelled attempt gives its unit back and keeps `not_before`, so the job
/// keeps its place in claim order instead of queueing behind the backlog.
const RELEASE: &str = "UPDATE background_jobs \
     SET state = 'pending', claim_expires_at = NULL, attempts = attempts - 1 \
     WHERE id = $1::uuid AND claim_generation = $2 AND state = 'running'";

/// The queue transition a known attempt result asks for.
enum Transition {
    Complete,
    Retry {
        summary: String,
        base_micros: f64,
        floor_micros: i64,
        timed_out: bool,
    },
    Fail {
        reason: Failure,
        summary: String,
    },
    Snooze {
        delay_micros: i64,
    },
    Release,
}

#[derive(Clone, Copy, Debug)]
enum Failure {
    Exhausted,
    Permanent,
}

impl Failure {
    const fn label(self) -> &'static str {
        match self {
            Self::Exhausted => "exhausted",
            Self::Permanent => "permanent",
        }
    }
}

impl Transition {
    const fn label(&self) -> &'static str {
        match self {
            Self::Complete => "completed",
            Self::Retry {
                timed_out: true, ..
            } => "timeout",
            Self::Retry { .. } => "retry",
            Self::Fail { reason, .. } => reason.label(),
            Self::Snooze { .. } => "snoozed",
            Self::Release => "cancelled",
        }
    }
}

/// Describe attempt metrics once during engine startup.
pub(crate) fn describe_metrics() {
    metrics::describe_counter!(
        ATTEMPTS_METRIC,
        "Observed attempt dispositions, independent of queue-write acknowledgement"
    );
    metrics::describe_histogram!(
        ATTEMPT_DURATION_METRIC,
        "Observed handler duration in seconds"
    );
    metrics::describe_counter!(
        PERSISTENCE_METRIC,
        "Queue outcome acknowledgement disposition"
    );
}

struct AttemptId {
    id: JobId,
    generation: i64,
    kind: &'static str,
    attempt: u16,
}

/// Claim-time exhaustion is observed only after its transaction is acknowledged.
pub(crate) fn record_exhausted(id: JobId, kind: &'static str, attempt: u16, summary: Option<&str>) {
    record(
        &AttemptId {
            id,
            generation: 0,
            kind,
            attempt,
        },
        &Transition::Fail {
            reason: Failure::Exhausted,
            summary: summary.unwrap_or("").to_owned(),
        },
        None,
    );
}

fn record(attempt: &AttemptId, transition: &Transition, ran: Option<Duration>) {
    metrics::counter!(ATTEMPTS_METRIC, "kind" => attempt.kind, "outcome" => transition.label())
        .increment(1);
    if let Some(ran) = ran {
        metrics::histogram!(ATTEMPT_DURATION_METRIC, "kind" => attempt.kind)
            .record(ran.as_secs_f64());
    }
    match transition {
        Transition::Complete => {
            tracing::debug!(job.id = %attempt.id, job.kind = attempt.kind, job.attempt = attempt.attempt, "job_attempt_completed");
        }
        Transition::Snooze { .. } | Transition::Release => {
            tracing::info!(job.id = %attempt.id, job.kind = attempt.kind, outcome = transition.label(), "job_attempt_finished");
        }
        Transition::Retry { summary, .. } => {
            tracing::info!(job.id = %attempt.id, job.kind = attempt.kind, job.attempt = attempt.attempt, error = summary.as_str(), "job_attempt_failed");
        }
        Transition::Fail { summary, .. } => {
            tracing::warn!(job.id = %attempt.id, job.kind = attempt.kind, job.attempts = attempt.attempt, job.failure_reason = transition.label(), error = summary.as_str(), "job_failed");
        }
    }
}

pub(crate) async fn supervise(
    shared: Arc<Shared>,
    claimed: crate::claim::Claimed,
    deadline: Instant,
) {
    let crate::claim::Claimed {
        id,
        generation,
        kind,
        attempt,
        payload,
        trace_context: parent,
        trace_state,
        slot,
    } = claimed;
    let _slot = slot;
    let span = tracing::info_span!("job_attempt", job.id = %id, job.kind = kind, job.attempt = u64::from(attempt), otel.kind = "consumer");
    crate::trace_context::link(&span, parent.as_deref(), trace_state.as_deref());
    run_attempt(
        &shared,
        AttemptId {
            id,
            generation,
            kind,
            attempt,
        },
        payload,
        deadline,
    )
    .instrument(span)
    .await;
}

async fn run_attempt(shared: &Shared, attempt: AttemptId, payload: Vec<u8>, deadline: Instant) {
    if expired(shared, deadline) {
        uncertain(shared, &attempt);
        return;
    }
    let Some(registered) = shared.registry.get(attempt.kind) else {
        return;
    };
    let policy = registered.policy;
    let attempt_deadline = Instant::now()
        .checked_add(policy.timeout)
        .unwrap_or(deadline)
        .min(deadline);
    let cancel = CancellationToken::new();
    let prepared = registered.dispatch.prepare(
        Attempt {
            id: attempt.id,
            number: attempt.attempt,
            generation: attempt.generation,
            deadline: attempt_deadline,
            cancellation: cancel.clone(),
            pool: shared.pool.clone(),
        },
        &payload,
    );
    drop(payload);
    let (ended, ran) = match prepared {
        Err(error) => (Ended::Payload(error), None),
        Ok(future) => {
            let started = Instant::now();
            let Some(ended) = drive(shared, future, cancel, attempt_deadline, deadline).await
            else {
                uncertain(shared, &attempt);
                return;
            };
            (ended, Some(started.elapsed()))
        }
    };
    let transition = map_outcome(attempt.kind, attempt.attempt, policy, ended);
    if matches!(transition, Transition::Release) {
        shared.counters.cancelled.fetch_add(1, Ordering::Relaxed);
    } else {
        shared
            .counters
            .known_results
            .fetch_add(1, Ordering::Relaxed);
    }
    record(&attempt, &transition, ran);
    persist(shared, &attempt, &transition, deadline).await;
}

/// `future`'s output, or `None` once the attempt's local deadline passes or cleanup ends.
async fn within<T>(shared: &Shared, local: Instant, future: impl Future<Output = T>) -> Option<T> {
    tokio::select! {
        biased;
        value = future => Some(value),
        () = shared.hard_stop.cancelled() => None,
        () = tokio::time::sleep_until(local) => None,
    }
}

fn expired(shared: &Shared, local: Instant) -> bool {
    Instant::now() >= local || shared.hard_stop.is_cancelled()
}

/// Poll a ready result before cancellation; it wins as is. After cancellation
/// only success wins (see [`ended_after_cancel`]).
async fn drive(
    shared: &Shared,
    future: HandlerFuture,
    cancel: CancellationToken,
    attempt_deadline: Instant,
    local: Instant,
) -> Option<Ended> {
    let mut join = tokio::spawn(future.instrument(tracing::Span::current()));
    let reason = tokio::select! {
        biased;
        result = &mut join => return Some(ended_from(result)),
        () = shared.force.cancelled() => Ended::Cancelled,
        () = tokio::time::sleep_until(attempt_deadline) => Ended::Timeout,
    };
    cancel.cancel();
    let grace = Instant::now()
        .checked_add(COOPERATIVE_GRACE)
        .unwrap_or(local)
        .min(local);
    if let Some(result) = within(shared, grace, &mut join).await {
        return Some(ended_after_cancel(&result, reason));
    }
    join.abort();
    within(shared, local, &mut join)
        .await
        .map(|result| ended_after_cancel(&result, reason))
}

/// A result that joins after the supervisor cancelled the handler. Success
/// means the work finished, so it is kept. An error, snooze, or panic is how
/// the handler reacted to the cancellation, so it takes the cancellation's
/// `reason`: a forced drain still releases the job and refunds its attempt.
fn ended_after_cancel(result: &Result<Result<(), JobError>, JoinError>, reason: Ended) -> Ended {
    if matches!(result, Ok(Ok(()))) {
        Ended::Success
    } else {
        reason
    }
}

fn ended_from(result: Result<Result<(), JobError>, JoinError>) -> Ended {
    match result {
        Ok(Ok(())) => Ended::Success,
        Ok(Err(error)) => Ended::Error(error),
        Err(error) if error.is_cancelled() => Ended::Cancelled,
        Err(_) => Ended::Panic,
    }
}

async fn persist(shared: &Shared, attempt: &AttemptId, transition: &Transition, local: Instant) {
    let operation = if matches!(transition, Transition::Release) {
        Operation::Release
    } else {
        Operation::Record
    };
    loop {
        if expired(shared, local) {
            break;
        }
        match within(shared, local, send_outcome(shared, attempt, transition)).await {
            None => break,
            Some(Ok(rows)) => {
                observe_recovery(shared, operation);
                let disposition = if rows == 1 { "applied" } else { "unchanged" };
                persistence(attempt.kind, disposition);
                tracing::debug!(
                    job.kind = attempt.kind,
                    disposition,
                    "job_persistence_finished"
                );
                if rows == 1 && matches!(transition, Transition::Release) {
                    shared.counters.released.fetch_add(1, Ordering::Relaxed);
                }
                return;
            }
            Some(Err(error)) => observe_failure(shared, operation, &error),
        }
        let until = Instant::now()
            .checked_add(RECORD_RETRY_INTERVAL)
            .unwrap_or(local);
        if within(shared, local, tokio::time::sleep_until(until))
            .await
            .is_none()
        {
            break;
        }
    }
    uncertain(shared, attempt);
}

fn persistence(kind: &'static str, disposition: &'static str) {
    metrics::counter!(PERSISTENCE_METRIC, "kind" => kind, "disposition" => disposition)
        .increment(1);
}

/// The attempt's queue transition is not known to have landed; lease expiry
/// recovers the row.
fn uncertain(shared: &Shared, attempt: &AttemptId) {
    shared.counters.uncertain.fetch_add(1, Ordering::Relaxed);
    persistence(attempt.kind, "unknown");
    tracing::warn!(
        job.kind = attempt.kind,
        disposition = "unknown",
        "job_persistence_finished"
    );
}

async fn send_outcome(
    shared: &Shared,
    attempt: &AttemptId,
    transition: &Transition,
) -> Result<u64, OperationError> {
    // An sqlx statement future is about 16 KiB; box it once so the supervisor stays small.
    backstop(Box::pin(async {
        let mut connection = shared
            .pool
            .acquire()
            .await
            .map_err(OperationError::Acquire)?;
        execute(
            &mut connection,
            &attempt.id.to_string(),
            attempt.generation,
            transition,
        )
        .await
        .map_err(OperationError::from)
    }))
    .await
}

async fn execute(
    connection: &mut PgConnection,
    id: &str,
    generation: i64,
    transition: &Transition,
) -> Result<u64, sqlx::Error> {
    let query = match transition {
        Transition::Complete => sqlx::query(COMPLETE).bind(id).bind(generation),
        Transition::Retry {
            summary,
            base_micros,
            floor_micros,
            ..
        } => sqlx::query(RETRY)
            .bind(id)
            .bind(generation)
            .bind(base_micros)
            .bind(floor_micros)
            .bind(summary.as_str()),
        Transition::Snooze { delay_micros } => {
            let delay = sqlx::postgres::types::PgInterval {
                months: 0,
                days: 0,
                microseconds: *delay_micros,
            };
            sqlx::query(SNOOZE).bind(id).bind(generation).bind(delay)
        }
        Transition::Release => sqlx::query(RELEASE).bind(id).bind(generation),
        Transition::Fail { reason, summary } => sqlx::query(FAIL)
            .bind(id)
            .bind(generation)
            .bind(reason.label())
            .bind(summary.as_str()),
    };
    Ok(query.execute(connection).await?.rows_affected())
}

enum Ended {
    Success,
    Error(JobError),
    Panic,
    Payload(serde_json::Error),
    Timeout,
    Cancelled,
}

fn map_outcome(kind: &'static str, attempt: u16, policy: Policy, ended: Ended) -> Transition {
    match ended {
        Ended::Success => Transition::Complete,
        Ended::Cancelled => Transition::Release,
        Ended::Error(error) => match error.disposition {
            Disposition::Snooze(delay_micros) => Transition::Snooze { delay_micros },
            Disposition::Permanent => Transition::Fail {
                reason: Failure::Permanent,
                summary: summary(&error.to_string()),
            },
            Disposition::RetryAfterAtLeast(floor_micros) => {
                retry(attempt, policy, &error.to_string(), floor_micros, false)
            }
            Disposition::Retry => retry(attempt, policy, &error.to_string(), 0, false),
        },
        Ended::Panic => retry(attempt, policy, "handler panicked", 0, false),
        Ended::Payload(error) => retry(attempt, policy, &payload_summary(kind, &error), 0, false),
        Ended::Timeout => retry(
            attempt,
            policy,
            &format!(
                "attempt timed out after {}",
                humantime::format_duration(policy.timeout)
            ),
            0,
            true,
        ),
    }
}

fn retry(
    attempt: u16,
    policy: Policy,
    text: &str,
    floor_micros: i64,
    timed_out: bool,
) -> Transition {
    let summary = summary(text);
    if attempt >= policy.max_attempts {
        Transition::Fail {
            reason: Failure::Exhausted,
            summary,
        }
    } else {
        Transition::Retry {
            summary,
            base_micros: f64::from(attempt).powi(4) * 1_000_000.0,
            floor_micros,
            timed_out,
        }
    }
}

fn payload_summary(kind: &str, error: &serde_json::Error) -> String {
    let category = match error.classify() {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
    };
    format!(
        "payload does not decode as {kind}: {category} error at line {} column {}",
        error.line(),
        error.column()
    )
}

fn summary(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len().min(ERROR_SUMMARY_MAX_BYTES));
    for ch in text.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        if sanitized.len() + ch.len_utf8() > ERROR_SUMMARY_MAX_BYTES {
            break;
        }
        sanitized.push(ch);
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(ended: Ended, attempt: u16) -> Transition {
        map_outcome("sample", attempt, Policy::default(), ended)
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "attempt^4 * 1e6 is exact in f64")]
    fn dispositions_preserve_cap_and_snooze() {
        assert!(matches!(outcome(Ended::Success, 25), Transition::Complete));
        assert!(matches!(
            outcome(Ended::Error(JobError::retryable("fail")), 25),
            Transition::Fail {
                reason: Failure::Exhausted,
                ..
            }
        ));
        assert!(matches!(
            outcome(
                Ended::Error(
                    JobError::retry_after_at_least("at least", Duration::from_secs(2)).unwrap()
                ),
                1,
            ),
            Transition::Retry {
                base_micros,
                floor_micros: 2_000_000,
                timed_out: false,
                ..
            } if base_micros == 1_000_000.0
        ));
        assert!(matches!(
            outcome(
                Ended::Error(
                    JobError::retry_after_at_least("at least", Duration::from_secs(2)).unwrap()
                ),
                3,
            ),
            Transition::Retry {
                base_micros,
                floor_micros: 2_000_000,
                timed_out: false,
                ..
            } if base_micros == 81_000_000.0
        ));
        assert!(matches!(
            outcome(
                Ended::Error(JobError::snooze(Duration::from_micros(7)).unwrap()),
                25,
            ),
            Transition::Snooze { delay_micros: 7 }
        ));
        assert!(matches!(
            outcome(Ended::Panic, 1),
            Transition::Retry {
                ref summary,
                timed_out: false,
                floor_micros: 0,
                ..
            } if summary == "handler panicked"
        ));
        assert!(matches!(
            outcome(Ended::Timeout, 1),
            Transition::Retry {
                timed_out: true,
                ..
            }
        ));
        assert!(matches!(
            outcome(Ended::Error(JobError::permanent("stop")), 1),
            Transition::Fail {
                reason: Failure::Permanent,
                ..
            }
        ));
    }

    #[test]
    fn summaries_sanitize_controls_and_preserve_utf8_without_payload_disclosure() {
        assert_eq!(summary("a\nb\0c"), "a b c");
        assert_eq!(summary(""), "");
        assert_eq!(summary(&"x".repeat(1024)), "x".repeat(1024));
        assert_eq!(summary(&"x".repeat(65_536)), "x".repeat(1024));
        assert_eq!(summary(&("\u{0085}".repeat(1023) + "é")), " ".repeat(1023));
        assert_eq!(summary(&("é".repeat(512) + "tail")), "é".repeat(512));
        assert_eq!(summary(&("a".repeat(1023) + "é")), "a".repeat(1023));
        let error = serde_json::from_str::<u32>("\"secret\"").unwrap_err();
        assert_eq!(
            payload_summary("sample", &error),
            "payload does not decode as sample: data error at line 1 column 8"
        );
    }
}
