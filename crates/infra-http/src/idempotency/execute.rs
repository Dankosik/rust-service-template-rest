//! The handler seam: the [`Idempotency`] extractor, [`Idempotency::execute`],
//! and the mapping of store outcomes to responses and the outcome counter.

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use infra_idempotency_store::{Attempted, ReadBack, ScopeKey, Store, Tx, WorkOutput};
use tokio::time::Instant;

use super::fingerprint::{Fingerprint, FingerprintError};
use super::stored::{self, Stored};
use crate::problem::{Code, Problem, SANITIZED_DETAIL};
use crate::request_id;

/// Idempotent request outcomes at the HTTP idempotency boundary, labelled
/// `outcome`.
pub const HTTP_IDEMPOTENCY_OUTCOMES_METRIC: &str = "http_idempotency_outcomes_total";

/// Time kept back from the request budget for answering after a readback;
/// authentication keeps the same response reserve.
const READBACK_RESERVE: Duration = Duration::from_millis(100);
/// The retry hint of the retryable idempotency Problems.
const RETRY_AFTER: Duration = Duration::from_secs(1);

const UNAVAILABLE_DETAIL: &str = "idempotent request processing is unavailable";
const KEY_MISMATCH_DETAIL: &str = "Idempotency-Key is bound to a different request";
const IN_PROGRESS_DETAIL: &str = "a request with this Idempotency-Key is in progress";
const OUTCOME_UNKNOWN_DETAIL: &str = "the outcome of this request is unknown";

/// The closed `outcome` label set of [`HTTP_IDEMPOTENCY_OUTCOMES_METRIC`].
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
    Reconciled,
    Unknown,
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
            Self::Reconciled => "reconciled",
            Self::Unknown => "outcome_unknown",
            Self::Abandoned => "abandoned",
        }
    }

    pub(super) fn record(self) {
        metrics::counter!(HTTP_IDEMPOTENCY_OUTCOMES_METRIC, "outcome" => self.label()).increment(1);
    }
}

/// What the key layer hands the [`Idempotency`] extractor for one request.
#[derive(Clone)]
pub(super) struct Attempt {
    pub(super) store: Store,
    pub(super) scope: ScopeKey,
    pub(super) operation: Arc<str>,
    pub(super) deadline: Instant,
    pub(super) request_id: Option<String>,
    /// Set by [`Idempotency::execute`]; the key layer refuses a success
    /// that never entered the seam.
    pub(super) seam_used: Arc<AtomicBool>,
}

/// One idempotent attempt: the extractor of a handler whose route was
/// composed by [`Composer::route`](super::Composer::route). Elsewhere it
/// answers a sanitized 500.
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
    /// Run `work` at most once for this caller, operation, and key.
    ///
    /// Call it after the operation has decoded, validated, and authorized the
    /// request: a replay never skips those checks. `fingerprint` is the
    /// operation's versioned semantic input; an `Err` is a code fault that
    /// answers a sanitized 500.
    ///
    /// `work` runs only when no live record for the key exists and no other
    /// attempt holds it, inside the one PostgreSQL transaction that also
    /// records its success. A 2xx response commits with that record and is
    /// answered in its replay form: the status, the body, and only the
    /// `Content-Type`, `Content-Encoding`, `Content-Language`,
    /// `Content-Disposition`, and `Location` headers. Any other response
    /// rolls back and is returned unchanged. A retry with the same key and
    /// input gets the recorded success without running `work`; a retry with
    /// different input gets 422 `idempotency_key_mismatch`.
    pub async fn execute<W, R>(
        self,
        fingerprint: Result<Fingerprint, FingerprintError>,
        work: W,
    ) -> Response
    where
        W: AsyncFnOnce(&mut Tx<'_>) -> R,
        R: IntoResponse,
    {
        let Self { attempt } = self;
        attempt.seam_used.store(true, Ordering::Relaxed);
        let Ok(fingerprint) = fingerprint else {
            tracing::error!(
                operation = %attempt.operation,
                failure = "fingerprint",
                "http_idempotency_fingerprint_failed"
            );
            return sanitized(attempt.request_id);
        };
        let outcome = OutcomeGuard::new();
        let current = fingerprint.current();
        let operation = &attempt.operation;
        let mut captured = None;
        let slot = &mut captured;
        let attempted = attempt
            .store
            .attempt(
                &attempt.scope,
                fingerprint.accepted(),
                async move |tx: &mut Tx<'_>| {
                    let response = work(tx).await.into_response();
                    if !response.status().is_success() {
                        return WorkOutput::Rollback(Rollback::Response(response));
                    }
                    let encoded = stored::capture(response)
                        .await
                        .and_then(|stored| stored.record(current).map(|record| (stored, record)));
                    match encoded {
                        Ok((stored, record)) => {
                            *slot = Some(stored);
                            WorkOutput::Commit(record)
                        }
                        Err(unstorable) => {
                            tracing::warn!(
                                operation = %operation,
                                failure = unstorable.class(),
                                "http_idempotency_success_not_stored"
                            );
                            WorkOutput::Rollback(Rollback::Unstorable)
                        }
                    }
                },
            )
            .await;
        let answer = match map_attempted(attempted, captured, attempt.request_id.clone()) {
            Mapped::Answer(answer) => answer,
            Mapped::ReadBack => map_read_back(
                tokio::time::timeout_at(
                    readback_deadline(attempt.deadline),
                    attempt
                        .store
                        .read_back(&attempt.scope, fingerprint.accepted()),
                )
                .await
                .ok(),
                attempt.request_id,
            ),
        };
        answer.send(attempt.deadline, outcome).await
    }
}

