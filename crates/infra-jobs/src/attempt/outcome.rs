//! What an attempt's end becomes in the queue, and the statements that store it.

use infra_postgres::observed;
use sqlx::postgres::PgConnection;
use tokio::time::Instant;

use super::{AttemptId, ClaimFence, Persistence, expired};
use crate::engine::{Operation, OperationError, Shared, backstop, observe_failure};
use crate::kind::{Disposition, JobError, MAX_ERROR_SUMMARY_BYTES, Policy};
use crate::state::FailureReason;

#[derive(Debug)]
pub(super) enum Transition {
    Complete,
    Retry {
        summary: String,
        base_micros: f64,
        cause: RetryCause,
    },
    Fail {
        reason: FailureReason,
        summary: String,
    },
    Snooze {
        delay_micros: i64,
        summary: String,
    },
    Release,
}

impl Transition {
    pub(super) const fn label(&self) -> &'static str {
        match self {
            Self::Complete => "completed",
            Self::Retry {
                cause: RetryCause::Timeout,
                ..
            } => "timeout",
            Self::Retry { .. } => "retry",
            Self::Fail { reason, .. } => reason.as_str(),
            Self::Snooze { .. } => "snoozed",
            Self::Release => "cancelled",
        }
    }
}

/// An outcome that stores its own row: the write-only view of a [`Transition`].
///
/// It carries only the values its statement binds. Labels stay with
/// [`Transition`], so a timed-out retry is a [`Self::Retry`] here.
#[derive(Clone, Copy, Debug)]
pub(super) enum Write<'a> {
    Retry {
        summary: &'a str,
        base_micros: f64,
        floor_micros: i64,
    },
    Snooze {
        delay_micros: i64,
    },
    Fail {
        reason: FailureReason,
        summary: &'a str,
    },
    Release,
}

impl Write<'_> {
    pub(super) const fn operation(self) -> Operation {
        match self {
            Self::Retry { .. } | Self::Snooze { .. } | Self::Fail { .. } => Operation::Record,
            Self::Release => Operation::Release,
        }
    }
}

/// An acknowledged outcome write: whether it changed the job row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Written {
    Applied,
    /// The fenced claim was no longer running, so the write matched no row.
    Unchanged,
}

impl Written {
    const fn from_rows(rows: u64) -> Self {
        if rows == 1 {
            Self::Applied
        } else {
            Self::Unchanged
        }
    }

    pub(super) const fn persistence(self) -> Persistence {
        match self {
            Self::Applied => Persistence::Applied,
            Self::Unchanged => Persistence::Unchanged,
        }
    }
}

/// An outcome write that did not acknowledge and needs no further
/// observation: its writer already counted and logged the failure, or its
/// batch ended without an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Unacknowledged;

/// The write-only view of `transition`: the outcome that stores its own row.
///
/// A completion returns `None`, because
/// [`complete_batched`](super::completion::complete_batched) is the only path
/// that completes an attempt. The match is exhaustive, so a new transition
/// cannot reach a statement without first choosing which of the two writes it
/// takes, and the view carries only the values its statement binds.
pub(super) fn write_of(transition: &Transition) -> Option<Write<'_>> {
    Some(match transition {
        Transition::Complete => return None,
        Transition::Retry {
            summary,
            base_micros,
            cause,
        } => Write::Retry {
            summary,
            base_micros: *base_micros,
            floor_micros: cause.floor_micros(),
        },
        Transition::Snooze { delay_micros, .. } => Write::Snooze {
            delay_micros: *delay_micros,
        },
        Transition::Fail { reason, summary } => Write::Fail {
            reason: *reason,
            summary,
        },
        Transition::Release => Write::Release,
    })
}

pub(super) async fn send_outcome(
    shared: &Shared,
    fence: ClaimFence,
    write: Write<'_>,
    deadline: Instant,
) -> Result<Written, Unacknowledged> {
    // An sqlx statement future is about 16 KiB; box it once so the supervisor stays small.
    let result = backstop(Box::pin(async {
        let mut connection = infra_postgres::acquire(&shared.pool, "record job outcome")
            .await
            .map_err(OperationError::Acquire)?;
        if expired(shared, deadline) {
            return Err(OperationError::TimedOut);
        }
        execute(&mut connection, fence, write)
            .await
            .map_err(OperationError::from)
    }))
    .await;
    result.map(Written::from_rows).map_err(|error| {
        observe_failure(shared, write.operation(), &error);
        Unacknowledged
    })
}

