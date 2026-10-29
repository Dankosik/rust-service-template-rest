//! The handler seam, one store attempt, and its HTTP outcome mapping.

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use infra_idempotency_store::{AttemptError, Attempted, CallerIdentity, Digest, ScopeKey, Store};
use tokio::time::Instant;

use super::Tx;
use super::stored::{self, Stored};
use crate::harden::postgres_attempt_end;
use crate::problem::{Code, Problem, sanitized_internal_error};

/// Idempotent request outcomes at the HTTP idempotency boundary.
pub const HTTP_IDEMPOTENCY_OUTCOMES_METRIC: &str = "http_idempotency_outcomes_total";

const RETRY_AFTER: Duration = Duration::from_secs(1);
const UNAVAILABLE_DETAIL: &str = "idempotent request processing is unavailable";
const KEY_MISMATCH_DETAIL: &str = "Idempotency-Key is bound to a different request";
const IN_PROGRESS_DETAIL: &str = "a request with this Idempotency-Key is in progress";

/// The closed metric labels of HTTP idempotency.
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
    const fn label(self) -> &'static str {
        match self {
            Self::InvalidKey => "invalid_key",
            Self::Unavailable => "unavailable",
            Self::InProgress => "in_progress",
            Self::KeyMismatch => "key_mismatch",
            Self::Replayed => "replayed",
            Self::Integrity => "integrity",
            Self::Internal => "internal",
            Self::Executed => "executed",
            Self::Unstorable => "unstorable",
            Self::NotStored => "not_stored",
            Self::Abandoned => "abandoned",
        }
    }

    pub(super) fn record(self) {
        metrics::counter!(HTTP_IDEMPOTENCY_OUTCOMES_METRIC, "outcome" => self.label()).increment(1);
    }
}

/// The trusted capture the middleware hands to the handler extractor.
#[derive(Clone)]
pub(super) struct Attempt {
    pub(super) store: Store,
    pub(super) scope: ScopeKey,
    pub(super) caller: CallerIdentity,
    pub(super) fingerprint: Digest,
    pub(super) operation: Arc<str>,
    pub(super) deadline: Instant,
}

/// Private provenance sealed onto the successful response returned from the
/// idempotency seam. The outer route boundary turns it into wire metadata only
/// after the handler has returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Provenance {
    Executed,
    Replayed,
}

/// The extractor on an idempotent composed handler.
pub struct Idempotency {
    attempt: Attempt,
}

impl fmt::Debug for Idempotency {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Idempotency")
            .field("operation", &self.attempt.operation)
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
    /// Commit participating work at most once for this captured caller, key,
    /// and request during the configured retention period.
    ///
    /// Authorization and validation remain in the handler before this call,
    /// including on replay. The closure receives the provider-owned
    /// transaction only when no live record decided the attempt.
    ///
    /// Only a storable 2xx response permits committing work and its replay
    /// record together. A non-2xx work response rolls back and is returned
    /// unchanged; an unstorable success rolls back and becomes a sanitized
    /// failure, subject to the outer request timeout.
    ///
    /// Only effects issued through the supplied [`Tx`] share this guarantee.
    /// External effects are not covered, and work may run again on a retry
    /// after rollback. A timeout during COMMIT does not guarantee rollback.
    /// See the [idempotency guide](https://github.com/Dankosik/rust-service-template-rest/blob/main/docs/http-idempotency.md#transaction-outcomes-and-replay)
    /// for storage limits, transaction outcomes, and replay policy.
    pub async fn execute<W, R>(self, work: W) -> Response
    where
        W: AsyncFnOnce(&mut Tx<'_>) -> R,
        R: IntoResponse,
    {
        let Attempt {
            store,
            scope,
            caller,
            fingerprint,
            operation,
            deadline,
        } = self.attempt;
        let outcome = OutcomeGuard::new();
        let Some(attempt_end) = postgres_attempt_end(deadline) else {
            return map_attempted(Err(AttemptError::Unavailable), &scope, &operation)
                .send(deadline, outcome)
                .await;
        };
        let attempt = store.attempt_until(
            &scope,
            &caller,
            &fingerprint,
            attempt_end,
            async |tx: &mut Tx<'_>| {
                let response = work(tx).await.into_response();
                if !response.status().is_success() {
                    return Err(Rollback::Response(response));
                }
                stored::capture(response)
                    .await
                    .and_then(|stored| Ok((stored.record(fingerprint)?, stored)))
                    .map_err(|unstorable| {
                        tracing::warn!(
                            operation = %operation,
                            failure = unstorable.class(),
                            "http_idempotency_success_not_stored"
                        );
                        Rollback::Unstorable
                    })
            },
        );
        let attempted = tokio::time::timeout_at(attempt_end, attempt)
            .await
            .unwrap_or(Err(AttemptError::Unavailable));
        map_attempted(attempted, &scope, &operation)
            .send(deadline, outcome)
            .await
    }
}

/// What work hands back for rollback.
pub(super) enum Rollback {
    /// A non-2xx response returned unchanged.
    Response(Response),
    /// A 2xx response that cannot be durably replayed.
    Unstorable,
}

struct Answer {
    response: Response,
    outcome: Outcome,
    // Only Problems synthesized by this boundary yield to the outer timeout.
    // A handler's non-2xx response, even a Problem, follows the computed path.
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