/// What the work hands back for rollback.
pub(super) enum Rollback {
    /// A non-2xx response, returned unchanged.
    Response(Response),
    /// A success that cannot be stored.
    Unstorable,
}

/// The mapping of one store result.
pub(super) enum Mapped {
    Answer(Answer),
    /// The commit outcome is unknown: read the record back.
    ReadBack,
}

/// A response with its outcome.
pub(super) struct Answer {
    response: Response,
    outcome: Outcome,
    /// An idempotency Problem, which yields to the chain's 504 once the
    /// request budget has expired. Successes, replays, and the work's own
    /// responses are returned as computed.
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

    /// Return the response and record its outcome. An idempotency Problem
    /// computed at or after the deadline instead waits for the chain's
    /// timer to answer 504 `request_timeout`, and the dropped guard records
    /// `abandoned`.
    async fn send(self, deadline: Instant, guard: OutcomeGuard) -> Response {
        if self.problem && Instant::now() >= deadline {
            return std::future::pending().await;
        }
        guard.record(self.outcome);
        self.response
    }
}

/// Map a store result to its response and outcome, or to a readback.
/// `captured` is the stored form of the success the work committed.
pub(super) fn map_attempted(
    attempted: Attempted<Rollback>,
    captured: Option<Stored>,
    request_id: Option<String>,
) -> Mapped {
    let answer = match attempted {
        Attempted::Unavailable
        | Attempted::WriteFailed
        | Attempted::CommitRejected { retryable: true } => {
            Answer::problem(unavailable(request_id), Outcome::Unavailable)
        }
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
        Attempted::InProgress => Answer::problem(in_progress(request_id), Outcome::InProgress),
        Attempted::RolledBack(Rollback::Response(response)) => {
            Answer::computed(response, Outcome::NotStored)
        }
        Attempted::RolledBack(Rollback::Unstorable)
        | Attempted::CommitRejected { retryable: false } => {
            Answer::problem(sanitized(request_id), Outcome::NotStored)
        }
        Attempted::Committed(_) => match captured {
            Some(stored) => Answer::computed(stored.into_response(), Outcome::Executed),
            None => integrity_failure(request_id),
        },
        Attempted::CommitUnknown => return Mapped::ReadBack,
    };
    Mapped::Answer(answer)
}

/// Map a readback after an unknown commit outcome; `None` is the readback
/// bound expiring.
pub(super) fn map_read_back(read_back: Option<ReadBack>, request_id: Option<String>) -> Answer {
    if let Some(ReadBack::Found {
        matched: true,
        record,
    }) = read_back
        && let Ok(stored) = stored::decode(record)
    {
        return Answer::computed(stored.into_response(), Outcome::Reconciled);
    }
    Answer::problem(outcome_unknown(request_id), Outcome::Unknown)
}

