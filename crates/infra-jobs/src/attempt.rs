//! One supervisor owns its handler and fixed queue outcome through cleanup.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use infra_postgres::{connection, in_tx_with};
use sqlx::postgres::PgConnection;
use tokio::task::JoinError;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::engine::{
    OpFailed, Operation, OperationError, READ_COMMITTED, RECORD_RETRY_INTERVAL, Shared, backstop,
    observe_failure, observe_recovery,
};
use crate::kind::{Disposition, HandlerFuture, JobError, JobId, Policy};
use crate::trace_context;

/// The longest stored failure summary, in bytes.
pub const ERROR_SUMMARY_MAX_BYTES: usize = 1024;
/// Observed handler results and claim-time exhaustions. Labels `kind`, `outcome`.
pub const ATTEMPTS_METRIC: &str = "jobs_attempts_total";
/// Queue-write acknowledgement, not attribution. Labels `kind`, `disposition`.
pub const PERSISTENCE_METRIC: &str = "jobs_persistence_total";
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
     SET state = 'pending', not_before = statement_timestamp() + $3, claim_expires_at = NULL, \
         error_summary = $4 \
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Completed,
    Retry,
    Timeout,
    Exhausted,
    Permanent,
    Snoozed,
    Cancelled,
    TransactionUnknown,
}

impl Outcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Retry => "retry",
            Self::Timeout => "timeout",
            Self::Exhausted => "exhausted",
            Self::Permanent => "permanent",
            Self::Snoozed => "snoozed",
            Self::Cancelled => "cancelled",
            Self::TransactionUnknown => "transaction_unknown",
        }
    }
}

struct Intended {
    outcome: Outcome,
    summary: Option<String>,
    delay_micros: i64,
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
        &Intended {
            outcome: Outcome::Exhausted,
            summary: summary.map(str::to_owned),
            delay_micros: 0,
        },
        None,
    );
}