    async fn send(self, deadline: Instant, guard: OutcomeGuard) -> Response {
        if self.yield_to_request_timeout && Instant::now() >= deadline {
            // Let the outer tower timeout produce the final 504 for an expired
            // boundary failure. Its cancellation drops the guard and records
            // abandonment; completed work and replay responses bypass this wait.
            return std::future::pending().await;
        }
        guard.record(self.outcome);
        self.response
    }
}

fn map_attempted(
    attempted: Result<Attempted<Stored, Rollback>, AttemptError>,
    scope: &ScopeKey,
    operation: &str,
) -> Answer {
    match attempted {
        Err(AttemptError::Unavailable) => {
            Answer::boundary_problem(unavailable(), Outcome::Unavailable)
        }
        Err(AttemptError::Internal) => {
            Answer::boundary_problem(sanitized_internal_error(), Outcome::Internal)
        }
        Ok(Attempted::RolledBack(Rollback::Unstorable)) => {
            Answer::boundary_problem(sanitized_internal_error(), Outcome::Unstorable)
        }
        Err(AttemptError::Integrity) => integrity_failure(),
        Ok(Attempted::Mismatch) => Answer::boundary_problem(key_mismatch(), Outcome::KeyMismatch),
        Ok(Attempted::Replay(record)) => match stored::decode(record) {
            Ok(stored) => Answer::operation_response(
                mark_provenance(stored.into_response(), Provenance::Replayed),
                Outcome::Replayed,
            ),
            Err(stored::Undecodable) => integrity_failure(),
        },
        Ok(Attempted::InProgress) => {
            Answer::boundary_problem(in_progress(scope, operation), Outcome::InProgress)
        }
        Ok(Attempted::RolledBack(Rollback::Response(response))) => {
            Answer::operation_response(response, Outcome::NotStored)
        }
        Ok(Attempted::Committed(stored)) => Answer::operation_response(
            mark_provenance(stored.into_response(), Provenance::Executed),
            Outcome::Executed,
        ),
    }
}

pub(super) fn mark_provenance(mut response: Response, provenance: Provenance) -> Response {
    response.extensions_mut().insert(provenance);
    response
}

fn integrity_failure() -> Answer {
    tracing::error!(failure = "integrity", "http_idempotency_integrity_failed");
    Answer::boundary_problem(sanitized_internal_error(), Outcome::Integrity)
}

fn unavailable() -> Response {
    Problem::new(Code::IdempotencyUnavailable)
        .detail(UNAVAILABLE_DETAIL)
        .retry_after(RETRY_AFTER)
        .into_response()
}

fn key_mismatch() -> Response {
    Problem::new(Code::IdempotencyKeyMismatch)
        .detail(KEY_MISMATCH_DETAIL)
        .into_response()
}

fn in_progress(scope: &ScopeKey, operation: &str) -> Response {
    // An expected answer to a concurrent retry; the request span carries the request id.
    tracing::info!(
        scope_digest = %ScopeDigest(scope.digest()),
        operation,
        "http_idempotency_in_progress"
    );
    Problem::new(Code::IdempotencyRequestInProgress)
        .detail(IN_PROGRESS_DETAIL)
        .retry_after(RETRY_AFTER)
        .into_response()
}

struct ScopeDigest<'a>(&'a Digest);

impl fmt::Display for ScopeDigest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Records exactly one outcome for every attempt that entered execute.
struct OutcomeGuard {
    recorded: bool,
}

impl OutcomeGuard {
    const fn new() -> Self {
        Self { recorded: false }
    }

    fn record(mut self, outcome: Outcome) {
        outcome.record();
        self.recorded = true;
    }
}

impl Drop for OutcomeGuard {
    fn drop(&mut self) {
        if !self.recorded {
            Outcome::Abandoned.record();
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};
    use http_body_util::BodyExt;

    use super::*;

    fn scope() -> ScopeKey {
        ScopeKey::from_digest([1; 32])
    }

    #[tokio::test(start_paused = true)]
    async fn an_exhausted_attempt_answers_unavailable_without_waiting_for_acquisition() {
        use futures_util::FutureExt as _;

        let pool = infra_postgres::PgPool::connect_lazy("postgres://localhost/unused")
            .expect("lazy pool does no I/O");
        for remaining in [Duration::from_millis(100), Duration::from_millis(99)] {
            let idempotency = Idempotency {
                attempt: Attempt {
                    store: Store::new(pool.clone(), Duration::from_secs(60)),
                    scope: scope(),
                    caller: CallerIdentity {
                        issuer: "issuer".into(),
                        kind: infra_idempotency_store::CallerKind::Subject,
                        value: "subject".into(),
                    },
                    fingerprint: [9; 32],
                    operation: "test".into(),
                    deadline: Instant::now() + remaining,
                },
            };
            let response = idempotency
                .execute(async |_: &mut Tx<'_>| -> StatusCode {
                    panic!("exhausted requests must not execute work");
                })
                .now_or_never()
                .expect("the reserve leaves no database attempt to poll");
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.headers()[RETRY_AFTER], "1");
            assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
            let body = response
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes();
            let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem");
            assert_eq!(problem["code"], "idempotency_unavailable");
        }
    }

    // The only store outcome the mounted PostgreSQL suite cannot provoke.
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
        let body = answer
            .response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem");
        assert_eq!(problem["code"], "internal_error");
    }
}
