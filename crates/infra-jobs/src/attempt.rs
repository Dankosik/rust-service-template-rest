//! One attempt: the supervisor, the outcome write, and the attempt records.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use infra_postgres::in_tx_with;
use sqlx::postgres::PgConnection;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::engine::{
    Handles, OpFailed, Operation, OperationError, READ_COMMITTED, RECORD_RETRY_INTERVAL, Shared,
    backstop, observe_failure, observe_recovery,
};
use crate::kind::{HandlerFuture, JobError, JobId, Policy};
use crate::lease::{self, JobState, RowState};
use crate::traceparent;

/// The longest stored failure summary, in bytes.
pub const ERROR_SUMMARY_MAX_BYTES: usize = 1024;
/// Counter of claimed attempts and claim-time exhaustions. Labels `kind`, `outcome`.
pub const ATTEMPTS_METRIC: &str = "jobs_attempts_total";
/// Handler run time. Label `kind`.
pub const ATTEMPT_DURATION_METRIC: &str = "jobs_attempt_duration_seconds";
/// Histogram buckets for [`ATTEMPT_DURATION_METRIC`], in seconds.
pub const ATTEMPT_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0,
    1800.0, 3600.0,
];

const COMPLETE: &str = "UPDATE background_jobs \
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

/// A recorded attempt outcome. [`Outcome::as_str`] is the `outcome` label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Completed,
    Retry,
    Timeout,
    Exhausted,
    Permanent,
    Released,
    Superseded,
    Lost,
}

impl Outcome {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Retry => "retry",
            Self::Timeout => "timeout",
            Self::Exhausted => "exhausted",
            Self::Permanent => "permanent",
            Self::Released => "released",
            Self::Superseded => "superseded",
            Self::Lost => "lost",
        }
    }
}

/// Why a retryable attempt failed. [`Failure::as_str`] is the `failure` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Error,
    Panic,
    Payload,
    Timeout,
}

impl Failure {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Panic => "panic",
            Self::Payload => "payload",
            Self::Timeout => "timeout",
        }
    }
}

/// Why an attempt was recorded `lost`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LostReason {
    Extension,
    Record,
    Release,
}

impl LostReason {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Extension => "extension",
            Self::Record => "record",
            Self::Release => "release",
        }
    }
}

/// The outcome the supervisor will write, stored by [`crate::engine::Attempts::settle`].
#[derive(Clone, Debug)]
pub(crate) struct Intended {
    pub(crate) outcome: Outcome,
    pub(crate) summary: Option<String>,
    pub(crate) failure: Option<Failure>,
    pub(crate) retry_in: Option<Duration>,
    pub(crate) ran: Option<Duration>,
}

/// Fields for the one [`record`] call.
#[derive(Debug)]
pub(crate) struct Facts<'a> {
    pub(crate) id: JobId,
    pub(crate) kind: &'static str,
    pub(crate) attempt: u16,
    pub(crate) summary: Option<&'a str>,
    pub(crate) failure: Option<Failure>,
    pub(crate) retry_in: Option<Duration>,
    pub(crate) lost: Option<LostReason>,
    pub(crate) ran: Option<Duration>,
}

/// What RECONCILE proved about this claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attribution {
    Written,
    Released,
    Superseded,
    Unapplied,
}

/// Map a RECONCILE row to this claim. `before_deadline` is the result's arrival.
#[must_use]
pub(crate) fn attribute(
    generation: i64,
    attempt: u16,
    row: Option<&RowState>,
    before_deadline: bool,
) -> Attribution {
    let Some(row) = row else {
        return Attribution::Superseded;
    };
    if row.generation != generation {
        return if before_deadline {
            Attribution::Written
        } else {
            Attribution::Superseded
        };
    }
    let attempt_i32 = i32::from(attempt);
    let attempts = i32::from(row.attempts);
    match row.state {
        JobState::Pending if attempts == attempt_i32 - 1 => Attribution::Released,
        JobState::Completed | JobState::Failed => Attribution::Written,
        JobState::Pending if attempts == attempt_i32 => Attribution::Written,
        JobState::Running | JobState::Pending => Attribution::Unapplied,
    }
}

