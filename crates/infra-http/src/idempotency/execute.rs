use std::fmt;
use std::mem::DropGuard;
use std::sync::Arc;
use std::time::Duration;

use axum::http::HeaderValue;
use axum::response::{IntoResponse, Response};
use infra_idempotency_store::{AttemptError, Attempted, Digest, ScopeKey, Store};
use infra_postgres::{SqlError, failure_cause, sqlstate};
use tokio::time::Instant;

use super::stored::ReplayableResponse;
use super::{REPLAYED_HEADER, REPLAYED_VALUE, Tx};
use crate::context::RequestDeadline;
use crate::problem::{Code, Problem, sanitized_internal_error};

pub const HTTP_IDEMPOTENCY_OUTCOMES_METRIC: &str = "http_idempotency_outcomes_total";

const RETRY_AFTER_HINT: Duration = Duration::from_secs(1);
const UNAVAILABLE_DETAIL: &str = "idempotent request processing is unavailable";
const KEY_MISMATCH_DETAIL: &str = "Idempotency-Key is bound to a different request";
const IN_PROGRESS_DETAIL: &str = "a request with this Idempotency-Key is in progress";
const ATTEMPT_TIMEOUT_DETAIL: &str = concat!(
    "the idempotent request outcome is unknown. Retry the same request with the same ",
    "Idempotency-Key within the published retry window. If the window has expired, ",
    "reconcile the outcome before attempting the operation again",
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    InvalidKey,
    Unavailable,
    InProgress,
    KeyMismatch,
    Replayed,
    Integrity,
    Internal,
    Executed,
    TerminalRefusal,
    Unstorable,
    NotStored,
    Abandoned,
}

impl Outcome {
    pub(super) fn record(self) {
        macro_rules! outcomes {
            ($($variant:ident => $label:literal),* $(,)?) => {
                match self {
                    $(Self::$variant => metrics::counter!(HTTP_IDEMPOTENCY_OUTCOMES_METRIC, "outcome" => $label),)*
                }
            };
        }
        let counter = outcomes!(
            InvalidKey => "invalid_key",
            Unavailable => "unavailable",
            InProgress => "in_progress",
            KeyMismatch => "key_mismatch",
            Replayed => "replayed",
            Integrity => "integrity",
            Internal => "internal",
            Executed => "executed",
            TerminalRefusal => "terminal_refusal",
            Unstorable => "unstorable",
            NotStored => "not_stored",
            Abandoned => "abandoned",
        );
        counter.increment(1);
    }
}

pub(super) struct Attempt {
    pub(super) store: Store,
    pub(super) scope: ScopeKey,
    pub(super) fingerprint: Digest,
    pub(super) operation_id: Arc<str>,
    pub(super) rules_link: HeaderValue,
    pub(super) deadline: RequestDeadline,
}

pub trait FreshCommand: Send {
    const ALLOW_TERMINAL_REFUSALS: bool = false;

    fn execute(
        self,
        tx: &mut Tx<'_>,
    ) -> impl Future<Output = Result<ReplayableResponse, Rollback>> + Send;

    fn attempt_timeout(&self) -> Option<Duration> {
        None
    }
}

impl Attempt {
    pub(super) async fn run(self, command: impl FreshCommand) -> Response {
        let command_end = command
            .attempt_timeout()
            .and_then(|budget| Instant::now().checked_add(budget));
        let execution = self.execute(command, command_end);
        match command_end {
            Some(end) => tokio::time::timeout_at(end, execution)
                .await
                .unwrap_or_else(|_| command_timeout()),
            None => execution.await,
        }
    }

    async fn execute<C: FreshCommand>(self, command: C, command_end: Option<Instant>) -> Response {
        let Attempt {
            store,
            scope,
            fingerprint,
            operation_id,
            rules_link,
            deadline,
        } = self;
        let guard = DropGuard::new((), |()| Outcome::Abandoned.record());
        let attempted = match deadline.attempt_end() {
            None => Err(AttemptError::Unavailable),
            Some(attempt_end) => {
                let store = store.with_terminal_refusals(C::ALLOW_TERMINAL_REFUSALS);
                let command_cutoff = command_end.filter(|end| *end <= attempt_end);
                let attempt = store.attempt_with_deadline(
                    &scope,
                    &fingerprint,
                    command_cutoff.unwrap_or(attempt_end),
                    async |tx: &mut Tx<'_>| match command.execute(tx).await {
                        Ok(response) => Ok(response.into_record()),
                        Err(Rollback::Terminal(problem)) => {
                            ReplayableResponse::terminal_problem(&problem)
                        }
                        Err(rollback) => Err(rollback),
                    },
                );
                let result = tokio::time::timeout_at(attempt_end, attempt)
                    .await
                    .unwrap_or(Err(AttemptError::Unavailable));
                if command_cutoff.is_some() && matches!(result, Err(AttemptError::Deadline)) {
                    return command_timeout();
                }
                result
            }
        };
        let mut answer = map_attempted(attempted, &scope, &operation_id);
        if answer.outcome == Outcome::KeyMismatch {
            answer
                .response
                .headers_mut()
                .insert(axum::http::header::LINK, rules_link);
        }
        answer.settle(deadline.at(), guard).await
    }
}

