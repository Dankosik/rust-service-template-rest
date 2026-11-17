use std::fmt;
use std::mem::DropGuard;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use infra_idempotency_store::{AttemptError, Attempted, CallerIdentity, Digest, ScopeKey, Store};
use tokio::time::Instant;

use super::Tx;
use super::stored::{self, ReplayableResponse};
use crate::context::RequestDeadline;
use crate::problem::{Code, Problem, sanitized_internal_error};

pub const HTTP_IDEMPOTENCY_OUTCOMES_METRIC: &str = "http_idempotency_outcomes_total";

const RETRY_AFTER_HINT: Duration = Duration::from_secs(1);
const UNAVAILABLE_DETAIL: &str = "idempotent request processing is unavailable";
const KEY_MISMATCH_DETAIL: &str = "Idempotency-Key is bound to a different request";
const IN_PROGRESS_DETAIL: &str = "a request with this Idempotency-Key is in progress";

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
            Unstorable => "unstorable",
            NotStored => "not_stored",
            Abandoned => "abandoned",
        );
        counter.increment(1);
    }
}

#[derive(Clone)]
pub(super) struct Attempt {
    pub(super) store: Store,
    pub(super) scope: ScopeKey,
    pub(super) caller: CallerIdentity,
    pub(super) fingerprint: Digest,
    pub(super) operation_id: Arc<str>,
    pub(super) deadline: RequestDeadline,
    pub(super) outcome: AttemptOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Provenance {
    Executed,
    Replayed,
}

pub struct Idempotency {
    attempt: Attempt,
}

impl fmt::Debug for Idempotency {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Idempotency")
            .field("operation_id", &self.attempt.operation_id)
            .finish_non_exhaustive()
    }
}

impl<S> FromRequestParts<S> for Idempotency
where
    S: Send + Sync,
{
    type Rejection = Response;

    fn from_request_parts(
        parts: &mut Parts,
        _: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let attempt = parts.extensions.remove::<Attempt>();
        std::future::ready(attempt.map(|attempt| Self { attempt }).ok_or_else(|| {
            tracing::error!(
                failure = "attempt_missing",
                "http_idempotency_wiring_failed"
            );
            sanitized_internal_error()
        }))
    }
}

impl Idempotency {
    pub async fn execute<W>(self, work: W) -> Response
    where
        W: AsyncFnOnce(&mut Tx<'_>) -> Result<ReplayableResponse, Rollback>,
    {
        let Attempt {
            store,
            scope,
            caller,
            fingerprint,
            operation_id,
            deadline,
            outcome,
        } = self.attempt;
        let guard = DropGuard::new(&outcome, |outcome| outcome.record(Outcome::Abandoned));
        let attempted = match deadline.attempt_end() {
            None => Err(AttemptError::Unavailable),
            Some(attempt_end) => {
                let attempt = store.attempt_with_acquire_deadline(
                    &scope,
                    &caller,
                    &fingerprint,
                    attempt_end,
                    async |tx: &mut Tx<'_>| {
                        let response = work(tx).await?;
                        Ok((response.to_record(), response))
                    },
                );
                tokio::time::timeout_at(attempt_end, attempt)
                    .await
                    .unwrap_or(Err(AttemptError::Unavailable))
            }
        };
        map_attempted(attempted, &scope, &operation_id)
            .settle(deadline.at(), guard)
            .await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Rollback {
    #[error("operation refused")]
    Refused(Box<Response>),
    #[error("status")]
    InvalidStatus,
    #[error("body_limit")]
    BodyLimit,
    #[error("header_limit")]
    HeaderLimit,
    #[error("serialization")]
    Serialization,
}

impl Rollback {
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

    async fn settle<'a>(
        self,
        deadline: Instant,
        guard: DropGuard<&'a AttemptOutcome, impl FnOnce(&'a AttemptOutcome)>,
    ) -> Response {
        if self.yield_to_request_timeout && Instant::now() >= deadline {
            return std::future::pending().await;
        }
        DropGuard::dismiss(guard).record(self.outcome);
        self.response
    }
}

