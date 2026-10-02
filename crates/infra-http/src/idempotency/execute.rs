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
#[cfg(test)]
use crate::problem::http_status;
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
        let attempted = store
            .attempt(&scope, &caller, &fingerprint, async |tx: &mut Tx<'_>| {
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
            })
            .await;
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
    use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};
    use axum::http::{HeaderValue, StatusCode};
    use http_body_util::BodyExt;
    use infra_idempotency_store::{HeaderPair, Record};

    use super::*;

    fn scope() -> ScopeKey {
        ScopeKey::from_digest([1; 32])
    }

    fn stored_record() -> Record {
        Record {
            fingerprint: [9; 32],
            status: 201,
            headers: vec![HeaderPair {
                name: "content-type".to_owned(),
                value: b"text/plain".to_vec(),
            }],
            body: axum::body::Bytes::from_static(b"stored"),
        }
    }

    async fn assert_problem(answer: Answer, outcome: Outcome, code: Code, retry_after: bool) {
        assert_eq!(answer.outcome, outcome);
        assert!(answer.yield_to_request_timeout);
        assert_eq!(answer.response.status(), http_status(code));
        assert_eq!(
            answer.response.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/problem+json"))
        );
        assert_eq!(
            answer.response.headers().get(RETRY_AFTER),
            retry_after
                .then_some(HeaderValue::from_static("1"))
                .as_ref()
        );
        let body = answer
            .response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).expect("problem")["code"],
            code.as_str()
        );
    }

    #[tokio::test]
    async fn store_outcomes_keep_retry_and_integrity_semantics_distinct() {
        assert_problem(
            map_attempted(Err(AttemptError::Unavailable), &scope(), "test"),
            Outcome::Unavailable,
            Code::IdempotencyUnavailable,
            true,
        )
        .await;
        assert_problem(
            map_attempted(Err(AttemptError::Internal), &scope(), "test"),
            Outcome::Internal,
            Code::InternalServerError,
            false,
        )
        .await;
        assert_problem(
            map_attempted(
                Ok(Attempted::RolledBack(Rollback::Unstorable)),
                &scope(),
                "test",
            ),
            Outcome::Unstorable,
            Code::InternalServerError,
            false,
        )
        .await;
        assert_problem(
            map_attempted(Err(AttemptError::Integrity), &scope(), "test"),
            Outcome::Integrity,
            Code::InternalServerError,
            false,
        )
        .await;
        assert_problem(
            map_attempted(Ok(Attempted::InProgress), &scope(), "test"),
            Outcome::InProgress,
            Code::IdempotencyRequestInProgress,
            true,
        )
        .await;
    }

    #[tokio::test]
    async fn a_live_record_replays_or_refuses_a_different_request() {
        let replay = map_attempted(Ok(Attempted::Replay(stored_record())), &scope(), "test");
        assert_eq!(replay.outcome, Outcome::Replayed);
        assert_eq!(replay.response.status(), StatusCode::CREATED);
        assert_eq!(
            replay.response.extensions().get::<Provenance>(),
            Some(&Provenance::Replayed)
        );
        assert_eq!(
            replay
                .response
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes(),
            "stored"
        );
        assert_problem(
            map_attempted(Ok(Attempted::Mismatch), &scope(), "test"),
            Outcome::KeyMismatch,
            Code::IdempotencyKeyMismatch,
            false,
        )
        .await;
    }

    #[test]
    fn committed_success_carries_executed_provenance() {
        let answer = map_attempted(
            Ok(Attempted::Committed(
                stored::decode(stored_record()).expect("stored success"),
            )),
            &scope(),
            "test",
        );
        assert_eq!(answer.outcome, Outcome::Executed);
        assert_eq!(
            answer.response.extensions().get::<Provenance>(),
            Some(&Provenance::Executed)
        );
    }
}
