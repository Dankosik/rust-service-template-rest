//! The handler seam, one store attempt, and its HTTP outcome mapping.

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use infra_idempotency_store::{Attempted, CallerIdentity, Digest, ScopeKey, Store, WorkOutput};
use tokio::time::Instant;

use super::Tx;
use super::stored::{self, Stored};
use crate::problem::{Code, Problem, SANITIZED_DETAIL};
use crate::request_id;

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
    Executed,
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
            Self::Executed => "executed",
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
    pub(super) request_id: Option<String>,
    pub(super) seam_used: Arc<AtomicBool>,
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
            sanitized(request_id::request_id(&parts.extensions))
        }))
    }
}

impl Idempotency {
    /// Run work at most once for this captured caller, key, and request.
    ///
    /// Authorization and validation remain in the handler before this call,
    /// including on replay. The closure receives the provider-owned
    /// transaction only when no live record decided the attempt.
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
            request_id,
            seam_used,
        } = self.attempt;
        seam_used.store(true, Ordering::Relaxed);
        let outcome = OutcomeGuard::new();
        let mut captured = None;
        let slot = &mut captured;
        let operation_for_work = Arc::clone(&operation);
        let attempted = store
            .attempt(
                &scope,
                &caller,
                &fingerprint,
                async move |tx: &mut Tx<'_>| {
                    let response = work(tx).await.into_response();
                    if !response.status().is_success() {
                        return WorkOutput::Rollback(Rollback::Response(response));
                    }
                    match stored::capture(response).await.and_then(|stored| {
                        stored.record(fingerprint).map(|record| (stored, record))
                    }) {
                        Ok((stored, record)) => {
                            *slot = Some(stored);
                            WorkOutput::Commit(record)
                        }
                        Err(unstorable) => {
                            tracing::warn!(
                                operation = %operation_for_work,
                                failure = unstorable.class(),
                                "http_idempotency_success_not_stored"
                            );
                            WorkOutput::Rollback(Rollback::Unstorable)
                        }
                    }
                },
            )
            .await;
        map_attempted(attempted, captured, request_id, &scope, &operation)
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
    problem: bool,
}

impl Answer {
    fn problem(response: Response, outcome: Outcome) -> Self {
        Self {
            response,
            outcome,
            problem: true,
        }
    }

    fn computed(response: Response, outcome: Outcome) -> Self {
        Self {
            response,
            outcome,
            problem: false,
        }
    }

    async fn send(self, deadline: Instant, guard: OutcomeGuard) -> Response {
        if self.problem && Instant::now() >= deadline {
            return std::future::pending().await;
        }
        guard.record(self.outcome);
        self.response
    }
}

fn map_attempted(
    attempted: Attempted<Rollback>,
    captured: Option<Stored>,
    request_id: Option<String>,
    scope: &ScopeKey,
    operation: &str,
) -> Answer {
    match attempted {
        Attempted::Unavailable => Answer::problem(unavailable(request_id), Outcome::Unavailable),
        Attempted::Internal => Answer::problem(sanitized(request_id), Outcome::NotStored),
        Attempted::Integrity => integrity_failure(request_id),
        Attempted::Live { matched: false, .. } => {
            Answer::problem(key_mismatch(request_id), Outcome::KeyMismatch)
        }
        Attempted::Live {
            matched: true,
            record,
        } => match stored::decode(record) {
            Ok(stored) => Answer::computed(stored.into_response(), Outcome::Replayed),
            Err(stored::Undecodable) => integrity_failure(request_id),
        },
        Attempted::InProgress => Answer::problem(
            in_progress(request_id, scope, operation),
            Outcome::InProgress,
        ),
        Attempted::RolledBack(Rollback::Response(response)) => {
            Answer::computed(response, Outcome::NotStored)
        }
        Attempted::RolledBack(Rollback::Unstorable) => {
            Answer::problem(sanitized(request_id), Outcome::NotStored)
        }
        Attempted::Committed(_) => match captured {
            Some(stored) => Answer::computed(stored.into_response(), Outcome::Executed),
            None => integrity_failure(request_id),
        },
    }
}

fn integrity_failure(request_id: Option<String>) -> Answer {
    tracing::error!(failure = "integrity", "http_idempotency_integrity_failed");
    Answer::problem(sanitized(request_id), Outcome::Integrity)
}

/// The sanitized 500 for wiring, persistence, and record faults.
pub(super) fn sanitized(request_id: Option<String>) -> Response {
    Problem::new(Code::InternalServerError)
        .detail(SANITIZED_DETAIL)
        .request_id(request_id)
        .into_response()
}

fn unavailable(request_id: Option<String>) -> Response {
    Problem::new(Code::IdempotencyUnavailable)
        .detail(UNAVAILABLE_DETAIL)
        .retry_after(RETRY_AFTER)
        .request_id(request_id)
        .into_response()
}

fn key_mismatch(request_id: Option<String>) -> Response {
    Problem::new(Code::IdempotencyKeyMismatch)
        .detail(KEY_MISMATCH_DETAIL)
        .request_id(request_id)
        .into_response()
}

fn in_progress(request_id: Option<String>, scope: &ScopeKey, operation: &str) -> Response {
    // An expected answer to a concurrent retry; the request span carries the request id.
    tracing::info!(
        scope_digest = %ScopeDigest(scope.digest()),
        operation,
        "http_idempotency_in_progress"
    );
    Problem::new(Code::IdempotencyRequestInProgress)
        .detail(IN_PROGRESS_DETAIL)
        .retry_after(RETRY_AFTER)
        .request_id(request_id)
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

    const REQUEST_ID: &str = "req-1";

    fn request_id() -> String {
        REQUEST_ID.to_owned()
    }

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
            body: b"stored".to_vec(),
        }
    }

    async fn assert_problem(answer: Answer, outcome: Outcome, code: Code, retry_after: bool) {
        assert_eq!(answer.outcome, outcome);
        assert!(answer.problem);
        assert_eq!(answer.response.status(), code.status());
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
            map_attempted(
                Attempted::Unavailable,
                None,
                Some(request_id()),
                &scope(),
                "test",
            ),
            Outcome::Unavailable,
            Code::IdempotencyUnavailable,
            true,
        )
        .await;
        assert_problem(
            map_attempted(
                Attempted::Internal,
                None,
                Some(request_id()),
                &scope(),
                "test",
            ),
            Outcome::NotStored,
            Code::InternalServerError,
            false,
        )
        .await;
        assert_problem(
            map_attempted(
                Attempted::Integrity,
                None,
                Some(request_id()),
                &scope(),
                "test",
            ),
            Outcome::Integrity,
            Code::InternalServerError,
            false,
        )
        .await;
        assert_problem(
            map_attempted(
                Attempted::InProgress,
                None,
                Some(request_id()),
                &scope(),
                "test",
            ),
            Outcome::InProgress,
            Code::IdempotencyRequestInProgress,
            true,
        )
        .await;
    }

    #[tokio::test]
    async fn live_record_replays_only_for_an_exact_fingerprint() {
        let replay = map_attempted(
            Attempted::Live {
                matched: true,
                record: stored_record(),
            },
            None,
            Some(request_id()),
            &scope(),
            "test",
        );
        assert_eq!(replay.outcome, Outcome::Replayed);
        assert_eq!(replay.response.status(), StatusCode::CREATED);
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
            map_attempted(
                Attempted::Live {
                    matched: false,
                    record: stored_record(),
                },
                None,
                Some(request_id()),
                &scope(),
                "test",
            ),
            Outcome::KeyMismatch,
            Code::IdempotencyKeyMismatch,
            false,
        )
        .await;
    }
}