fn map_attempted(
    attempted: Result<Attempted<ReplayableResponse, Rollback>, AttemptError>,
    scope: &ScopeKey,
    operation_id: &str,
) -> Answer {
    match attempted {
        Err(AttemptError::Unavailable) => {
            Answer::boundary_problem(unavailable(), Outcome::Unavailable)
        }
        Err(AttemptError::Internal) => {
            Answer::boundary_problem(sanitized_internal_error(), Outcome::Internal)
        }
        Ok(Attempted::RolledBack(Rollback::Refused(response))) => {
            Answer::operation_response(*response, Outcome::NotStored)
        }
        Ok(Attempted::RolledBack(error)) => {
            tracing::warn!(
                operation = operation_id,
                failure = %error,
                "http_idempotency_success_not_stored"
            );
            Answer::boundary_problem(sanitized_internal_error(), Outcome::Unstorable)
        }
        Err(AttemptError::Integrity) => {
            integrity_failure(scope, operation_id, "store_undecodable", None)
        }
        Ok(Attempted::Mismatch) => Answer::boundary_problem(key_mismatch(), Outcome::KeyMismatch),
        Ok(Attempted::Replay(record)) => match stored::decode(record) {
            Ok(replayable) => Answer::operation_response(
                mark_provenance(replayable.into_response(), Provenance::Replayed),
                Outcome::Replayed,
            ),
            Err(undecodable) => integrity_failure(
                scope,
                operation_id,
                "replay_undecodable",
                Some(undecodable.class()),
            ),
        },
        Ok(Attempted::InProgress) => {
            Answer::boundary_problem(in_progress(scope, operation_id), Outcome::InProgress)
        }
        Ok(Attempted::Committed(replayable)) => Answer::operation_response(
            mark_provenance(replayable.into_response(), Provenance::Executed),
            Outcome::Executed,
        ),
    }
}

pub(super) fn mark_provenance(mut response: Response, provenance: Provenance) -> Response {
    response.extensions_mut().insert(provenance);
    response
}

fn integrity_failure(
    scope: &ScopeKey,
    operation_id: &str,
    failure: &'static str,
    reason: Option<&'static str>,
) -> Answer {
    tracing::error!(
        scope_digest = %ScopeDigest(scope.digest()),
        operation = operation_id,
        failure,
        reason,
        "http_idempotency_integrity_failed"
    );
    Answer::boundary_problem(sanitized_internal_error(), Outcome::Integrity)
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

#[derive(Clone, Default)]
pub(super) struct AttemptOutcome(Arc<AtomicBool>);

impl AttemptOutcome {
    pub(super) fn record(&self, outcome: Outcome) {
        if !self.0.swap(true, Ordering::Relaxed) {
            outcome.record();
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};

    use super::*;
    use crate::test_support::body_json;

    fn scope() -> ScopeKey {
        ScopeKey::from_digest([1; 32])
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
            let idempotency = Idempotency {
                attempt: Attempt {
                    store: Store::new(pool.clone(), Duration::from_mins(1))
                        .expect("retention fits PostgreSQL"),
                    scope: scope(),
                    caller: CallerIdentity {
                        issuer: "issuer".into(),
                        kind: infra_idempotency_store::CallerKind::Subject,
                        value: "subject".into(),
                    },
                    fingerprint: [9; 32],
                    operation_id: "test".into(),
                    deadline: RequestDeadline::from_deadline(
                        operation_context::Deadline::new(origin, budget),
                        budget,
                    )
                    .expect("the deadline fits"),
                    outcome: AttemptOutcome::default(),
                },
            };
            let response = idempotency
                .execute(async |_: &mut Tx<'_>| {
                    panic!("exhausted requests must not execute work");
                })
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
}