fn command_timeout() -> Response {
    Problem::new(Code::GatewayTimeout)
        .detail(ATTEMPT_TIMEOUT_DETAIL)
        .into_response()
}

#[derive(thiserror::Error)]
pub enum Rollback {
    #[error("operation refused")]
    Refused(Box<Response>),
    #[error("terminal business refusal")]
    Terminal(Box<Problem>),
    #[error("database")]
    Database(SqlError),
    #[error("status")]
    InvalidStatus,
    #[error("body_limit")]
    BodyLimit,
    #[error("body_forbidden")]
    BodyForbidden,
    #[error("content_encoding")]
    ContentEncoding,
    #[error("header_limit")]
    HeaderLimit,
    #[error("serialization")]
    Serialization,
}

impl fmt::Debug for Rollback {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl From<SqlError> for Rollback {
    fn from(error: SqlError) -> Self {
        Self::Database(error)
    }
}

impl Rollback {
    #[must_use]
    pub fn terminal(problem: Problem) -> Self {
        Self::Terminal(Box::new(problem))
    }

    #[must_use]
    pub fn refuse(response: impl IntoResponse) -> Self {
        Self::Refused(Box::new(response.into_response()))
    }
}

struct Answer {
    response: Response,
    outcome: Outcome,
    yield_to_request_timeout: bool,
}

impl Answer {
    fn boundary_problem(response: Response, outcome: Outcome) -> Self {
        Self {
            response,
            outcome,
            yield_to_request_timeout: true,
        }
    }

    fn operation_response(response: Response, outcome: Outcome) -> Self {
        Self {
            response,
            outcome,
            yield_to_request_timeout: false,
        }
    }

    async fn settle(self, deadline: Instant, guard: DropGuard<(), impl FnOnce(())>) -> Response {
        if self.yield_to_request_timeout && Instant::now() >= deadline {
            return std::future::pending().await;
        }
        DropGuard::dismiss(guard);
        self.outcome.record();
        self.response
    }
}

fn map_attempted(
    attempted: Result<Attempted<Rollback>, AttemptError>,
    scope: &ScopeKey,
    operation_id: &str,
) -> Answer {
    match attempted {
        Err(error) => attempt_failure(error, scope, operation_id),
        Ok(Attempted::RolledBack(Rollback::Refused(response))) => {
            Answer::operation_response(refusal(*response), Outcome::NotStored)
        }
        Ok(Attempted::RolledBack(Rollback::Database(error))) => {
            let class = AttemptError::classify_sql(&error);
            tracing::warn!(
                operation = operation_id,
                failure_class = %class,
                sqlstate = sqlstate(&error).as_deref(),
                cause = failure_cause(&error),
                "http_idempotency_command_failed"
            );
            attempt_failure(class, scope, operation_id)
        }
        Ok(Attempted::RolledBack(error)) => {
            tracing::warn!(
                operation = operation_id,
                failure = %error,
                "http_idempotency_success_not_stored"
            );
            Answer::boundary_problem(sanitized_internal_error(), Outcome::Unstorable)
        }
        Ok(Attempted::Mismatch) => Answer::boundary_problem(key_mismatch(), Outcome::KeyMismatch),
        Ok(Attempted::Replay(record)) => {
            let mut response = ReplayableResponse::from_record(record).into_response();
            response
                .headers_mut()
                .insert(REPLAYED_HEADER, HeaderValue::from_static(REPLAYED_VALUE));
            Answer::operation_response(response, Outcome::Replayed)
        }
        Ok(Attempted::InProgress) => {
            Answer::boundary_problem(in_progress(scope, operation_id), Outcome::InProgress)
        }
        Ok(Attempted::Committed(record)) => {
            let outcome = if record.is_terminal_refusal() {
                Outcome::TerminalRefusal
            } else {
                Outcome::Executed
            };
            Answer::operation_response(
                ReplayableResponse::from_record(record).into_response(),
                outcome,
            )
        }
    }
}

fn attempt_failure(error: AttemptError, scope: &ScopeKey, operation_id: &str) -> Answer {
    match error {
        AttemptError::Unavailable | AttemptError::Deadline => {
            Answer::boundary_problem(unavailable(), Outcome::Unavailable)
        }
        AttemptError::Internal => {
            Answer::boundary_problem(sanitized_internal_error(), Outcome::Internal)
        }
        AttemptError::Integrity => {
            tracing::error!(
                scope_digest = %ScopeDigest(scope.digest()),
                operation = operation_id,
                failure = "store_undecodable",
                "http_idempotency_integrity_failed"
            );
            Answer::boundary_problem(sanitized_internal_error(), Outcome::Integrity)
        }
    }
}

fn refusal(mut response: Response) -> Response {
    if !(response.status().is_client_error() || response.status().is_server_error()) {
        tracing::error!(
            failure = "invalid_refusal_status",
            "http_idempotency_wiring_failed"
        );
        return sanitized_internal_error();
    }
    response.headers_mut().remove(REPLAYED_HEADER);
    response
}

fn unavailable() -> Response {
    Problem::new(Code::IdempotencyUnavailable)
        .detail(UNAVAILABLE_DETAIL)
        .retry_after(RETRY_AFTER_HINT)
        .into_response()
}

fn key_mismatch() -> Response {
    Problem::new(Code::IdempotencyKeyMismatch)
        .detail(KEY_MISMATCH_DETAIL)
        .into_response()
}

fn in_progress(scope: &ScopeKey, operation_id: &str) -> Response {
    tracing::info!(
        scope_digest = %ScopeDigest(scope.digest()),
        operation = operation_id,
        "http_idempotency_in_progress"
    );
    Problem::new(Code::IdempotencyRequestInProgress)
        .detail(IN_PROGRESS_DETAIL)
        .retry_after(RETRY_AFTER_HINT)
        .into_response()
}

struct ScopeDigest<'a>(&'a Digest);