/// The record for an attempt RELEASE did not return. A `Lost` result has reason `release`.
#[must_use]
pub(crate) fn drain_outcome(
    attribution: Option<Attribution>,
    newer: bool,
    release_unknown: bool,
    intended: Option<Outcome>,
) -> Outcome {
    match attribution {
        Some(Attribution::Released) => Outcome::Released,
        Some(Attribution::Written) if !newer => intended.unwrap_or(Outcome::Lost),
        Some(Attribution::Written) => match intended {
            Some(outcome @ (Outcome::Retry | Outcome::Timeout)) => outcome,
            _ if release_unknown => Outcome::Released,
            _ => Outcome::Superseded,
        },
        Some(Attribution::Superseded) => Outcome::Superseded,
        Some(Attribution::Unapplied) | None => Outcome::Lost,
    }
}

/// The one emitter of `jobs_attempts_total`, the attempt records, and the duration histogram.
pub(crate) fn record(outcome: Outcome, facts: &Facts<'_>) {
    metrics::counter!(ATTEMPTS_METRIC, "kind" => facts.kind, "outcome" => outcome.as_str())
        .increment(1);
    if let Some(ran) = facts.ran {
        metrics::histogram!(ATTEMPT_DURATION_METRIC, "kind" => facts.kind)
            .record(ran.as_secs_f64());
    }
    let attempt = u64::from(facts.attempt);
    match outcome {
        Outcome::Completed => {
            tracing::debug!(
                job.id = %facts.id,
                job.kind = facts.kind,
                job.attempt = attempt,
                duration_ms = millis(facts.ran),
                "job_attempt_completed"
            );
        }
        Outcome::Retry | Outcome::Timeout => {
            tracing::info!(
                job.id = %facts.id,
                job.kind = facts.kind,
                job.attempt = attempt,
                failure = facts.failure.map_or("", Failure::as_str),
                retry_in_ms = millis(facts.retry_in),
                error = facts.summary.unwrap_or(""),
                "job_attempt_failed"
            );
        }
        Outcome::Exhausted | Outcome::Permanent => {
            tracing::warn!(
                job.id = %facts.id,
                job.kind = facts.kind,
                job.attempts = attempt,
                job.failure_reason = failure_reason(outcome),
                error = facts.summary.unwrap_or(""),
                "job_failed"
            );
        }
        Outcome::Superseded => {
            tracing::info!(
                job.id = %facts.id,
                job.kind = facts.kind,
                job.attempt = attempt,
                "job_attempt_superseded"
            );
        }
        Outcome::Lost => {
            tracing::warn!(
                job.id = %facts.id,
                job.kind = facts.kind,
                job.attempt = attempt,
                reason = facts.lost.map_or("", LostReason::as_str),
                "job_claim_lost"
            );
        }
        Outcome::Released => {}
    }
}

/// Run one admitted attempt. The slot permit is released on every exit, abort included.
pub(crate) async fn supervise(
    shared: std::sync::Arc<Shared>,
    claimed: crate::claim::Claimed,
    handles: Handles,
) {
    let crate::claim::Claimed {
        id,
        generation,
        kind,
        attempt,
        payload,
        trace_context,
        slot,
    } = claimed;
    let _slot = slot;
    let span = tracing::info_span!(
        "job_attempt",
        job.id = %id,
        job.kind = kind,
        job.attempt = u64::from(attempt),
        otel.kind = "consumer",
    );
    if let Some(text) = trace_context.as_deref() {
        traceparent::link(&span, text);
    }
    let attempt = AttemptId {
        id,
        generation,
        kind,
        attempt,
    };
    run_attempt(shared, attempt, payload, handles)
        .instrument(span)
        .await;
}

struct AttemptId {
    id: JobId,
    generation: i64,
    kind: &'static str,
    attempt: u16,
}

enum Start {
    Stop,
    Payload {
        policy: Policy,
        error: serde_json::Error,
    },
    Ready {
        policy: Policy,
        fut: HandlerFuture,
    },
}

async fn run_attempt(
    shared: std::sync::Arc<Shared>,
    attempt: AttemptId,
    payload: Vec<u8>,
    handles: Handles,
) {
    match classify(&shared, &attempt, &payload, &handles) {
        Start::Stop => record_taken(
            &shared,
            &attempt,
            Outcome::Lost,
            &side_facts(&attempt, None, Some(LostReason::Extension)),
        ),
        Start::Payload { policy, error } => {
            let intended = map_outcome(
                attempt.id,
                attempt.kind,
                attempt.attempt,
                policy,
                Ended::Payload(error),
                None,
            );
            let deadline = *handles.deadline.borrow();
            settle_and_resolve(&shared, &attempt, deadline, intended).await;
        }
        Start::Ready { policy, fut } => drive(&shared, &attempt, policy, fut, handles).await,
    }
}