/// Store one attempt outcome. A completion is not among the views: its only
/// write is [`complete_batched`](super::completion::complete_batched).
async fn execute(
    connection: &mut PgConnection,
    fence: ClaimFence,
    write: Write<'_>,
) -> Result<u64, sqlx::Error> {
    let ClaimFence { job_id, generation } = fence;
    let id = job_id.0;
    let (summary, query) = match write {
        Write::Retry {
            summary,
            base_micros,
            floor_micros,
        } => (
            "retry job",
            sqlx::query!(
                "UPDATE background_jobs \
                 SET state = 'pending', not_before = statement_timestamp() + \
                     GREATEST($3::double precision * (0.9 + 0.2 * random()), $4::bigint) * interval '1 microsecond', \
                     claim_expires_at = NULL, error_summary = $5, \
                     errors = errors || jsonb_build_object('attempt', attempts, 'at', statement_timestamp(), 'error', $5::text) \
                 WHERE id = $1 AND claim_generation = $2 AND claim_expires_at IS NOT NULL",
                id,
                generation,
                base_micros,
                floor_micros,
                summary,
            ),
        ),
        Write::Snooze { delay_micros } => {
            let delay = sqlx::postgres::types::PgInterval {
                months: 0,
                days: 0,
                microseconds: delay_micros,
            };
            (
                "snooze job",
                sqlx::query!(
                    "UPDATE background_jobs \
                     SET state = 'pending', not_before = statement_timestamp() + $3, claim_expires_at = NULL, \
                         attempts = attempts - 1 \
                     WHERE id = $1 AND claim_generation = $2 AND claim_expires_at IS NOT NULL",
                    id,
                    generation,
                    delay,
                ),
            )
        }
        // A cancelled attempt gives its unit back and keeps `not_before`, so the job
        // keeps its place in claim order instead of queueing behind the backlog.
        Write::Release => (
            "release job",
            sqlx::query!(
                "UPDATE background_jobs \
                 SET state = 'pending', claim_expires_at = NULL, attempts = attempts - 1 \
                 WHERE id = $1 AND claim_generation = $2 AND claim_expires_at IS NOT NULL",
                id,
                generation,
            ),
        ),
        Write::Fail { reason, summary } => (
            "fail job",
            sqlx::query!(
                "UPDATE background_jobs \
                 SET state = 'failed', failure_reason = $3, finished_at = statement_timestamp(), \
                     claim_expires_at = NULL, error_summary = $4, \
                     errors = errors || jsonb_build_object('attempt', attempts, 'at', statement_timestamp(), 'error', $4::text) \
                 WHERE id = $1 AND claim_generation = $2 AND claim_expires_at IS NOT NULL",
                id,
                generation,
                reason.as_str(),
                summary,
            ),
        ),
    };
    Ok(observed(summary, query.execute(connection))
        .await?
        .rows_affected())
}

/// COMPLETE for one fenced claim, for the caller to execute.
///
/// Every outcome write fences on `claim_expires_at IS NOT NULL`, which the
/// table's CHECK makes equivalent to `state = 'running'`. A literal
/// `state = 'running'` proves the partial `background_jobs_running` predicate,
/// and after ANALYZE saw few running rows the planner scans that whole index
/// for the id, dead entries of every job claimed since the last VACUUM
/// included. This predicate leaves the primary key as the only access path.
pub(crate) fn complete_query(
    fence: ClaimFence,
) -> sqlx::query::Query<'static, sqlx::Postgres, sqlx::postgres::PgArguments> {
    sqlx::query!(
        "UPDATE background_jobs \
         SET state = 'completed', finished_at = statement_timestamp(), claim_expires_at = NULL \
         WHERE id = $1 AND claim_generation = $2 AND claim_expires_at IS NOT NULL",
        fence.job_id.0,
        fence.generation,
    )
}

