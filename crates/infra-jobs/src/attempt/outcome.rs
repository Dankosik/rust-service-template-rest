use infra_postgres::observed;
use sqlx::postgres::PgConnection;
use tokio::time::Instant;

use super::{AttemptId, ClaimFence, Persistence, deadline_reached_or_cleanup_ended};
use crate::engine::{Operation, OperationError, Shared, backstop, observe_failure};
use crate::kind::{
    Disposition, JobError, MAX_ATTEMPTS, MAX_DELAY, MAX_ERROR_SUMMARY_BYTES, Policy,
    saturating_micros,
};
use crate::state::FailureReason;

const OUTBOX_MAX_RETRY_BASE_SECONDS: i64 = 54;

#[derive(Debug)]
pub(super) enum Transition {
    Complete,
    Retry {
        summary: String,
        base_micros: i64,
        floor_micros: i64,
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
            Self::Retry {
                cause: RetryCause::Failure,
                ..
            } => "retry",
            Self::Fail { reason, .. } => reason.as_str(),
            Self::Snooze { .. } => "snoozed",
            Self::Release => "cancelled",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Write<'a> {
    Retry {
        summary: &'a str,
        minimum_micros: i64,
        jitter_micros: i64,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Written {
    Applied,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Unacknowledged;

pub(super) fn write_of(transition: &Transition) -> Option<Write<'_>> {
    Some(match transition {
        Transition::Complete => return None,
        Transition::Retry {
            summary,
            base_micros,
            floor_micros: floor,
            ..
        } => {
            let minimum_micros = (base_micros - base_micros / 10).max(*floor);
            let jitter_micros = (base_micros / 5)
                .max(floor / 10)
                .min(saturating_micros(MAX_DELAY) - minimum_micros);
            Write::Retry {
                summary,
                minimum_micros,
                jitter_micros,
            }
        }
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
    let result = backstop(Box::pin(async {
        let mut connection = infra_postgres::acquire(&shared.pool, "record job outcome")
            .await
            .map_err(OperationError::Acquire)?;
        if deadline_reached_or_cleanup_ended(&shared.lifecycle.cleanup, deadline) {
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

async fn execute(
    connection: &mut PgConnection,
    fence: ClaimFence,
    write: Write<'_>,
) -> Result<u64, sqlx::Error> {
    let ClaimFence {
        job_id,
        generation,
        token,
    } = fence;
    let id = job_id.0;
    let (statement_summary, query) = match write {
        Write::Retry {
            summary,
            minimum_micros,
            jitter_micros,
        } => (
            "retry job",
            sqlx::query!(
                "UPDATE background_jobs \
                 SET state = 'pending', not_before = statement_timestamp() + \
                     ($3::bigint + $4::bigint * random()) * interval '1 microsecond', \
                     claim_expires_at = NULL, error_summary = $5, \
                     errors = (CASE WHEN jsonb_array_length(errors) >= $6 THEN errors - 0 ELSE errors END) \
                         || jsonb_build_object('attempt', attempts, 'at', statement_timestamp(), 'error', $5::text) \
                 WHERE id = $1 AND claim_generation = $2 AND claim_token = $7 AND claim_expires_at IS NOT NULL",
                id,
                generation,
                minimum_micros,
                jitter_micros,
                summary,
                i32::from(MAX_ATTEMPTS),
                token,
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
                     WHERE id = $1 AND claim_generation = $2 AND claim_token = $4 AND claim_expires_at IS NOT NULL",
                    id,
                    generation,
                    delay,
                    token,
                ),
            )
        }
        Write::Release => (
            "release job",
            sqlx::query!(
                "UPDATE background_jobs \
                 SET state = 'pending', claim_expires_at = NULL, attempts = attempts - 1 \
                 WHERE id = $1 AND claim_generation = $2 AND claim_token = $3 AND claim_expires_at IS NOT NULL",
                id,
                generation,
                token,
            ),
        ),
        Write::Fail { reason, summary } => (
            "fail job",
            sqlx::query!(
                "UPDATE background_jobs \
                 SET state = 'failed', failure_reason = $3, finished_at = statement_timestamp(), \
                     claim_expires_at = NULL, error_summary = $4, \
                     errors = (CASE WHEN jsonb_array_length(errors) >= $5 THEN errors - 0 ELSE errors END) \
                         || jsonb_build_object('attempt', attempts, 'at', statement_timestamp(), 'error', $4::text) \
                 WHERE id = $1 AND claim_generation = $2 AND claim_token = $6 AND claim_expires_at IS NOT NULL",
                id,
                generation,
                reason.as_str(),
                summary,
                i32::from(MAX_ATTEMPTS),
                token,
            ),
        ),
    };
    Ok(observed(statement_summary, query.execute(connection))
        .await?
        .rows_affected())
}

#[derive(Debug)]
pub(super) enum Ended {
    Success(crate::kind::Completion),
    Error(JobError),
    Panic,
    Payload(serde_json::Error),
    Timeout,
    Cancelled,
}

pub(super) fn map_outcome(
    attempt: &AttemptId,
    policy: Policy,
    is_outbox: bool,
    ended: Ended,
) -> Transition {
    let number = attempt.number;
    let exhausted = number >= policy.max_attempts;
    let retry_budget_spent = exhausted && !is_outbox;
    match ended {
        Ended::Success(_) => Transition::Complete,
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
                retry_budget_spent,
                &error.summary,
                RetryCause::Failure,
                floor_micros,
                is_outbox,
            ),
            Disposition::Retry => retry_or_exhaust(
                number,
                retry_budget_spent,
                &error.summary,
                RetryCause::Failure,
                0,
                is_outbox,
            ),
        },
        Ended::Panic => retry_or_exhaust(
            number,
            exhausted,
            "handler panicked",
            RetryCause::Failure,
            0,
            is_outbox,
        ),
        Ended::Payload(error) => retry_or_exhaust(
            number,
            exhausted,
            &payload_summary(attempt.kind, &error),
            RetryCause::Failure,
            0,
            is_outbox,
        ),
        Ended::Timeout => retry_or_exhaust(
            number,
            retry_budget_spent,
            &format!(
                "attempt timed out after {}",
                jiff::fmt::friendly::SpanPrinter::new()
                    .unsigned_duration_to_string(&policy.timeout)
            ),
            RetryCause::Timeout,
            0,
            is_outbox,
        ),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetryCause {
    Failure,
    Timeout,
}

fn retry_or_exhaust(
    number: u16,
    exhausted: bool,
    text: &str,
    cause: RetryCause,
    floor_micros: i64,
    is_outbox: bool,
) -> Transition {
    let summary = sanitize_summary(text);
    if exhausted {
        return Transition::Fail {
            reason: FailureReason::Exhausted,
            summary,
        };
    }
    let base_seconds = if is_outbox {
        2_i64
            .saturating_pow(u32::from(number.max(1)))
            .min(OUTBOX_MAX_RETRY_BASE_SECONDS)
    } else {
        i64::from(number).pow(4)
    };
    Transition::Retry {
        summary,
        base_micros: base_seconds * 1_000_000,
        floor_micros,
        cause,
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
        map_outcome(&attempt, Policy::default(), false, ended)
    }

    #[test]
    fn unhandled_driver_error_retries_with_only_its_category() {
        let error = sqlx::Error::Protocol("private driver detail".to_owned());
        std::assert_matches!(
            outcome(Ended::Error(error.into()), 1),
            Transition::Retry { ref summary, .. } if summary == "postgres protocol"
        );
    }

    #[test]
    fn dispositions_preserve_cap_and_snooze() {
        std::assert_matches!(
            outcome(Ended::Success(crate::kind::Completion::Pending), 25),
            Transition::Complete
        );
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
                cause: RetryCause::Failure,
                floor_micros: 2_000_001,
                ..
            } if base_micros == 1_000_000
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
                cause: RetryCause::Failure,
                floor_micros: 2_000_000,
                ..
            } if base_micros == 81_000_000
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
                cause: RetryCause::Failure,
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
    fn outbox_timeouts_remain_retryable_after_the_budget_and_invalid_work_exhausts() {
        let attempt = AttemptId {
            job_id: ID,
            kind: "sample",
            number: 25,
        };
        let outcome = |ended| map_outcome(&attempt, Policy::default(), true, ended);
        std::assert_matches!(
            outcome(Ended::Timeout),
            Transition::Retry {
                floor_micros: 0,
                cause: RetryCause::Timeout,
                ..
            }
        );
        let decode_error = serde_json::from_str::<u64>("null").unwrap_err();
        for ended in [Ended::Panic, Ended::Payload(decode_error)] {
            std::assert_matches!(
                outcome(ended),
                Transition::Fail {
                    reason: FailureReason::Exhausted,
                    ..
                }
            );
        }
    }

    #[test]
    fn a_completed_attempt_selects_no_single_row_write() {
        assert!(write_of(&Transition::Complete).is_none());
        std::assert_matches!(write_of(&Transition::Release), Some(Write::Release));
    }

    #[test]
    fn the_write_view_carries_each_statement_binding() {
        let retry = Transition::Retry {
            summary: "retry me".to_owned(),
            base_micros: 81_000_000,
            floor_micros: 2_000_000,
            cause: RetryCause::Failure,
        };
        std::assert_matches!(
            write_of(&retry),
            Some(Write::Retry {
                summary,
                minimum_micros: 72_900_000,
                jitter_micros: 16_200_000,
            }) if summary == "retry me"
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
    fn simultaneous_retry_after_responses_keep_a_spread_above_the_requested_floor() {
        let maximum = saturating_micros(MAX_DELAY);
        for (floor, width) in [(60_000_000, 6_000_000), (maximum - 1, 1), (maximum, 0)] {
            let error =
                JobError::retry_after_at_least("busy", Duration::from_micros(floor.unsigned_abs()))
                    .unwrap();
            let transition = outcome(Ended::Error(error), 1);
            std::assert_matches!(
                write_of(&transition),
                Some(Write::Retry { minimum_micros, jitter_micros, .. })
                    if minimum_micros == floor && jitter_micros == width
            );
        }
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