/// A live record that cannot be replayed, or a committed success without
/// its captured form: a sanitized 500 on every retry until the record
/// expires.
fn integrity_failure(request_id: Option<String>) -> Answer {
    tracing::error!(failure = "integrity", "http_idempotency_integrity_failed");
    Answer::problem(sanitized(request_id), Outcome::Integrity)
}

/// The readback bound: the request deadline less the response reserve.
fn readback_deadline(deadline: Instant) -> Instant {
    deadline.checked_sub(READBACK_RESERVE).unwrap_or(deadline)
}

/// Records exactly one outcome for an attempt with a valid fingerprint: the
/// mapped one when [`Idempotency::execute`] returns, or `abandoned` when its
/// future is dropped first (budget expiry, disconnect, or a panic in the
/// work).
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

/// The sanitized 500 of wiring faults, code faults, and integrity failures.
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

fn in_progress(request_id: Option<String>) -> Response {
    Problem::new(Code::IdempotencyRequestInProgress)
        .detail(IN_PROGRESS_DETAIL)
        .retry_after(RETRY_AFTER)
        .request_id(request_id)
        .into_response()
}

fn outcome_unknown(request_id: Option<String>) -> Response {
    Problem::new(Code::IdempotencyOutcomeUnknown)
        .detail(OUTCOME_UNKNOWN_DETAIL)
        .retry_after(RETRY_AFTER)
        .request_id(request_id)
        .into_response()
}

/// A thread-local count of [`HTTP_IDEMPOTENCY_OUTCOMES_METRIC`] by
/// `outcome`, installed with `metrics::set_default_local_recorder`.
#[cfg(test)]
pub(super) mod recorded {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use metrics::{
        Counter, CounterFn, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
    };

    use super::HTTP_IDEMPOTENCY_OUTCOMES_METRIC;

    type Counts = Arc<Mutex<BTreeMap<String, u64>>>;

    #[derive(Default)]
    pub(in crate::idempotency) struct Outcomes {
        counts: Counts,
    }

    impl Outcomes {
        pub(in crate::idempotency) fn counts(&self) -> BTreeMap<String, u64> {
            self.counts.lock().unwrap().clone()
        }
    }

    /// The expected counts, for comparison with [`Outcomes::counts`].
    pub(in crate::idempotency) fn counts(expected: &[(&str, u64)]) -> BTreeMap<String, u64> {
        expected
            .iter()
            .map(|(outcome, count)| ((*outcome).to_owned(), *count))
            .collect()
    }

    struct Cell {
        outcome: String,
        counts: Counts,
    }

    impl CounterFn for Cell {
        fn increment(&self, value: u64) {
            *self
                .counts
                .lock()
                .unwrap()
                .entry(self.outcome.clone())
                .or_default() += value;
        }

        fn absolute(&self, value: u64) {
            self.counts
                .lock()
                .unwrap()
                .insert(self.outcome.clone(), value);
        }
    }

    impl Recorder for Outcomes {
        fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
            if key.name() != HTTP_IDEMPOTENCY_OUTCOMES_METRIC {
                return Counter::noop();
            }
            let outcome = key
                .labels()
                .find(|label| label.key() == "outcome")
                .map(|label| label.value().to_owned())
                .unwrap_or_default();
            Counter::from_arc(Arc::new(Cell {
                outcome,
                counts: Arc::clone(&self.counts),
            }))
        }

        fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
            Gauge::noop()
        }

        fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
            Histogram::noop()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};
    use axum::http::{HeaderValue, Request, StatusCode};
    use http_body_util::BodyExt;
    use infra_idempotency_store::Record;

    use super::recorded::{Outcomes, counts};
    use super::*;

    const REQUEST_ID: &str = "req-1";
    const VERSION: NonZeroU32 = NonZeroU32::MIN;

    fn request_id() -> String {
        REQUEST_ID.to_owned()
    }

    /// A format-1 record: 201, `Content-Type: text/plain`, body `stored`.
    fn stored_record() -> Record {
        Record {
            fingerprint: [9; 32],
            format: 1,
            status: 201,
            headers: [&[1, 0, 10][..], b"text/plain"].concat(),
            body: b"stored".to_vec(),
        }
    }

    fn corrupt_record() -> Record {
        Record {
            format: 9,
            ..stored_record()
        }
    }

    fn new_attempt(store: Store, deadline: Instant) -> (Attempt, Arc<AtomicBool>) {
        let seam_used = Arc::new(AtomicBool::new(false));
        let attempt = Attempt {
            store,
            scope: ScopeKey::from_digest([1; 32]),
            operation: Arc::from("infraHttpTestIdempotent"),
            deadline,
            request_id: Some(request_id()),
            seam_used: Arc::clone(&seam_used),
        };
        (attempt, seam_used)
    }

    fn mapped(attempted: Attempted<Rollback>) -> Answer {
        match map_attempted(attempted, None, Some(request_id())) {
            Mapped::Answer(answer) => answer,
            Mapped::ReadBack => panic!("expected an answer"),
        }
    }

    async fn assert_problem(
        answer: Answer,
        outcome: Outcome,
        code: Code,
        detail: &str,
        retry_after: bool,
    ) {
        assert_eq!(answer.outcome, outcome);
        assert!(answer.problem, "{outcome:?} is an idempotency Problem");
        assert_problem_response(answer.response, code, detail, retry_after).await;
    }

    async fn assert_problem_response(
        response: Response,
        code: Code,
        detail: &str,
        retry_after: bool,
    ) {
        assert_eq!(response.status(), code.status());
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/problem+json"))
        );
        assert_eq!(
            response.headers().get(RETRY_AFTER),
            retry_after
                .then_some(HeaderValue::from_static("1"))
                .as_ref()
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], code.as_str());
        assert_eq!(json["detail"], detail);
        assert_eq!(json["request_id"], REQUEST_ID);
    }

    async fn assert_stored_success(answer: Answer, outcome: Outcome) {
        assert_eq!(answer.outcome, outcome);
        assert!(!answer.problem, "{outcome:?} is returned as computed");
        let response = answer.response;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("text/plain"))
        );
        assert_eq!(response.headers().len(), 1);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.as_ref(), b"stored");
    }

    #[tokio::test]
    async fn unavailable_writers_and_retryable_commit_rejections_answer_503() {
        for attempted in [
            Attempted::Unavailable,
            Attempted::WriteFailed,
            Attempted::CommitRejected { retryable: true },
        ] {
            assert_problem(
                mapped(attempted),
                Outcome::Unavailable,
                Code::IdempotencyUnavailable,
                "idempotent request processing is unavailable",
                true,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn a_live_record_decides_mismatch_replay_or_integrity() {
        assert_problem(
            mapped(Attempted::Live {
                matched: false,
                record: stored_record(),
            }),
            Outcome::KeyMismatch,
            Code::IdempotencyKeyMismatch,
            "Idempotency-Key is bound to a different request",
            false,
        )
        .await;
        assert_stored_success(
            mapped(Attempted::Live {
                matched: true,
                record: stored_record(),
            }),
            Outcome::Replayed,
        )
        .await;
        assert_problem(
            mapped(Attempted::Live {
                matched: true,
                record: corrupt_record(),
            }),
            Outcome::Integrity,
            Code::InternalServerError,
            SANITIZED_DETAIL,
            false,
        )
        .await;
        assert_problem(
            mapped(Attempted::InProgress),
            Outcome::InProgress,
            Code::IdempotencyRequestInProgress,
            "a request with this Idempotency-Key is in progress",
            true,
        )
        .await;
    }

    #[tokio::test]
    async fn rolled_back_and_rejected_work_is_not_stored() {
        let teapot = Response::builder()
            .status(StatusCode::IM_A_TEAPOT)
            .header("x-operation", "kept")
            .body(axum::body::Body::from("short and stout"))
            .unwrap();
        let returned = mapped(Attempted::RolledBack(Rollback::Response(teapot)));
        assert_eq!(returned.outcome, Outcome::NotStored);
        assert!(!returned.problem);
        assert_eq!(returned.response.status(), StatusCode::IM_A_TEAPOT);
        assert_eq!(returned.response.headers()["x-operation"], "kept");
        let body = returned.response.into_body().collect().await.unwrap();
        assert_eq!(body.to_bytes().as_ref(), b"short and stout");

        for attempted in [
            Attempted::RolledBack(Rollback::Unstorable),
            Attempted::CommitRejected { retryable: false },
        ] {
            assert_problem(
                mapped(attempted),
                Outcome::NotStored,
                Code::InternalServerError,
                SANITIZED_DETAIL,
                false,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn a_commit_answers_from_the_captured_form_or_reads_back() {
        let captured = stored::decode(stored_record()).unwrap();
        let Mapped::Answer(executed) = map_attempted(
            Attempted::Committed(corrupt_record()),
            Some(captured),
            Some(request_id()),
        ) else {
            panic!("expected an answer");
        };
        assert_stored_success(executed, Outcome::Executed).await;
        assert_problem(
            mapped(Attempted::Committed(stored_record())),
            Outcome::Integrity,
            Code::InternalServerError,
            SANITIZED_DETAIL,
            false,
        )
        .await;
        assert!(matches!(
            map_attempted(Attempted::CommitUnknown, None, Some(request_id())),
            Mapped::ReadBack
        ));
    }

    #[tokio::test]
    async fn only_a_matching_decodable_readback_reconciles() {
        assert_stored_success(
            map_read_back(
                Some(ReadBack::Found {
                    matched: true,
                    record: stored_record(),
                }),
                Some(request_id()),
            ),
            Outcome::Reconciled,
        )
        .await;
        for read_back in [
            Some(ReadBack::Found {
                matched: true,
                record: corrupt_record(),
            }),
            Some(ReadBack::Found {
                matched: false,
                record: stored_record(),
            }),
            Some(ReadBack::Absent),
            Some(ReadBack::NotWritable),
            Some(ReadBack::Failed),
            None,
        ] {
            assert_problem(
                map_read_back(read_back, Some(request_id())),
                Outcome::Unknown,
                Code::IdempotencyOutcomeUnknown,
                "the outcome of this request is unknown",
                true,
            )
            .await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_idempotency_problem_at_the_deadline_yields_to_the_chain_timeout() {
        let recorder = Outcomes::default();
        let _local = metrics::set_default_local_recorder(&recorder);
        let deadline = Instant::now() + Duration::from_millis(10);

        let early = mapped(Attempted::Unavailable)
            .send(deadline, OutcomeGuard::new())
            .await;
        assert_eq!(early.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(recorder.counts(), counts(&[("unavailable", 1)]));

        tokio::time::advance(Duration::from_millis(10)).await;
        let late = tokio::time::timeout(
            Duration::from_secs(1),
            mapped(Attempted::InProgress).send(deadline, OutcomeGuard::new()),
        )
        .await;
        assert!(late.is_err(), "the Problem must wait for the chain's 504");
        assert_eq!(
            recorder.counts(),
            counts(&[("unavailable", 1), ("abandoned", 1)])
        );

        let replay = mapped(Attempted::Live {
            matched: true,
            record: stored_record(),
        })
        .send(deadline, OutcomeGuard::new())
        .await;
        assert_eq!(replay.status(), StatusCode::CREATED);
        assert_eq!(
            recorder.counts(),
            counts(&[("unavailable", 1), ("abandoned", 1), ("replayed", 1)])
        );
    }

    #[test]
    fn the_readback_bound_keeps_the_response_reserve() {
        let deadline = Instant::now() + Duration::from_secs(5);
        assert_eq!(
            readback_deadline(deadline),
            deadline - Duration::from_millis(100)
        );
    }

    #[tokio::test]
    async fn an_unencodable_input_is_a_sanitized_500_without_an_outcome() {
        let recorder = Outcomes::default();
        let _local = metrics::set_default_local_recorder(&recorder);
        let ran = AtomicBool::new(false);
        let (attempt, seam_used) =
            new_attempt(Store::inert(), Instant::now() + Duration::from_secs(5));
        let response = Idempotency { attempt }
            .execute(Err(FingerprintError), async |_: &mut Tx<'_>| {
                ran.store(true, Ordering::SeqCst);
                StatusCode::CREATED
            })
            .await;
        assert!(!ran.load(Ordering::SeqCst));
        assert!(seam_used.load(Ordering::SeqCst));
        assert_problem_response(response, Code::InternalServerError, SANITIZED_DETAIL, false).await;
        assert!(recorder.counts().is_empty());
    }

    #[tokio::test]
    async fn an_inert_store_answers_unavailable_without_running_the_work() {
        let recorder = Outcomes::default();
        let _local = metrics::set_default_local_recorder(&recorder);
        let ran = AtomicBool::new(false);
        let (attempt, seam_used) =
            new_attempt(Store::inert(), Instant::now() + Duration::from_secs(5));
        let response = Idempotency { attempt }
            .execute(
                Fingerprint::new(VERSION, "input"),
                async |_: &mut Tx<'_>| {
                    ran.store(true, Ordering::SeqCst);
                    StatusCode::CREATED
                },
            )
            .await;
        assert!(!ran.load(Ordering::SeqCst));
        assert!(seam_used.load(Ordering::SeqCst));
        assert_problem_response(
            response,
            Code::IdempotencyUnavailable,
            "idempotent request processing is unavailable",
            true,
        )
        .await;
        assert_eq!(recorder.counts(), counts(&[("unavailable", 1)]));
    }

    #[tokio::test]
    async fn the_extractor_takes_the_attempt_or_answers_a_sanitized_500() {
        let (mut parts, ()) = Request::new(()).into_parts();
        let rejection = Idempotency::from_request_parts(&mut parts, &())
            .await
            .unwrap_err();
        assert_eq!(rejection.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let (attempt, _) = new_attempt(Store::inert(), Instant::now());
        parts.extensions.insert(attempt);
        let extracted = Idempotency::from_request_parts(&mut parts, &()).await;
        assert!(extracted.is_ok());
        assert!(parts.extensions.get::<Attempt>().is_none());
    }

    #[test]
    fn the_idempotency_codes_carry_their_catalog_metadata() {
        for (code, wire, status, title, section) in [
            (
                Code::IdempotencyRequestInProgress,
                "idempotency_request_in_progress",
                StatusCode::CONFLICT,
                "conflict",
                "rfc9110#section-15.5.10",
            ),
            (
                Code::IdempotencyKeyMismatch,
                "idempotency_key_mismatch",
                StatusCode::UNPROCESSABLE_ENTITY,
                "unprocessable content",
                "rfc9110#section-15.5.21",
            ),
            (
                Code::IdempotencyUnavailable,
                "idempotency_unavailable",
                StatusCode::SERVICE_UNAVAILABLE,
                "service unavailable",
                "rfc9110#section-15.6.4",
            ),
            (
                Code::IdempotencyOutcomeUnknown,
                "idempotency_outcome_unknown",
                StatusCode::SERVICE_UNAVAILABLE,
                "service unavailable",
                "rfc9110#section-15.6.4",
            ),
        ] {
            assert!(Code::ALL.contains(&code));
            assert_eq!(code.as_str(), wire);
            assert_eq!(code.status(), status);
            assert_eq!(code.title(), title);
            assert_eq!(
                code.type_uri(),
                format!("https://www.rfc-editor.org/rfc/{section}")
            );
        }
    }

    #[test]
    fn the_outcome_labels_are_the_closed_specified_set() {
        let labels: Vec<&str> = [
            Outcome::InvalidKey,
            Outcome::Unavailable,
            Outcome::InProgress,
            Outcome::KeyMismatch,
            Outcome::Replayed,
            Outcome::Integrity,
            Outcome::Executed,
            Outcome::NotStored,
            Outcome::Reconciled,
            Outcome::Unknown,
            Outcome::Abandoned,
        ]
        .into_iter()
        .map(Outcome::label)
        .collect();
        assert_eq!(
            labels,
            [
                "invalid_key",
                "unavailable",
                "in_progress",
                "key_mismatch",
                "replayed",
                "integrity",
                "executed",
                "not_stored",
                "reconciled",
                "outcome_unknown",
                "abandoned",
            ]
        );
    }
}