#[derive(Debug)]
pub(super) enum Ended {
    Success,
    Error(JobError),
    Panic,
    Payload(serde_json::Error),
    Timeout,
    Cancelled,
}

pub(super) fn map_outcome(attempt: &AttemptId, policy: Policy, ended: Ended) -> Transition {
    let number = attempt.number;
    match ended {
        Ended::Success => Transition::Complete,
        Ended::Cancelled => Transition::Release,
        Ended::Error(error) => match error.disposition {
            Disposition::Snooze { delay_micros } => Transition::Snooze {
                delay_micros,
                summary: sanitize_summary(&error.summary),
            },
            Disposition::Permanent => Transition::Fail {
                reason: FailureReason::Permanent,
                summary: sanitize_summary(&error.summary),
            },
            Disposition::RetryAfterAtLeast { floor_micros } => retry_or_exhaust(
                number,
                policy,
                &error.summary,
                RetryCause::Failure { floor_micros },
            ),
            Disposition::Retry => {
                retry_or_exhaust(number, policy, &error.summary, RetryCause::FAILURE)
            }
        },
        Ended::Panic => retry_or_exhaust(number, policy, "handler panicked", RetryCause::FAILURE),
        Ended::Payload(error) => retry_or_exhaust(
            number,
            policy,
            &payload_summary(attempt.kind, &error),
            RetryCause::FAILURE,
        ),
        Ended::Timeout => retry_or_exhaust(
            number,
            policy,
            &format!(
                "attempt timed out after {}",
                humantime::format_duration(policy.timeout)
            ),
            RetryCause::Timeout,
        ),
    }
}

/// Why an attempt spent its unit without completing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetryCause {
    /// The handler failed; the next attempt waits at least `floor_micros`.
    Failure { floor_micros: i64 },
    /// The handler ran past its timeout.
    Timeout,
}

impl RetryCause {
    const FAILURE: Self = Self::Failure { floor_micros: 0 };

    /// The least delay before the next attempt; a timeout sets none.
    const fn floor_micros(self) -> i64 {
        match self {
            Self::Failure { floor_micros } => floor_micros,
            Self::Timeout => 0,
        }
    }
}

/// Retry with backoff, or fail as exhausted once the attempt budget is spent.
///
/// The nominal delay after attempt `n` is `n^4` seconds: 1 s, 16 s, 81 s, and
/// so on. The retry statement in [`execute`] jitters it by ±10% and raises it
/// to the cause's floor.
fn retry_or_exhaust(number: u16, policy: Policy, text: &str, cause: RetryCause) -> Transition {
    let summary = sanitize_summary(text);
    if number >= policy.max_attempts {
        return Transition::Fail {
            reason: FailureReason::Exhausted,
            summary,
        };
    }
    Transition::Retry {
        summary,
        base_micros: f64::from(number).powi(4) * 1_000_000.0,
        cause,
    }
}

/// The stored summary of a payload that does not decode as `kind`.
///
/// The rejected payload is never quoted: it holds untrusted producer or handler
/// input, and a value copied here would be stored, logged, and exported. The
/// category, line, and column locate the defect without it.
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