fn classify(shared: &Shared, attempt: &AttemptId, payload: &[u8], handles: &Handles) -> Start {
    let Some(registered) = shared.registry.get(attempt.kind) else {
        return Start::Stop;
    };
    if Instant::now() >= *handles.deadline.borrow() {
        return Start::Stop;
    }
    let policy = registered.policy;
    match registered.dispatch.prepare(
        attempt.id,
        attempt.attempt,
        payload,
        handles.handler_cancel.clone(),
        shared.pool.clone(),
    ) {
        Ok(fut) => Start::Ready { policy, fut },
        Err(error) => Start::Payload { policy, error },
    }
}

async fn drive(
    shared: &Shared,
    attempt: &AttemptId,
    policy: Policy,
    fut: HandlerFuture,
    handles: Handles,
) {
    let Some(mut join) = spawn_handler(shared, attempt, fut) else {
        return;
    };
    let started = Instant::now();
    let Handles {
        mut deadline,
        superseded,
        handler_cancel,
    } = handles;
    let timeout = tokio::time::sleep(policy.timeout);
    tokio::pin!(timeout);
    loop {
        let at = *deadline.borrow();
        tokio::select! {
            biased;
            () = superseded.cancelled() => {
                end_running(shared, attempt, &handler_cancel, &join, started, Outcome::Superseded, None);
                return;
            }
            result = &mut join => {
                let intended = map_outcome(
                    attempt.id,
                    attempt.kind,
                    attempt.attempt,
                    policy,
                    ended_from(result),
                    Some(started.elapsed()),
                );
                let at_settle = *deadline.borrow();
                settle_and_resolve(shared, attempt, at_settle, intended).await;
                return;
            }
            () = &mut timeout => {
                stop_handler(&handler_cancel, &join);
                let intended = map_outcome(
                    attempt.id,
                    attempt.kind,
                    attempt.attempt,
                    policy,
                    Ended::Timeout,
                    Some(started.elapsed()),
                );
                let at_settle = *deadline.borrow();
                settle_and_resolve(shared, attempt, at_settle, intended).await;
                return;
            }
            () = tokio::time::sleep_until(at) => {
                end_running(
                    shared,
                    attempt,
                    &handler_cancel,
                    &join,
                    started,
                    Outcome::Lost,
                    Some(LostReason::Extension),
                );
                return;
            }
            changed = deadline.changed() => {
                if changed.is_err() {
                    stop_handler(&handler_cancel, &join);
                    return;
                }
            }
        }
    }
}

fn spawn_handler(
    shared: &Shared,
    attempt: &AttemptId,
    fut: HandlerFuture,
) -> Option<JoinHandle<Result<(), JobError>>> {
    let mut join = None;
    let started = shared
        .attempts
        .start_handler(attempt.id, attempt.generation, || {
            let task = tokio::spawn(fut.instrument(tracing::Span::current()));
            let abort = task.abort_handle();
            join = Some(task);
            abort
        });
    if started { join } else { None }
}

fn ended_from(result: Result<Result<(), JobError>, JoinError>) -> Ended {
    match result {
        Ok(Ok(())) => Ended::Success,
        Ok(Err(error)) => Ended::Error(error),
        Err(_) => Ended::Panic,
    }
}

fn end_running(
    shared: &Shared,
    attempt: &AttemptId,
    cancel: &CancellationToken,
    join: &JoinHandle<Result<(), JobError>>,
    started: Instant,
    outcome: Outcome,
    lost: Option<LostReason>,
) {
    stop_handler(cancel, join);
    record_taken(
        shared,
        attempt,
        outcome,
        &side_facts(attempt, Some(started.elapsed()), lost),
    );
}

fn stop_handler(cancel: &CancellationToken, join: &JoinHandle<Result<(), JobError>>) {
    cancel.cancel();
    join.abort();
}

async fn settle_and_resolve(
    shared: &Shared,
    attempt: &AttemptId,
    deadline: Instant,
    intended: Intended,
) {
    if !shared
        .attempts
        .settle(attempt.id, attempt.generation, intended.clone())
    {
        return;
    }
    resolve_write(shared, attempt, deadline, &intended).await;
}

