use std::convert::Infallible;
use std::mem::DropGuard;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::http::{HeaderName, HeaderValue};
use axum::response::{IntoResponse, Response};
use enum_map::{Enum, EnumMap};
use infra_idempotency_store::{
    AttemptError, Attempted, RequestFingerprint, ScopeKey, Store, WriteOutcome,
};
use infra_postgres::{SqlError, failure_cause, sqlstate};
use tokio::time::Instant;

use super::stored::{ReplayError, ReplaySuccess, TerminalProblem, response_from_record};
use super::{REPLAYED_HEADER_NAME, REPLAYED_VALUE, Tx};
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

#[derive(Clone, Copy, Debug, Enum, PartialEq, Eq)]
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
    const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidKey => "invalid_key",
            Self::Unavailable => "unavailable",
            Self::InProgress => "in_progress",
            Self::KeyMismatch => "key_mismatch",
            Self::Replayed => "replayed",
            Self::Integrity => "integrity",
            Self::Internal => "internal",
            Self::Executed => "executed",
            Self::TerminalRefusal => "terminal_refusal",
            Self::Unstorable => "unstorable",
            Self::NotStored => "not_stored",
            Self::Abandoned => "abandoned",
        }
    }
}

pub(super) struct RouteState {
    pub(super) operation_id: Box<str>,
    pub(super) fingerprint_headers: Box<[HeaderName]>,
    pub(super) rules_link: HeaderValue,
    outcomes: EnumMap<Outcome, OnceLock<metrics::Counter>>,
}

impl RouteState {
    pub(super) fn new(
        operation_id: String,
        fingerprint_headers: Vec<HeaderName>,
        rules_link: HeaderValue,
    ) -> Self {
        Self {
            operation_id: operation_id.into_boxed_str(),
            fingerprint_headers: fingerprint_headers.into_boxed_slice(),
            rules_link,
            outcomes: EnumMap::default(),
        }
    }

    pub(super) fn record(&self, outcome: Outcome) {
        self.outcomes[outcome]
            .get_or_init(|| {
                metrics::counter!(HTTP_IDEMPOTENCY_OUTCOMES_METRIC, "outcome" => outcome.as_str())
            })
            .increment(1);
    }
}

pub(super) struct Attempt {
    pub(super) store: Store,
    pub(super) scope: ScopeKey,
    pub(super) fingerprint: RequestFingerprint,
    pub(super) route: Arc<RouteState>,
    pub(super) deadline: RequestDeadline,
}

pub trait FreshCommand: Send {
    type Success: ReplaySuccess;
    type Terminal: TerminalMode;

    fn execute(
        self,
        tx: &mut Tx<'_>,
    ) -> impl Future<Output = Result<Self::Success, Rollback<Self::Terminal>>> + Send;

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
            route,
            deadline,
        } = self;
        let guard = DropGuard::new(&route, |route| route.record(Outcome::Abandoned));
        let attempted = match deadline.attempt_end() {
            None => Err(AttemptError::Unavailable),
            Some(attempt_end) => {
                let command_cutoff = command_end.filter(|end| *end <= attempt_end);
                let attempt = store.attempt_with_deadline(
                    &scope,
                    &fingerprint,
                    command_cutoff.unwrap_or(attempt_end),
                    async |tx: &mut Tx<'_>| match command.execute(tx).await {
                        Ok(success) => Ok(WriteOutcome::Success(success.capture()?.into_success())),
                        Err(Rollback::Terminal(problem)) => {
                            Ok(WriteOutcome::TerminalRefusal(problem.capture_terminal()?))
                        }
                        Err(Rollback::Refused(problem)) => Err(AttemptRollback::Refused(problem)),
                        Err(Rollback::Database(error)) => Err(AttemptRollback::Database(error)),
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
        map_attempted(attempted, &scope, &route)
            .settle(deadline.at(), &route, guard)
            .await
    }
}

fn command_timeout() -> Response {
    Problem::new(Code::GatewayTimeout)
        .detail_static(ATTEMPT_TIMEOUT_DETAIL)
        .into_response()
}

mod sealed {
    pub trait TerminalMode {}
}

pub trait TerminalMode: sealed::TerminalMode + Send {
    type StoreMode: infra_idempotency_store::RefusalMode;

    fn capture_terminal(self) -> Result<Self::StoreMode, ReplayError>;
}

impl sealed::TerminalMode for Infallible {}
impl TerminalMode for Infallible {
    type StoreMode = Self;

    fn capture_terminal(self) -> Result<Self::StoreMode, ReplayError> {
        match self {}
    }
}

impl sealed::TerminalMode for TerminalProblem {}
impl TerminalMode for TerminalProblem {
    type StoreMode = infra_idempotency_store::TerminalRefusal;

    fn capture_terminal(self) -> Result<Self::StoreMode, ReplayError> {
        self.capture()
    }
}

const _: () = assert!(std::mem::size_of::<Rollback<TerminalProblem>>() <= 128);

#[derive(derive_more::Debug, derive_more::From, thiserror::Error)]
pub enum Rollback<T = Infallible> {
    #[debug("{self}")]
    #[error("operation refused")]
    Refused(Box<Problem>),
    #[debug("{self}")]
    #[error("terminal business refusal")]
    Terminal(T),
    #[from(SqlError)]
    #[debug("{self}")]
    #[error("database")]
    Database(SqlError),
}

impl<T> From<Problem> for Rollback<T> {
    fn from(problem: Problem) -> Self {
        Self::Refused(Box::new(problem))
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

    async fn settle<'a>(
        self,
        deadline: Instant,
        route: &RouteState,
        guard: DropGuard<&'a Arc<RouteState>, impl FnOnce(&'a Arc<RouteState>)>,
    ) -> Response {
        if self.yield_to_request_timeout && Instant::now() >= deadline {
            return std::future::pending().await;
        }
        DropGuard::dismiss(guard);
        route.record(self.outcome);
        self.response
    }
}