/// The final stored form of a summary: control characters become spaces, and
/// the UTF-8 length stops at [`MAX_ERROR_SUMMARY_BYTES`] without splitting a
/// character.
fn sanitize_summary(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len().min(MAX_ERROR_SUMMARY_BYTES));
    for ch in text.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        if sanitized.len() + ch.len_utf8() > MAX_ERROR_SUMMARY_BYTES {
            break;
        }
        sanitized.push(ch);
    }
    sanitized
}
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::kind::JobId;

    const ID: JobId = JobId(uuid::Uuid::nil());

    fn outcome(ended: Ended, number: u16) -> Transition {
        let attempt = AttemptId {
            job_id: ID,
            kind: "sample",
            number,
        };
        map_outcome(&attempt, Policy::default(), ended)
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "attempt^4 * 1e6 is exact in f64")]
    fn dispositions_preserve_cap_and_snooze() {
        std::assert_matches!(outcome(Ended::Success, 25), Transition::Complete);
        std::assert_matches!(
            outcome(Ended::Error(JobError::retryable("fail")), 25),
            Transition::Fail {
                reason: FailureReason::Exhausted,
                ..
            }
        );
        std::assert_matches!(
            outcome(
                Ended::Error(
                    JobError::retry_after_at_least("at least", Duration::new(2, 500)).unwrap()
                ),
                1,
            ),
            Transition::Retry {
                base_micros,
                cause: RetryCause::Failure {
                    floor_micros: 2_000_001
                },
                ..
            } if base_micros == 1_000_000.0
        );
        std::assert_matches!(
            outcome(
                Ended::Error(
                    JobError::retry_after_at_least("at least", Duration::from_secs(2)).unwrap()
                ),
                3,
            ),
            Transition::Retry {
                base_micros,
                cause: RetryCause::Failure {
                    floor_micros: 2_000_000
                },
                ..
            } if base_micros == 81_000_000.0
        );
        std::assert_matches!(
            outcome(
                Ended::Error(JobError::snooze(Duration::from_micros(7)).unwrap()),
                25,
            ),
            Transition::Snooze {
                delay_micros: 7,
                ..
            }
        );
        std::assert_matches!(
            outcome(Ended::Error(JobError::unavailable("pool timed out")), 25),
            Transition::Snooze { delay_micros: 10_000_000, ref summary } if summary == "pool timed out"
        );
        std::assert_matches!(
            outcome(Ended::Panic, 1),
            Transition::Retry {
                ref summary,
                cause: RetryCause::FAILURE,
                ..
            } if summary == "handler panicked"
        );
        std::assert_matches!(
            outcome(Ended::Timeout, 1),
            Transition::Retry {
                cause: RetryCause::Timeout,
                ..
            }
        );
        std::assert_matches!(
            outcome(Ended::Error(JobError::permanent("stop")), 1),
            Transition::Fail {
                reason: FailureReason::Permanent,
                ..
            }
        );
    }

    #[test]
    fn a_completed_attempt_selects_no_single_row_write() {
        assert!(write_of(&Transition::Complete).is_none());
        std::assert_matches!(write_of(&Transition::Release), Some(Write::Release));
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "the view copies the exact binding")]
    fn the_write_view_carries_each_statement_binding() {
        let retry = Transition::Retry {
            summary: "retry me".to_owned(),
            base_micros: 81_000_000.0,
            cause: RetryCause::Failure {
                floor_micros: 2_000_000,
            },
        };
        std::assert_matches!(
            write_of(&retry),
            Some(Write::Retry {
                summary,
                base_micros,
                floor_micros,
            }) if summary == "retry me" && base_micros == 81_000_000.0 && floor_micros == 2_000_000
        );
        let snooze = Transition::Snooze {
            delay_micros: 7,
            summary: String::new(),
        };
        std::assert_matches!(write_of(&snooze), Some(Write::Snooze { delay_micros: 7 }));
        let fail = Transition::Fail {
            reason: FailureReason::Exhausted,
            summary: "done".to_owned(),
        };
        std::assert_matches!(
            write_of(&fail),
            Some(Write::Fail {
                reason: FailureReason::Exhausted,
                summary,
            }) if summary == "done"
        );
    }

    #[test]
    fn summaries_sanitize_controls_and_preserve_utf8_without_payload_disclosure() {
        assert_eq!(sanitize_summary("a\nb\0c"), "a b c");
        assert_eq!(sanitize_summary(""), "");
        assert_eq!(sanitize_summary(&"x".repeat(1024)), "x".repeat(1024));
        assert_eq!(sanitize_summary(&"x".repeat(65_536)), "x".repeat(1024));
        assert_eq!(
            sanitize_summary(&("\u{0085}".repeat(1023) + "é")),
            " ".repeat(1023)
        );
        assert_eq!(
            sanitize_summary(&("é".repeat(512) + "tail")),
            "é".repeat(512)
        );
        assert_eq!(
            sanitize_summary(&("a".repeat(1023) + "é")),
            "a".repeat(1023)
        );
        let error = serde_json::from_str::<u32>("\"secret\"").unwrap_err();
        assert_eq!(
            payload_summary("sample", &error),
            "payload does not decode as sample: data error at line 1 column 8"
        );
    }
}