async fn resolve_write(
    shared: &Shared,
    attempt: &AttemptId,
    deadline: Instant,
    intended: &Intended,
) {
    loop {
        if Instant::now() >= deadline {
            record_lost_record(shared, attempt, intended.ran);
            return;
        }
        match send_bounded(shared, attempt, intended, deadline).await {
            Send::Applied => {
                observe_recovery(shared, Operation::Record);
                record_taken(
                    shared,
                    attempt,
                    intended.outcome,
                    &facts_intended(attempt, intended),
                );
                return;
            }
            Send::Unchanged => {
                observe_recovery(shared, Operation::Record);
                if reconcile_write(shared, attempt, deadline, intended).await {
                    return;
                }
            }
            Send::Unknown => {
                observe_failure(shared, Operation::Record, OperationError::CommitUnknown);
                if reconcile_write(shared, attempt, deadline, intended).await {
                    return;
                }
            }
            Send::Failed(error) => {
                observe_failure(shared, Operation::Record, error);
                sleep_capped(deadline, Duration::from_secs(1)).await;
            }
            Send::Expired => {
                record_lost_record(shared, attempt, intended.ran);
                return;
            }
        }
    }
}

enum Send {
    Applied,
    Unchanged,
    Unknown,
    Failed(OperationError),
    Expired,
}

async fn send_bounded(
    shared: &Shared,
    attempt: &AttemptId,
    intended: &Intended,
    deadline: Instant,
) -> Send {
    let send = send_outcome(shared, attempt.id, attempt.generation, intended);
    match tokio::time::timeout_at(deadline, send).await {
        Err(_elapsed) => Send::Expired,
        Ok(Ok(1)) => Send::Applied,
        Ok(Ok(_)) => Send::Unchanged,
        Ok(Err(OperationError::CommitUnknown)) => Send::Unknown,
        Ok(Err(error)) => Send::Failed(error),
    }
}

async fn send_outcome(
    shared: &Shared,
    id: JobId,
    generation: i64,
    intended: &Intended,
) -> Result<u64, OperationError> {
    let returned = AtomicBool::new(false);
    let id_text = id.to_string();
    let summary = intended.summary.clone().unwrap_or_default();
    let delay = intended.retry_in.unwrap_or(Duration::ZERO);
    let outcome = intended.outcome;
    backstop(
        &returned,
        in_tx_with(
            &shared.pool,
            READ_COMMITTED,
            async |conn| -> Result<u64, OpFailed> {
                let affected =
                    execute(conn, &id_text, generation, outcome, delay, &summary).await?;
                returned.store(true, Ordering::SeqCst);
                Ok(affected)
            },
        ),
    )
    .await
}

async fn execute(
    conn: &mut PgConnection,
    id_text: &str,
    generation: i64,
    outcome: Outcome,
    delay: Duration,
    summary: &str,
) -> Result<u64, OpFailed> {
    let affected = match outcome {
        Outcome::Completed => sqlx::query(COMPLETE)
            .bind(id_text)
            .bind(generation)
            .execute(&mut *conn)
            .await?
            .rows_affected(),
        Outcome::Retry | Outcome::Timeout => sqlx::query(RETRY)
            .bind(id_text)
            .bind(generation)
            .bind(delay)
            .bind(summary)
            .execute(&mut *conn)
            .await?
            .rows_affected(),
        Outcome::Exhausted => fail(conn, id_text, generation, "exhausted", summary).await?,
        Outcome::Permanent => fail(conn, id_text, generation, "permanent", summary).await?,
        Outcome::Released | Outcome::Superseded | Outcome::Lost => 0,
    };
    Ok(affected)
}

async fn fail(
    conn: &mut PgConnection,
    id_text: &str,
    generation: i64,
    reason: &str,
    summary: &str,
) -> Result<u64, OpFailed> {
    let affected = sqlx::query(FAIL)
        .bind(id_text)
        .bind(generation)
        .bind(reason)
        .bind(summary)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    Ok(affected)
}

async fn reconcile_write(
    shared: &Shared,
    attempt: &AttemptId,
    deadline: Instant,
    intended: &Intended,
) -> bool {
    let ids = [attempt.id];
    let read = lease::reconcile(shared, &ids);
    match tokio::time::timeout_at(deadline, read).await {
        Err(_elapsed) => {
            record_lost_record(shared, attempt, intended.ran);
            true
        }
        Ok(Err(error)) => {
            observe_failure(shared, Operation::Reconcile, error);
            sleep_capped(deadline, Duration::from_secs(1)).await;
            false
        }
        Ok(Ok(rows)) => {
            observe_recovery(shared, Operation::Reconcile);
            let row = rows.iter().find(|row| row.id == attempt.id);
            let attribution = attribute(
                attempt.generation,
                attempt.attempt,
                row,
                Instant::now() < deadline,
            );
            apply_attribution(shared, attempt, deadline, intended, attribution).await
        }
    }
}