impl fmt::Display for ScopeDigest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(const_hex::Buffer::<32>::new().format(self.0))
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};

    use super::*;
    use crate::test_support::body_json;

    fn scope() -> ScopeKey {
        ScopeKey::new(
            infra_idempotency_store::CallerIdentity {
                issuer: "issuer".into(),
                kind: infra_idempotency_store::CallerKind::Subject,
                value: "subject".into(),
            },
            b"key",
        )
    }

    struct UnreachableCommand;

    impl FreshCommand for UnreachableCommand {
        async fn execute(self, _: &mut Tx<'_>) -> Result<ReplayableResponse, Rollback> {
            panic!("exhausted requests must not execute work");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_exhausted_attempt_answers_unavailable_without_waiting_for_acquisition() {
        use futures_util::FutureExt as _;

        let pool = infra_postgres::PgPool::connect_lazy("postgres://localhost/unused")
            .expect("lazy pool does no I/O");
        let budget = Duration::from_millis(200);
        for elapsed in [Duration::from_millis(100), Duration::from_millis(101)] {
            let origin = Instant::now()
                .checked_sub(elapsed)
                .expect("the deadline origin fits");
            let attempt = Attempt {
                store: Store::new(pool.clone(), Duration::from_mins(1))
                    .expect("retention fits PostgreSQL"),
                scope: scope(),
                fingerprint: [9; 32],
                operation_id: "test".into(),
                rules_link: HeaderValue::from_static(
                    "<https://docs.example/retry>; rel=\"describedby\"",
                ),
                deadline: RequestDeadline::from_deadline(
                    operation_context::Deadline::new(origin, budget),
                    budget,
                )
                .expect("the deadline fits"),
            };
            let response = attempt
                .run(UnreachableCommand)
                .now_or_never()
                .expect("the reserve leaves no database attempt to poll");
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.headers()[RETRY_AFTER], "1");
            assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
            let problem = body_json(response).await;
            assert_eq!(problem["code"], "idempotency_unavailable");
        }
    }

    #[tokio::test]
    async fn an_internal_store_failure_is_a_sanitized_500() {
        let answer = map_attempted(Err(AttemptError::Internal), &scope(), "test");
        assert_eq!(answer.outcome, Outcome::Internal);
        assert_eq!(answer.response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            answer.response.headers()[CONTENT_TYPE],
            "application/problem+json"
        );
        assert!(answer.response.headers().get(RETRY_AFTER).is_none());
        let problem = body_json(answer.response).await;
        assert_eq!(problem["code"], "internal_error");
    }

    #[test]
    fn a_rollback_cannot_claim_success_or_replay() {
        for (status, expected) in [
            (StatusCode::CONTINUE, StatusCode::INTERNAL_SERVER_ERROR),
            (StatusCode::CREATED, StatusCode::INTERNAL_SERVER_ERROR),
            (StatusCode::NOT_MODIFIED, StatusCode::INTERNAL_SERVER_ERROR),
            (StatusCode::BAD_REQUEST, StatusCode::BAD_REQUEST),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                StatusCode::from_u16(600).unwrap(),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ] {
            let rollback = Rollback::refuse((status, [(REPLAYED_HEADER, "true")]));
            let answer = map_attempted(Ok(Attempted::RolledBack(rollback)), &scope(), "test");
            assert_eq!(answer.response.status(), expected);
            assert!(!answer.response.headers().contains_key(REPLAYED_HEADER));
        }
    }
}