fn record(attempt: &AttemptId, intended: &Intended, ran: Option<Duration>) {
    metrics::describe_counter!(
        ATTEMPTS_METRIC,
        "Observed attempt dispositions, independent of queue-write acknowledgement"
    );
    metrics::describe_histogram!(
        ATTEMPT_DURATION_METRIC,
        "Observed handler duration in seconds"
    );
    metrics::counter!(ATTEMPTS_METRIC, "kind" => attempt.kind, "outcome" => intended.outcome.as_str()).increment(1);
    if let Some(ran) = ran {
        metrics::histogram!(ATTEMPT_DURATION_METRIC, "kind" => attempt.kind)
            .record(ran.as_secs_f64());
    }
    match intended.outcome {
        Outcome::Completed => {
            tracing::debug!(job.id = %attempt.id, job.kind = attempt.kind, job.attempt = attempt.attempt, "job_attempt_completed");
        }
        Outcome::Snoozed | Outcome::Cancelled => {
            tracing::info!(job.id = %attempt.id, job.kind = attempt.kind, outcome = intended.outcome.as_str(), "job_attempt_finished");
        }
        Outcome::TransactionUnknown => {
            tracing::warn!(job.id = %attempt.id, job.kind = attempt.kind, "job_transaction_unknown");
        }
        Outcome::Retry | Outcome::Timeout => {
            tracing::info!(job.id = %attempt.id, job.kind = attempt.kind, job.attempt = attempt.attempt, retry_in_micros = intended.delay_micros, error = intended.summary.as_deref().unwrap_or(""), "job_attempt_failed");
        }
        Outcome::Exhausted | Outcome::Permanent => {
            tracing::warn!(job.id = %attempt.id, job.kind = attempt.kind, job.attempts = attempt.attempt, job.failure_reason = intended.outcome.as_str(), error = intended.summary.as_deref().unwrap_or(""), "job_failed");
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
        jitter,
        slot,
    } = claimed;
    let _slot = slot;
    let span = tracing::info_span!("job_attempt", job.id = %id, job.kind = kind, job.attempt = u64::from(attempt), otel.kind = "consumer");
    trace_context::link(&span, parent.as_deref(), trace_state.as_deref());
    run_attempt(
        &shared,
        AttemptId {
            id,
            generation,
            kind,
            attempt,
        },
        payload,
        jitter,
        deadline,
    )
    .instrument(span)
    .await;
}

async fn run_attempt(
    shared: &Shared,
    attempt: AttemptId,
    payload: Vec<u8>,
    jitter: f64,
    deadline: Instant,
) {
    if Instant::now() >= shared.deadline(deadline) {
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
        attempt.id,
        attempt.attempt,
        attempt.generation,
        attempt_deadline,
        &payload,
        cancel.clone(),
        shared.pool.clone(),
    );
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
    let intended = map_outcome(attempt.kind, attempt.attempt, policy, jitter, ended);
    if intended.outcome == Outcome::Cancelled {
        shared.counters.cancelled.fetch_add(1, Ordering::Relaxed);
    } else {
        shared
            .counters
            .known_results
            .fetch_add(1, Ordering::Relaxed);
    }
    record(&attempt, &intended, ran);
    if intended.outcome == Outcome::TransactionUnknown {
        uncertain(shared, &attempt);
        return;
    }
    persist(shared, &attempt, &intended, deadline).await;
}

/// Poll a ready result before cancellation. Even after abort, a joined result wins.
async fn drive(
    shared: &Shared,
    future: HandlerFuture,
    cancel: CancellationToken,
    deadline: Instant,
    local: Instant,
) -> Option<Ended> {
    let mut join = tokio::spawn(future.instrument(tracing::Span::current()));
    let reason = tokio::select! {
        biased;
        result = &mut join => return Some(ended_from(result)),
        () = shared.force.cancelled() => Ended::Cancelled,
        () = tokio::time::sleep_until(deadline) => Ended::Timeout,
    };
    cancel.cancel();
    join.abort();
    loop {
        let forced = shared.force.is_cancelled();
        let deadline = shared.deadline(local);
        tokio::select! {
            biased;
            result = &mut join => return Some(match result {
                Err(error) if error.is_cancelled() => reason,
                result => ended_from(result),
            }),
            () = shared.force.cancelled(), if !forced => {},
            () = tokio::time::sleep_until(deadline) => return None,
        }
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

async fn persist(shared: &Shared, attempt: &AttemptId, intended: &Intended, local: Instant) {
    let operation = if intended.outcome == Outcome::Cancelled {
        Operation::Release
    } else {
        Operation::Record
    };
    loop {
        if Instant::now() >= shared.deadline(local) {
            break;
        }
        let send = send_outcome(shared, attempt, intended);
        tokio::pin!(send);
        let result = loop {
            let forced = shared.force.is_cancelled();
            let deadline = shared.deadline(local);
            tokio::select! {
                biased;
                result = &mut send => break Some(result),
                () = shared.force.cancelled(), if !forced => {},
                () = tokio::time::sleep_until(deadline) => break None,
            }
        };
        match result {
            Some(Ok(rows)) => {
                observe_recovery(shared, operation);
                let disposition = if rows == 1 { "applied" } else { "unchanged" };
                persistence(attempt.kind, disposition);
                tracing::debug!(
                    job.kind = attempt.kind,
                    disposition,
                    "job_persistence_finished"
                );
                if rows == 1 && intended.outcome == Outcome::Cancelled {
                    shared.counters.released.fetch_add(1, Ordering::Relaxed);
                }
                return;
            }
            Some(Err(error)) => observe_failure(shared, operation, error),
            None => break,
        }
        let forced = shared.force.is_cancelled();
        let until = Instant::now()
            .checked_add(RECORD_RETRY_INTERVAL)
            .unwrap_or(local)
            .min(shared.deadline(local));
        tokio::select! {
            () = tokio::time::sleep_until(until) => {},
            () = shared.force.cancelled(), if !forced => {},
        }
    }
    uncertain(shared, attempt);
}

fn persistence(kind: &'static str, disposition: &'static str) {
    metrics::describe_counter!(
        PERSISTENCE_METRIC,
        "Queue outcome acknowledgement disposition"
    );
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
    intended: &Intended,
) -> Result<u64, OperationError> {
    let returned = AtomicBool::new(false);
    let id = attempt.id.to_string();
    backstop(
        &returned,
        in_tx_with(
            &shared.pool,
            READ_COMMITTED,
            async |tx| -> Result<u64, OpFailed> {
                let affected = execute(connection(tx), &id, attempt.generation, intended).await?;
                returned.store(true, Ordering::SeqCst);
                Ok(affected)
            },
        ),
    )
    .await
}

async fn execute(
    conn: &mut PgConnection,
    id: &str,
    generation: i64,
    intended: &Intended,
) -> Result<u64, sqlx::Error> {
    let delay = sqlx::postgres::types::PgInterval {
        months: 0,
        days: 0,
        microseconds: intended.delay_micros,
    };
    let query = match intended.outcome {
        Outcome::Completed => sqlx::query(COMPLETE).bind(id).bind(generation),
        Outcome::Retry | Outcome::Timeout => sqlx::query(RETRY)
            .bind(id)
            .bind(generation)
            .bind(delay)
            .bind(intended.summary.as_deref().unwrap_or("")),
        Outcome::Snoozed => sqlx::query(SNOOZE).bind(id).bind(generation).bind(delay),
        Outcome::Cancelled => sqlx::query(RELEASE).bind(id).bind(generation),
        Outcome::Exhausted | Outcome::Permanent => sqlx::query(FAIL)
            .bind(id)
            .bind(generation)
            .bind(intended.outcome.as_str())
            .bind(intended.summary.as_deref().unwrap_or("")),
        Outcome::TransactionUnknown => return Ok(0),
    };
    Ok(query.execute(conn).await?.rows_affected())
}

enum Ended {
    Success,
    Error(JobError),
    Panic,
    Payload(serde_json::Error),
    Timeout,
    Cancelled,
}

fn map_outcome(
    kind: &'static str,
    attempt: u16,
    policy: Policy,
    jitter: f64,
    ended: Ended,
) -> Intended {
    let empty = |outcome| Intended {
        outcome,
        summary: None,
        delay_micros: 0,
    };
    let (text, delay, floor, timed_out) = match ended {
        Ended::Success => return empty(Outcome::Completed),
        Ended::Cancelled => return empty(Outcome::Cancelled),
        Ended::Error(error) => match error.disposition {
            Disposition::Snooze(delay_micros) => {
                return Intended {
                    outcome: Outcome::Snoozed,
                    summary: None,
                    delay_micros,
                };
            }
            Disposition::TransactionUnknown => return empty(Outcome::TransactionUnknown),
            Disposition::Permanent => {
                return Intended {
                    outcome: Outcome::Permanent,
                    summary: Some(summary(&error.to_string())),
                    delay_micros: 0,
                };
            }
            Disposition::RetryAfter(delay) => (error.to_string(), Some(delay), None, false),
            Disposition::RetryAfterAtLeast(delay) => (error.to_string(), None, Some(delay), false),
            Disposition::Retry => (error.to_string(), None, None, false),
        },
        Ended::Panic => ("handler panicked".to_owned(), None, None, false),
        Ended::Payload(error) => (payload_summary(kind, &error), None, None, false),
        Ended::Timeout => (
            format!(
                "attempt timed out after {}",
                humantime::format_duration(policy.timeout)
            ),
            None,
            None,
            true,
        ),
    };
    let exhausted = attempt >= policy.max_attempts;
    Intended {
        outcome: if exhausted {
            Outcome::Exhausted
        } else if timed_out {
            Outcome::Timeout
        } else {
            Outcome::Retry
        },
        summary: Some(summary(&text)),
        delay_micros: if exhausted {
            0
        } else {
            delay.unwrap_or_else(|| backoff(attempt, jitter).max(floor.unwrap_or_default()))
        },
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
    let mut sanitized: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let mut end = sanitized.len().min(ERROR_SUMMARY_MAX_BYTES);
    while !sanitized.is_char_boundary(end) {
        end -= 1;
    }
    sanitized.truncate(end);
    sanitized
}

fn backoff(attempt: u16, draw: f64) -> i64 {
    // Registered policies cap attempts at 25, so the finite SQL draw fits i64 microseconds.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "accepted jitter rounds down to microseconds; the policy bound fits i64"
    )]
    let micros = (f64::from(attempt).powi(4) * (0.9 + 0.2 * draw) * 1_000_000.0).floor() as i64;
    micros
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(ended: Ended, attempt: u16) -> Intended {
        map_outcome("sample", attempt, Policy::default(), 0.5, ended)
    }

    #[test]
    fn dispositions_preserve_cap_snooze_and_transaction_uncertainty() {
        assert_eq!(outcome(Ended::Success, 25).outcome, Outcome::Completed);
        assert_eq!(
            outcome(Ended::Error(JobError::retryable("fail")), 25).outcome,
            Outcome::Exhausted
        );
        let retry = outcome(
            Ended::Error(JobError::retry_after("later", Duration::from_micros(9)).unwrap()),
            1,
        );
        assert_eq!(retry.delay_micros, 9);
        let floor = outcome(
            Ended::Error(
                JobError::retry_after_at_least("at least", Duration::from_secs(2)).unwrap(),
            ),
            1,
        );
        assert_eq!(floor.delay_micros, 2_000_000);
        let backoff = outcome(
            Ended::Error(
                JobError::retry_after_at_least("at least", Duration::from_secs(2)).unwrap(),
            ),
            3,
        );
        assert_eq!(backoff.delay_micros, 81_000_000);
        let snooze = outcome(
            Ended::Error(JobError::snooze(Duration::from_micros(7)).unwrap()),
            25,
        );
        assert_eq!(snooze.outcome, Outcome::Snoozed);
        assert_eq!(snooze.delay_micros, 7);
        assert!(snooze.summary.is_none());
        assert_eq!(
            outcome(Ended::Error(JobError::transaction_unknown("commit")), 25).outcome,
            Outcome::TransactionUnknown
        );
        assert_eq!(
            outcome(Ended::Panic, 1).summary.as_deref(),
            Some("handler panicked")
        );
        assert_eq!(outcome(Ended::Timeout, 1).outcome, Outcome::Timeout);
        assert_eq!(
            outcome(Ended::Error(JobError::permanent("stop")), 1).outcome,
            Outcome::Permanent
        );
    }

    #[test]
    fn sql_draw_determines_fixed_microsecond_jitter() {
        for attempt in 1..=25 {
            let base = i64::from(attempt).pow(4) * 1_000_000;
            assert_eq!(backoff(attempt, 0.5), base);
            assert_eq!(backoff(attempt, 0.0), base * 9 / 10);
            assert!(backoff(attempt, 0.999_999) < base * 11 / 10);
        }
    }

    #[test]
    fn summaries_sanitize_controls_and_preserve_utf8_without_payload_disclosure() {
        assert_eq!(summary("a\nb\0c"), "a b c");
        assert_eq!(summary(&("a".repeat(1023) + "é")), "a".repeat(1023));
        let error = serde_json::from_str::<u32>("\"secret\"").unwrap_err();
        assert_eq!(
            payload_summary("sample", &error),
            "payload does not decode as sample: data error at line 1 column 8"
        );
    }
}