async fn apply_attribution(
    shared: &Shared,
    attempt: &AttemptId,
    deadline: Instant,
    intended: &Intended,
    attribution: Attribution,
) -> bool {
    match attribution {
        Attribution::Unapplied => {
            sleep_capped(deadline, RECORD_RETRY_INTERVAL).await;
            false
        }
        Attribution::Written => {
            record_taken(
                shared,
                attempt,
                intended.outcome,
                &facts_intended(attempt, intended),
            );
            true
        }
        Attribution::Superseded => {
            record_taken(
                shared,
                attempt,
                Outcome::Superseded,
                &side_facts(attempt, intended.ran, None),
            );
            true
        }
        Attribution::Released => {
            record_taken(
                shared,
                attempt,
                Outcome::Released,
                &side_facts(attempt, intended.ran, None),
            );
            true
        }
    }
}

fn record_lost_record(shared: &Shared, attempt: &AttemptId, ran: Option<Duration>) {
    record_taken(
        shared,
        attempt,
        Outcome::Lost,
        &side_facts(attempt, ran, Some(LostReason::Record)),
    );
}

fn record_taken(shared: &Shared, attempt: &AttemptId, outcome: Outcome, facts: &Facts<'_>) {
    if shared
        .attempts
        .take(attempt.id, attempt.generation)
        .is_some()
    {
        record(outcome, facts);
    }
}

fn facts_intended<'a>(attempt: &'a AttemptId, intended: &'a Intended) -> Facts<'a> {
    Facts {
        id: attempt.id,
        kind: attempt.kind,
        attempt: attempt.attempt,
        summary: intended.summary.as_deref(),
        failure: intended.failure,
        retry_in: intended.retry_in,
        lost: None,
        ran: intended.ran,
    }
}

fn side_facts(attempt: &AttemptId, ran: Option<Duration>, lost: Option<LostReason>) -> Facts<'_> {
    Facts {
        id: attempt.id,
        kind: attempt.kind,
        attempt: attempt.attempt,
        summary: None,
        failure: None,
        retry_in: None,
        lost,
        ran,
    }
}

fn failure_reason(outcome: Outcome) -> &'static str {
    if outcome == Outcome::Permanent {
        "permanent"
    } else {
        "exhausted"
    }
}

fn millis(duration: Option<Duration>) -> u64 {
    duration
        .and_then(|value| u64::try_from(value.as_millis()).ok())
        .unwrap_or(0)
}

async fn sleep_capped(deadline: Instant, delay: Duration) {
    let until = match Instant::now().checked_add(delay) {
        Some(at) if at < deadline => at,
        _ => deadline,
    };
    tokio::time::sleep_until(until).await;
}

enum Ended {
    Success,
    Error(JobError),
    Panic,
    Payload(serde_json::Error),
    Timeout,
}

fn map_outcome(
    id: JobId,
    kind: &'static str,
    attempt: u16,
    policy: Policy,
    ended: Ended,
    ran: Option<Duration>,
) -> Intended {
    let last = attempt >= policy.max_attempts;
    match ended {
        Ended::Success => Intended {
            outcome: Outcome::Completed,
            summary: None,
            failure: None,
            retry_in: None,
            ran,
        },
        Ended::Error(error) if error.is_permanent() => Intended {
            outcome: Outcome::Permanent,
            summary: Some(summary(&error.to_string())),
            failure: None,
            retry_in: None,
            ran,
        },
        Ended::Error(error) => retry_or_exhausted(
            last,
            id,
            attempt,
            Failure::Error,
            summary(&error.to_string()),
            ran,
        ),
        Ended::Panic => retry_or_exhausted(
            last,
            id,
            attempt,
            Failure::Panic,
            summary("handler panicked"),
            ran,
        ),
        Ended::Payload(error) => retry_or_exhausted(
            last,
            id,
            attempt,
            Failure::Payload,
            summary(&payload_summary(kind, &error)),
            ran,
        ),
        Ended::Timeout => timeout_outcome(id, attempt, policy, last, ran),
    }
}

fn retry_or_exhausted(
    last: bool,
    id: JobId,
    attempt: u16,
    failure: Failure,
    summary: String,
    ran: Option<Duration>,
) -> Intended {
    if last {
        Intended {
            outcome: Outcome::Exhausted,
            summary: Some(summary),
            failure: Some(failure),
            retry_in: None,
            ran,
        }
    } else {
        Intended {
            outcome: Outcome::Retry,
            summary: Some(summary),
            failure: Some(failure),
            retry_in: Some(backoff(id, attempt)),
            ran,
        }
    }
}

