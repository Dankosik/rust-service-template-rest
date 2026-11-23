use infra_clickhouse::{
    AsyncInsertOptions, BatchInsertOptions, ClickHouse, InsertError, InsertFailure, InsertReport,
    QueryError, UnknownOutcome, WriteRow,
};

use crate::problem::{body_too_large, sanitized_problem};
use crate::{Code, Problem, RequestContext};

impl RequestContext {
    pub async fn clickhouse_insert_batch<'a, R: WriteRow>(
        &'a self,
        client: &'a ClickHouse,
        summary: &'static str,
        table: &'a str,
        rows: &'a [R],
        options: BatchInsertOptions<'a>,
    ) -> Result<InsertReport, InsertError> {
        let options = match self.clickhouse_insert_outcomes() {
            Some(outcomes) => options.observe_outcomes(outcomes),
            None => options,
        };
        client
            .insert_batch(self.operation(), summary, table, rows, options)
            .await
    }

    pub async fn clickhouse_insert_async<'a, R: WriteRow>(
        &'a self,
        client: &'a ClickHouse,
        summary: &'static str,
        table: &'a str,
        rows: &'a [R],
        options: AsyncInsertOptions<'a>,
    ) -> Result<InsertReport, InsertError> {
        let options = match self.clickhouse_insert_outcomes() {
            Some(outcomes) => options.observe_outcomes(outcomes),
            None => options,
        };
        client
            .insert_async(self.operation(), summary, table, rows, options)
            .await
    }
}

#[must_use]
pub fn query_problem(error: QueryError, not_found: impl FnOnce() -> Problem) -> Problem {
    match error {
        QueryError::NotFound => not_found(),
        QueryError::Busy | QueryError::Unavailable | QueryError::ResourceLimit => {
            Problem::new(Code::ServiceUnavailable)
        }
        QueryError::Stopped(stopped) => Problem::from(stopped),
        QueryError::TooManyRows
        | QueryError::TooLarge
        | QueryError::Rejected
        | QueryError::Decode => sanitized_problem(),
    }
}

#[must_use]
pub fn insert_problem(error: InsertError) -> Problem {
    match error {
        InsertError::NotApplied(reason) => match reason {
            InsertFailure::TooLarge => body_too_large(),
            InsertFailure::Busy | InsertFailure::Unavailable | InsertFailure::TooManyParts => {
                Problem::new(Code::ServiceUnavailable)
            }
            InsertFailure::Stopped(stopped) => Problem::from(stopped),
            InsertFailure::InvalidInput | InsertFailure::Encode | InsertFailure::Rejected => {
                sanitized_problem()
            }
        },
        InsertError::OutcomeUnknown(reason) => {
            let problem = match reason {
                UnknownOutcome::Stopped(stopped) => Problem::from(stopped),
                UnknownOutcome::Unavailable => Problem::new(Code::ServiceUnavailable),
            };
            with_unconfirmed_insert_detail(problem)
        }
    }
}

pub(super) fn with_unconfirmed_insert_detail(problem: Problem) -> Problem {
    problem.detail_static("the analytics database could not confirm whether the insert took effect")
}

#[cfg(test)]
mod tests;