enum AttemptRollback {
    Refused(Box<Problem>),
    Database(SqlError),
    Unstorable(ReplayError),
}

impl From<ReplayError> for AttemptRollback {
    fn from(error: ReplayError) -> Self {
        Self::Unstorable(error)
    }
}

fn map_attempted(
    attempted: Result<Attempted<AttemptRollback>, AttemptError>,
    scope: &ScopeKey,
    route: &RouteState,
) -> Answer {
    let operation_id = &route.operation_id;
    match attempted {
        Err(error) => attempt_failure(error, scope, operation_id),
        Ok(Attempted::RolledBack(AttemptRollback::Refused(problem))) => {
            Answer::operation_response((*problem).into_response(), Outcome::NotStored)
        }
        Ok(Attempted::RolledBack(AttemptRollback::Database(error))) => {
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
        Ok(Attempted::RolledBack(AttemptRollback::Unstorable(error))) => {
            tracing::warn!(
                operation = operation_id,
                failure = %error,
                "http_idempotency_success_not_stored"
            );
            Answer::boundary_problem(sanitized_internal_error(), Outcome::Unstorable)
        }
        Ok(Attempted::Mismatch) => {
            Answer::boundary_problem(key_mismatch(route.rules_link.clone()), Outcome::KeyMismatch)
        }
        Ok(Attempted::Replay(record)) => {
            let mut response = response_from_record(record);
            response.headers_mut().insert(
                REPLAYED_HEADER_NAME,
                HeaderValue::from_static(REPLAYED_VALUE),
            );
            Answer::operation_response(response, Outcome::Replayed)
        }
        Ok(Attempted::InProgress) => {
            Answer::boundary_problem(in_progress(scope, operation_id), Outcome::InProgress)
        }
        Ok(Attempted::Committed(record)) => {
            Answer::operation_response(response_from_record(record), Outcome::Executed)
        }
        Ok(Attempted::TerminalRefusal(record)) => {
            Answer::operation_response(response_from_record(record), Outcome::TerminalRefusal)
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
                scope_digest = %const_hex::display(scope.digest()),
                operation = operation_id,
                failure = "store_undecodable",
                "http_idempotency_integrity_failed"
            );
            Answer::boundary_problem(sanitized_internal_error(), Outcome::Integrity)
        }
    }
}

fn unavailable() -> Response {
    Problem::new(Code::IdempotencyUnavailable)
        .detail_static(UNAVAILABLE_DETAIL)
        .retry_after(RETRY_AFTER_HINT)
        .into_response()
}

fn key_mismatch(rules_link: HeaderValue) -> Response {
    (
        [(axum::http::header::LINK, rules_link)],
        Problem::new(Code::IdempotencyKeyMismatch).detail_static(KEY_MISMATCH_DETAIL),
    )
        .into_response()
}