fn timeout_outcome(
    id: JobId,
    attempt: u16,
    policy: Policy,
    last: bool,
    ran: Option<Duration>,
) -> Intended {
    let summary = summary(&timeout_summary(policy.timeout));
    if last {
        Intended {
            outcome: Outcome::Exhausted,
            summary: Some(summary),
            failure: Some(Failure::Timeout),
            retry_in: None,
            ran,
        }
    } else {
        Intended {
            outcome: Outcome::Timeout,
            summary: Some(summary),
            failure: Some(Failure::Timeout),
            retry_in: Some(backoff(id, attempt)),
            ran,
        }
    }
}

fn timeout_summary(timeout: Duration) -> String {
    format!(
        "attempt timed out after {}",
        humantime::format_duration(timeout)
    )
}

fn payload_summary(kind: &str, err: &serde_json::Error) -> String {
    let category = category(err);
    let line = err.line();
    let column = err.column();
    format!("payload does not decode as {kind}: {category} error at line {line} column {column}")
}

fn category(err: &serde_json::Error) -> &'static str {
    match err.classify() {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
    }
}

fn summary(text: &str) -> String {
    let sanitized: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    cut_bytes(&sanitized, ERROR_SUMMARY_MAX_BYTES)
}

fn cut_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn backoff(id: JobId, n: u16) -> Duration {
    let (hi, lo) = id.as_u64_pair();
    let n = u64::from(n);
    let x = hi ^ lo ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let z = splitmix64(x);
    let p = 900 + (z % 201);
    let n4 = n * n * n * n;
    let delay_ms = n4 * 1000 * p / 1000;
    Duration::from_millis(delay_ms)
}

fn splitmix64(x: u64) -> u64 {
    let mut z = x ^ (x >> 30);
    z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z ^= z >> 27;
    z = z.wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        Attribution, Ended, Failure, LostReason, Outcome, attribute, backoff, drain_outcome,
        map_outcome, summary,
    };
    use crate::kind::{JobError, JobId, Policy};
    use crate::lease::{JobState, RowState};

    fn id(text: &str) -> JobId {
        JobId::parse(text).unwrap()
    }

    fn policy(max_attempts: u16) -> Policy {
        Policy {
            max_attempts,
            timeout: Duration::from_secs(60),
        }
    }

    fn sample() -> JobId {
        id("01234567-89ab-cdef-fedc-ba9876543210")
    }

    #[test]
    fn outcome_labels() {
        assert_eq!(Outcome::Completed.as_str(), "completed");
        assert_eq!(Outcome::Retry.as_str(), "retry");
        assert_eq!(Outcome::Timeout.as_str(), "timeout");
        assert_eq!(Outcome::Exhausted.as_str(), "exhausted");
        assert_eq!(Outcome::Permanent.as_str(), "permanent");
        assert_eq!(Outcome::Released.as_str(), "released");
        assert_eq!(Outcome::Superseded.as_str(), "superseded");
        assert_eq!(Outcome::Lost.as_str(), "lost");
        assert_eq!(Failure::Error.as_str(), "error");
        assert_eq!(Failure::Panic.as_str(), "panic");
        assert_eq!(Failure::Payload.as_str(), "payload");
        assert_eq!(Failure::Timeout.as_str(), "timeout");
        assert_eq!(LostReason::Extension.as_str(), "extension");
        assert_eq!(LostReason::Record.as_str(), "record");
        assert_eq!(LostReason::Release.as_str(), "release");
    }

    #[test]
    fn maps_every_x7_row() {
        let id = sample();
        let open = policy(25);
        let success = map_outcome(id, "sample", 1, open, Ended::Success, None);
        assert_eq!(success.outcome, Outcome::Completed);
        assert!(success.summary.is_none());
        assert!(success.failure.is_none());
        assert!(success.retry_in.is_none());

        let retryable = map_outcome(
            id,
            "sample",
            1,
            open,
            Ended::Error(JobError::retryable("disk failed")),
            None,
        );
        assert_eq!(retryable.outcome, Outcome::Retry);
        assert_eq!(retryable.failure, Some(Failure::Error));
        assert_eq!(retryable.summary.as_deref(), Some("disk failed"));
        assert!(retryable.retry_in.is_some());

        let last = map_outcome(
            id,
            "sample",
            25,
            open,
            Ended::Error(JobError::retryable("disk failed")),
            None,
        );
        assert_eq!(last.outcome, Outcome::Exhausted);
        assert_eq!(last.failure, Some(Failure::Error));
        assert!(last.retry_in.is_none());

        let permanent = map_outcome(
            id,
            "sample",
            1,
            open,
            Ended::Error(JobError::permanent("stop")),
            None,
        );
        assert_eq!(permanent.outcome, Outcome::Permanent);
        assert_eq!(permanent.summary.as_deref(), Some("stop"));
        assert!(permanent.failure.is_none());
        assert!(permanent.retry_in.is_none());

        let panic = map_outcome(id, "sample", 1, open, Ended::Panic, None);
        assert_eq!(panic.outcome, Outcome::Retry);
        assert_eq!(panic.failure, Some(Failure::Panic));
        assert_eq!(panic.summary.as_deref(), Some("handler panicked"));

        let timed = map_outcome(id, "sample", 1, open, Ended::Timeout, None);
        assert_eq!(timed.outcome, Outcome::Timeout);
        assert_eq!(timed.failure, Some(Failure::Timeout));
        assert_eq!(timed.summary.as_deref(), Some("attempt timed out after 1m"));
        assert!(timed.retry_in.is_some());

        let timed_last = map_outcome(id, "sample", 25, open, Ended::Timeout, None);
        assert_eq!(timed_last.outcome, Outcome::Exhausted);
        assert_eq!(timed_last.failure, Some(Failure::Timeout));
        assert_eq!(
            timed_last.summary.as_deref(),
            Some("attempt timed out after 1m")
        );
        assert!(timed_last.retry_in.is_none());
    }

    #[test]
    fn payload_summary_uses_category_line_and_column() {
        let err = serde_json::from_slice::<u32>(b"x").unwrap_err();
        assert_eq!(err.classify(), serde_json::error::Category::Syntax);
        let line = err.line();
        let column = err.column();
        let expected = format!(
            "payload does not decode as sample: syntax error at line {line} column {column}"
        );
        assert_eq!(
            expected,
            "payload does not decode as sample: syntax error at line 1 column 1"
        );
        let intended = map_outcome(
            sample(),
            "sample",
            1,
            policy(25),
            Ended::Payload(serde_json::from_slice::<u32>(b"x").unwrap_err()),
            None,
        );
        assert_eq!(intended.outcome, Outcome::Retry);
        assert_eq!(intended.failure, Some(Failure::Payload));
        assert_eq!(intended.summary.as_deref(), Some(expected.as_str()));
        assert!(!intended.summary.unwrap().contains("expected value"));
    }

    #[test]
    fn summaries_replace_controls_and_cut_on_a_char_boundary() {
        assert_eq!(summary("a\nb\u{0}c"), "a b c");
        assert_eq!(summary("plain"), "plain");
        let exact = "b".repeat(1024);
        assert_eq!(summary(&exact), exact);
        let mut straddling = "a".repeat(1023);
        straddling.push('é');
        assert_eq!(straddling.len(), 1025);
        let cut = summary(&straddling);
        assert_eq!(cut, "a".repeat(1023));
        assert_eq!(cut.len(), 1023);
        assert_eq!(summary(&"c".repeat(1025)).len(), 1024);
    }

    #[test]
    fn backoff_matches_pinned_vectors_and_the_ten_percent_bound() {
        let first = id("01234567-89ab-cdef-fedc-ba9876543210");
        assert_eq!(backoff(first, 1).as_millis(), 917);
        assert_eq!(backoff(first, 2).as_millis(), 15728);
        assert_eq!(backoff(first, 3).as_millis(), 81000);
        assert_eq!(backoff(first, 10).as_millis(), 10_450_000);
        assert_eq!(backoff(first, 24).as_millis(), 318_836_736);
        let second = id("9f1c2e4a-7b3d-4c5e-8f60-a1b2c3d4e5f6");
        assert_eq!(backoff(second, 1).as_millis(), 1046);
        assert_eq!(backoff(second, 2).as_millis(), 14848);
        assert_eq!(backoff(second, 3).as_millis(), 77598);
        assert_eq!(backoff(second, 10).as_millis(), 10_680_000);
        assert_eq!(backoff(second, 24).as_millis(), 309_878_784);
        let zero = id("00000000-0000-0000-0000-000000000000");
        assert_eq!(backoff(zero, 1).as_millis(), 970);
        assert_eq!(backoff(zero, 24).as_millis(), 345_710_592);
        for job in [first, second, zero] {
            for n in 1..=24u16 {
                let ms = u64::try_from(backoff(job, n).as_millis()).unwrap();
                let n4 = u64::from(n).pow(4);
                assert!((n4 * 900..=n4 * 1100).contains(&ms), "{job} n={n} ms={ms}");
            }
        }
    }

    #[test]
    fn attribute_covers_the_reconciliation_table() {
        let generation = 10;
        let attempt = 3u16;
        assert_eq!(
            attribute(generation, attempt, None, true),
            Attribution::Superseded
        );
        assert_eq!(
            attribute(generation, attempt, None, false),
            Attribution::Superseded
        );
        let other = row(11, JobState::Running, 4);
        assert_eq!(
            attribute(generation, attempt, Some(&other), true),
            Attribution::Written
        );
        assert_eq!(
            attribute(generation, attempt, Some(&other), false),
            Attribution::Superseded
        );
        let running = row(generation, JobState::Running, 3);
        assert_eq!(
            attribute(generation, attempt, Some(&running), true),
            Attribution::Unapplied
        );
        let released = row(generation, JobState::Pending, 2);
        assert_eq!(
            attribute(generation, attempt, Some(&released), true),
            Attribution::Released
        );
        let completed = row(generation, JobState::Completed, 3);
        assert_eq!(
            attribute(generation, attempt, Some(&completed), false),
            Attribution::Written
        );
        let failed = row(generation, JobState::Failed, 9);
        assert_eq!(
            attribute(generation, attempt, Some(&failed), true),
            Attribution::Written
        );
        let pending = row(generation, JobState::Pending, 3);
        assert_eq!(
            attribute(generation, attempt, Some(&pending), true),
            Attribution::Written
        );
        let other_pending = row(generation, JobState::Pending, 1);
        assert_eq!(
            attribute(generation, attempt, Some(&other_pending), true),
            Attribution::Unapplied
        );
    }

    fn row(generation: i64, state: JobState, attempts: i16) -> RowState {
        RowState {
            id: sample(),
            generation,
            state,
            attempts,
        }
    }

    #[test]
    fn drain_outcome_covers_the_drain_end_table() {
        let classes = [
            None,
            Some(Outcome::Completed),
            Some(Outcome::Permanent),
            Some(Outcome::Exhausted),
            Some(Outcome::Retry),
            Some(Outcome::Timeout),
        ];
        for (newer, unknown, intended) in combinations(&classes) {
            assert_eq!(
                drain_outcome(Some(Attribution::Released), newer, unknown, intended),
                Outcome::Released,
                "released {newer} {unknown} {intended:?}"
            );
            assert_eq!(
                drain_outcome(Some(Attribution::Superseded), newer, unknown, intended),
                Outcome::Superseded,
                "superseded {newer} {unknown} {intended:?}"
            );
            assert_eq!(
                drain_outcome(Some(Attribution::Unapplied), newer, unknown, intended),
                Outcome::Lost,
                "unapplied {newer} {unknown} {intended:?}"
            );
            assert_eq!(
                drain_outcome(None, newer, unknown, intended),
                Outcome::Lost,
                "none {newer} {unknown} {intended:?}"
            );
        }
        for unknown in [false, true] {
            assert_eq!(
                drain_outcome(Some(Attribution::Written), false, unknown, None),
                Outcome::Lost
            );
            for intended in [
                Outcome::Completed,
                Outcome::Permanent,
                Outcome::Exhausted,
                Outcome::Retry,
                Outcome::Timeout,
            ] {
                assert_eq!(
                    drain_outcome(Some(Attribution::Written), false, unknown, Some(intended)),
                    intended
                );
            }
        }
        for intended in [Outcome::Retry, Outcome::Timeout] {
            assert_eq!(
                drain_outcome(Some(Attribution::Written), true, false, Some(intended)),
                intended
            );
            assert_eq!(
                drain_outcome(Some(Attribution::Written), true, true, Some(intended)),
                intended
            );
        }
        for intended in [
            None,
            Some(Outcome::Completed),
            Some(Outcome::Permanent),
            Some(Outcome::Exhausted),
        ] {
            assert_eq!(
                drain_outcome(Some(Attribution::Written), true, false, intended),
                Outcome::Superseded,
                "known {intended:?}"
            );
            assert_eq!(
                drain_outcome(Some(Attribution::Written), true, true, intended),
                Outcome::Released,
                "unknown {intended:?}"
            );
        }
    }

    fn combinations(classes: &[Option<Outcome>]) -> Vec<(bool, bool, Option<Outcome>)> {
        let mut rows = Vec::new();
        for newer in [false, true] {
            for unknown in [false, true] {
                for intended in classes {
                    rows.push((newer, unknown, *intended));
                }
            }
        }
        rows
    }
}