fn in_progress(scope: &ScopeKey, operation_id: &str) -> Response {
    tracing::info!(
        scope_digest = %const_hex::display(scope.digest()),
        operation = operation_id,
        "http_idempotency_in_progress"
    );
    Problem::new(Code::IdempotencyRequestInProgress)
        .detail_static(IN_PROGRESS_DETAIL)
        .retry_after(RETRY_AFTER_HINT)
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use axum::http::StatusCode;
    use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    use super::*;
    use crate::test_support::body_json;

    fn rules_link() -> HeaderValue {
        HeaderValue::from_static("<https://docs.example/retry>; rel=\"describedby\"")
    }

    fn route_state() -> RouteState {
        RouteState::new("test".to_owned(), Vec::new(), rules_link())
    }

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
        type Success = super::super::CreatedText;
        type Terminal = Infallible;

        async fn execute(self, _: &mut Tx<'_>) -> Result<Self::Success, Rollback> {
            panic!("exhausted requests must not execute work");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_exhausted_attempt_answers_unavailable_without_waiting_for_acquisition() {
        use futures_util::FutureExt as _;

        let pool = infra_postgres::test_support::connect_lazy("postgres://localhost/unused")
            .expect("lazy pool does no I/O");
        let budget = Duration::from_millis(200);
        for elapsed in [Duration::from_millis(100), Duration::from_millis(101)] {
            let origin = Instant::now()
                .checked_sub(elapsed)
                .expect("the deadline origin fits");
            let attempt = Attempt {
                store: Store::try_new(pool.clone(), Duration::from_mins(1))
                    .expect("retention fits PostgreSQL"),
                scope: scope(),
                fingerprint: RequestFingerprint::from_bytes([9; 32]),
                route: Arc::new(route_state()),
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
        let route = route_state();
        let answer = map_attempted(Err(AttemptError::Internal), &scope(), &route);
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

    #[tokio::test]
    async fn a_problem_refusal_answers_without_replay_header() {
        let route = route_state();
        let rollback = AttemptRollback::Refused(Box::new(
            Problem::new(Code::UnprocessableContent).detail("rejected"),
        ));
        let answer = map_attempted(Ok(Attempted::RolledBack(rollback)), &scope(), &route);
        assert_eq!(answer.outcome, Outcome::NotStored);
        assert_eq!(answer.response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            answer.response.headers()[CONTENT_TYPE],
            "application/problem+json"
        );
        assert!(
            !answer
                .response
                .headers()
                .contains_key(&REPLAYED_HEADER_NAME)
        );
        let problem = body_json(answer.response).await;
        assert_eq!(problem["code"], "unprocessable_content");
        assert_eq!(problem["detail"], "rejected");
    }

    #[test]
    fn rollback_debug_and_conversions_preserve_bounds_and_source_privacy() {
        struct OpaqueTerminal;

        let refused: Rollback<OpaqueTerminal> = Rollback::from(Problem::new(Code::BadRequest));
        assert_eq!(format!("{refused:?}"), "operation refused");
        assert!(refused.source().is_none());

        let terminal: Rollback<OpaqueTerminal> = Rollback::Terminal(OpaqueTerminal);
        assert_eq!(format!("{terminal:?}"), "terminal business refusal");
        assert!(terminal.source().is_none());

        let database: Rollback<OpaqueTerminal> = Rollback::from(SqlError::RowNotFound);
        assert_eq!(format!("{database:?}"), "database");
        assert!(database.source().is_none());
    }

    #[test]
    fn route_states_keep_scoped_outcome_recorders_isolated() {
        let first_recorder = DebuggingRecorder::new();
        let first_snapshot = first_recorder.snapshotter();
        let first_route = route_state();
        metrics::with_local_recorder(&first_recorder, || {
            first_route.record(Outcome::Executed);
            first_route.record(Outcome::Executed);
        });

        let second_recorder = DebuggingRecorder::new();
        let second_snapshot = second_recorder.snapshotter();
        let second_route = route_state();
        metrics::with_local_recorder(&second_recorder, || {
            second_route.record(Outcome::Executed);
        });

        let count = |snapshot: &metrics_util::debugging::Snapshotter| {
            snapshot
                .snapshot()
                .into_vec()
                .into_iter()
                .find_map(|(key, _, _, value)| {
                    (key.key().name() == HTTP_IDEMPOTENCY_OUTCOMES_METRIC).then_some(match value {
                        DebugValue::Counter(count) => count,
                        _ => 0,
                    })
                })
        };
        assert_eq!(count(&first_snapshot), Some(2));
        assert_eq!(count(&second_snapshot), Some(1));
    }
}
