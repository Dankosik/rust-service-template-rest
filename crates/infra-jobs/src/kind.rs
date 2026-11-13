//! Job kinds, their policies, and the erased handler a worker runs.

use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use infra_postgres::{Tx, observed};
use operation_context::{Deadline, OperationContext};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::postgres::PgPool;
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_KIND_LEN: usize = 64;
pub(crate) const DEFAULT_MAX_ATTEMPTS: u16 = 25;
/// The largest attempt budget a [`Policy`] may declare.
pub const MAX_ATTEMPTS: u16 = 25;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Delay of [`JobError::unavailable`], in microseconds. A refunded attempt does
/// not count, so the delay cannot grow with attempts as the retry backoff
/// (`retry_or_exhaust` in `crate::attempt::outcome`) does; 10 s sits between
/// that backoff's first two steps, long enough for an overloaded dependency
/// to drain and short enough to notice its recovery.
pub(crate) const UNAVAILABLE_DELAY_MICROS: i64 = 10_000_000;
pub const MIN_TIMEOUT: Duration = Duration::from_secs(1);
/// The longest attempt timeout a [`Policy`] may declare.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(3600);

const MAX_DELAY_DAYS: u64 = 36_500;
/// The longest delay [`crate::EnqueueOptions::delay`] and
/// [`JobError::retry_after_at_least`] accept.
pub const MAX_DELAY: Duration = Duration::from_hours(MAX_DELAY_DAYS * 24);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, thiserror::Error)]
#[error("delay exceeds the maximum of {MAX_DELAY_DAYS} days")]
pub struct InvalidDelay;

/// Returns the exact PostgreSQL microsecond delay, dropping sub-microseconds.
///
/// The queue accepts zero through [`MAX_DELAY`] inclusive.
pub(crate) fn checked_delay_micros(duration: Duration) -> Result<i64, InvalidDelay> {
    if duration > MAX_DELAY {
        return Err(InvalidDelay);
    }
    i64::try_from(duration.as_micros()).map_err(|_| InvalidDelay)
}

/// Bound, in UTF-8 bytes, on a stored error summary.
pub(crate) const MAX_ERROR_SUMMARY_BYTES: usize = 1024;

/// A payload type a worker can run.
///
/// Deserialization must be bounded, nonblocking, and free of effects. The worker
/// catches preparation panics, but cannot preempt synchronous work.
pub trait JobKind: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// 1 to 64 of `a-z`, `0-9`, `.`, `_`, `-`, starting with `a-z`.
    const NAME: &'static str;
}

/// A job's stable identifier, as enqueue generates it.
#[derive(
    Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Debug, derive_more::Display,
)]
#[debug("JobId({_0})")]
pub struct JobId(pub(crate) uuid::Uuid);

impl JobId {
    /// A time-ordered id: consecutive enqueues append to the primary key
    /// instead of writing random index pages.
    pub(crate) fn new_v7() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

/// Custody of one claimed attempt, assembled before the handler runs.
pub(crate) struct Attempt {
    pub(crate) job_id: JobId,
    /// Attempts used, including this one.
    pub(crate) number: u16,
    pub(crate) generation: i64,
    /// This attempt's own deadline, never later than the supervisor's.
    #[expect(
        clippy::struct_field_names,
        reason = "the name contrasts with the supervisor deadline every caller compares it to"
    )]
    pub(crate) attempt_deadline: Instant,
    pub(crate) cancellation: CancellationToken,
    pub(crate) pool: PgPool,
}

/// One claimed attempt, as the handler receives it.
pub struct Job<K> {
    attempt: Attempt,
    payload: K,
}

impl<K: JobKind> Job<K> {
    #[must_use]
    pub const fn id(&self) -> JobId {
        self.attempt.job_id
    }

    /// Attempts used, including this one.
    #[must_use]
    pub const fn attempt(&self) -> u16 {
        self.attempt.number
    }

    #[must_use]
    pub const fn payload(&self) -> &K {
        &self.payload
    }

    /// Fires at timeout or at the end of the drain.
    #[must_use]
    pub fn cancellation(&self) -> CancellationToken {
        self.attempt.cancellation.clone()
    }

    /// This attempt's local deadline: its registered timeout, capped by the
    /// supervisor's lease deadline. After it passes, the supervisor signals
    /// cancellation before polling the handler again for cleanup. Synchronous
    /// handler work already in progress cannot be preempted.
    #[must_use]
    pub const fn deadline(&self) -> Instant {
        self.attempt.attempt_deadline
    }

    /// A child scope under this attempt's existing deadline and cancellation.
    /// Cancelling the returned context cannot cancel the attempt or sibling work.
    #[must_use]
    pub fn context(&self) -> OperationContext {
        OperationContext::new(
            Some(Deadline::at(self.attempt.attempt_deadline)),
            self.attempt.cancellation.child_token(),
        )
    }

    /// Complete this fenced claim on the caller's already-open transaction.
    ///
    /// Propagate this error out of the transaction closure so stale ownership
    /// rolls back earlier business writes. This method never commits or retries.
    /// It fences this job row; another enqueue of the same logical operation
    /// needs business effect identity of its own.
    ///
    /// The handler still returns `Ok(())` after the commit; the engine's own
    /// completion then finds the row completed and changes nothing. A commit
    /// whose outcome is unknown converts into a retryable [`JobError`]; the
    /// engine's fenced retry write changes nothing if the commit landed, and
    /// schedules another attempt if it did not.
    ///
    /// # Errors
    ///
    /// [`CompleteError::StaleClaim`] when this running claim no longer exists,
    /// or [`CompleteError::Database`] when the statement fails.
    pub async fn complete_in_tx(&self, tx: &mut Tx<'_>) -> Result<(), CompleteError> {
        let complete = crate::attempt::complete_query(crate::attempt::ClaimFence {
            job_id: self.attempt.job_id,
            generation: self.attempt.generation,
        });
        let affected = observed("complete job", complete.execute(&mut *tx))
            .await?
            .rows_affected();
        if affected == 1 {
            Ok(())
        } else {
            Err(CompleteError::StaleClaim)
        }
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.attempt.pool
    }
}

impl<K: JobKind> fmt::Debug for Job<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Job")
            .field("id", &self.attempt.job_id)
            .field("kind", &K::NAME)
            .field("attempt", &self.attempt.number)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CompleteError {
    #[error("job claim is stale")]
    StaleClaim,
    #[error("complete job")]
    Database(#[from] sqlx::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Disposition {
    Retry,
    Permanent,
    RetryAfterAtLeast { floor_micros: i64 },
    Snooze { delay_micros: i64 },
}

/// A handler disposition. [`Display`](std::fmt::Display) is the summary.
///
/// Not a [`std::error::Error`]: that impl would overlap the blanket
/// `From<E: Error>` below with the reflexive `From<T> for T` (E0119), and the
/// blanket impl is what lets `?` in a handler convert any error.
#[derive(Debug, derive_more::Display)]
#[display("{summary}")]
pub struct JobError {
    pub(crate) disposition: Disposition,
    pub(crate) summary: String,
}

impl JobError {
    /// A retryable failure. The summary is `error`'s [`Display`](std::fmt::Display) text alone;
    /// `?` on an [`std::error::Error`] also keeps its sources.
    #[must_use]
    pub fn retryable(error: impl fmt::Display) -> Self {
        Self {
            disposition: Disposition::Retry,
            summary: error.to_string(),
        }
    }

    #[must_use]
    pub fn permanent(error: impl fmt::Display) -> Self {
        Self {
            disposition: Disposition::Permanent,
            summary: error.to_string(),
        }
    }

    /// Retry no sooner than `delay`, spending this attempt.
    ///
    /// The queue keeps its jittered backoff and rounds this floor up to a
    /// whole microsecond for PostgreSQL. The floor never bypasses exhaustion.
    ///
    /// # Errors
    ///
    /// [`InvalidDelay`] if the floor exceeds [`crate::MAX_DELAY`].
    pub fn retry_after_at_least(
        error: impl fmt::Display,
        delay: Duration,
    ) -> Result<Self, InvalidDelay> {
        let whole_micros = checked_delay_micros(delay)?;
        let has_fraction = !delay.subsec_nanos().is_multiple_of(1_000);
        let floor_micros = whole_micros + i64::from(has_fraction);
        Ok(Self {
            disposition: Disposition::RetryAfterAtLeast { floor_micros },
            summary: error.to_string(),
        })
    }

    /// Schedule again without a failure, refunding this attempt exactly once.
    ///
    /// # Errors
    /// [`InvalidDelay`] if the delay exceeds [`crate::MAX_DELAY`].
    pub fn snooze(delay: Duration) -> Result<Self, InvalidDelay> {
        Ok(Self {
            disposition: Disposition::Snooze {
                delay_micros: checked_delay_micros(delay)?,
            },
            summary: String::new(),
        })
    }

    /// A dependency was overloaded or unavailable (a gRPC `RESOURCE_EXHAUSTED`
    /// or `UNAVAILABLE`, an HTTP `429` or `503`, an exhausted pool). Schedule
    /// again after a fixed delay without spending an attempt, so a dependency
    /// that is down for longer than the retry budget does not exhaust the job
    /// and stop its work. The summary is logged with the snooze.
    ///
    /// Use it for the dependency's own refusal, never for a failure of the job:
    /// a refunded attempt never exhausts, so a condition that does not clear
    /// shows only in `jobs_attempts_total{outcome="snoozed"}` and the queue's
    /// age. A pool acquire timeout found by [`shared_rules::error_chain::causes`]
    /// converts into this on its own.
    #[must_use]
    pub fn unavailable(error: impl fmt::Display) -> Self {
        Self {
            disposition: Disposition::Snooze {
                delay_micros: UNAVAILABLE_DELAY_MICROS,
            },
            summary: error.to_string(),
        }
    }

    /// Whether the worker must not retry this failure.
    #[must_use]
    pub const fn is_permanent(&self) -> bool {
        matches!(self.disposition, Disposition::Permanent)
    }
}

/// A retryable failure whose summary is `error`'s text and its bounded
/// [source chain](shared_rules::error_chain::sources).
///
/// Retryable, except when a cause within [`shared_rules::error_chain::causes`] is
/// [`sqlx::Error::PoolTimedOut`]: waiting for a connection that did not come is
/// the pool's capacity, not a fault of the job, so that conversion schedules a
/// snooze instead (see [`JobError::unavailable`]).
impl<E> From<E> for JobError
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn from(error: E) -> Self {
        let summary = with_sources(&error);
        if pool_timed_out(&error) {
            return Self::unavailable(summary);
        }
        Self {
            disposition: Disposition::Retry,
            summary,
        }
    }
}

/// Whether `error` or one of its sources is the pool's acquire timeout: a wait
/// for a connection that did not come is the pool's capacity, not a fault of
/// the job.
fn pool_timed_out(error: &(dyn std::error::Error + 'static)) -> bool {
    shared_rules::error_chain::causes(error).any(|error| {
        matches!(
            error.downcast_ref::<sqlx::Error>(),
            Some(sqlx::Error::PoolTimedOut)
        )
    })
}

/// `error`'s text, then each source whose text the summary does not already
/// contain: an error that prints its cause (`"complete job: {0}"`) is not
/// repeated, and one that keeps its cause only in `source()` does not lose it.
///
/// Once the summary reaches [`MAX_ERROR_SUMMARY_BYTES`], no further sources
/// are read. The final append can cross the limit, which the stored summary's
/// sanitizer enforces.
fn with_sources(error: &(dyn std::error::Error + 'static)) -> String {
    let mut summary = error.to_string();
    let mut sources = shared_rules::error_chain::sources(error);
    while summary.len() < MAX_ERROR_SUMMARY_BYTES
        && let Some(cause) = sources.next()
    {
        let text = cause.to_string();
        if !text.is_empty() && !summary.contains(&text) {
            summary.push_str(": ");
            summary.push_str(&text);
        }
    }
    summary
}

/// A handler for one job kind.
///
/// Keep work within the job's deadline and cancellation, and own and join child
/// work. Retries can overlap earlier blocking or remote work; external effects
/// need recipient-scoped idempotency and ambiguous-outcome reconciliation.
pub trait Handler<K: JobKind>: Send + Sync + 'static {
    fn run(&self, job: Job<K>) -> impl Future<Output = Result<(), JobError>> + Send;
}

impl<K, F, Fut> Handler<K> for F
where
    K: JobKind,
    F: Fn(Job<K>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), JobError>> + Send + 'static,
{
    fn run(&self, job: Job<K>) -> impl Future<Output = Result<(), JobError>> + Send {
        self(job)
    }
}

/// The claiming worker's attempt budget, timeout, and concurrency for one kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// From 1 through [`MAX_ATTEMPTS`].
    pub max_attempts: u16,
    /// From [`MIN_TIMEOUT`] through [`MAX_TIMEOUT`].
    pub timeout: Duration,
    /// The most attempts of this kind one engine runs at once. The engine
    /// claims no more than the free part, so the kind's due jobs wait in the
    /// queue and the engine's other slots stay free for its other kinds.
    /// `None` leaves the engine's slots as the only bound.
    pub max_running: Option<NonZeroU32>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            timeout: DEFAULT_TIMEOUT,
            max_running: None,
        }
    }
}

/// Registered kinds, not yet checked. [`Self::validate`] checks them into
/// [`Kinds`].
pub struct KindsBuilder {
    entries: Vec<Registered>,
}

impl KindsBuilder {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Record `handler` under `K::NAME` with `policy`. Checked by [`Self::validate`].
    pub fn register<K: JobKind>(&mut self, policy: Policy, handler: impl Handler<K>) -> &mut Self {
        self.entries.push(Registered {
            name: K::NAME,
            policy,
            running_slots: policy
                .max_running
                .map(|limit| Arc::new(crate::claim::admission_slots(limit))),
            dispatch: Box::new(Typed {
                handler: Arc::new(handler),
                _kind: PhantomData,
            }),
            metrics: std::sync::OnceLock::new(),
        });
        self
    }

    /// Check every registered kind. The first failure wins, in registration
    /// order: an invalid name, a duplicate name, then a policy outside its range.
    ///
    /// # Errors
    ///
    /// [`KindError`] when nothing is registered, a name is invalid or repeated,
    /// or a policy is outside its range.
    pub fn validate(self) -> Result<Kinds, KindError> {
        if self.entries.is_empty() {
            return Err(KindError::NoKinds);
        }
        let mut seen = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            if !is_valid_kind_name(entry.name) {
                return Err(KindError::InvalidName(entry.name));
            }
            if seen.contains(&entry.name) {
                return Err(KindError::Duplicate(entry.name));
            }
            seen.push(entry.name);
            if !(1..=MAX_ATTEMPTS).contains(&entry.policy.max_attempts) {
                return Err(KindError::InvalidMaxAttempts { kind: entry.name });
            }
            if !(MIN_TIMEOUT..=MAX_TIMEOUT).contains(&entry.policy.timeout) {
                return Err(KindError::InvalidTimeout { kind: entry.name });
            }
        }
        Ok(Kinds {
            entries: self.entries.into_iter().map(Arc::new).collect(),
        })
    }
}

impl Default for KindsBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for KindsBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KindsBuilder")
            .field(
                "names",
                &self
                    .entries
                    .iter()
                    .map(|entry| entry.name)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// Checked kinds, as an [`Engine`](crate::Engine) claims them.
pub struct Kinds {
    entries: Vec<Arc<Registered>>,
}

impl Kinds {
    pub fn names(&self) -> impl Iterator<Item = &'static str> {
        self.entries.iter().map(|entry| entry.name)
    }

    pub(crate) fn get(&self, kind: &str) -> Option<&Arc<Registered>> {
        self.entries.iter().find(|entry| entry.name == kind)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Registered> {
        self.entries.iter().map(Arc::as_ref)
    }
}

impl fmt::Debug for Kinds {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Kinds")
            .field(
                "names",
                &self
                    .entries
                    .iter()
                    .map(|entry| entry.name)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KindError {
    #[error("no job kind is registered")]
    NoKinds,
    #[error("job kind {0:?} is registered twice")]
    Duplicate(&'static str),
    #[error(
        "job kind name {0:?} must be 1 to {MAX_KIND_LEN} characters of a-z, 0-9, '.', '_', or '-', starting with a lowercase letter"
    )]
    InvalidName(&'static str),
    #[error("job kind {kind:?}: max_attempts must be between 1 and {MAX_ATTEMPTS}")]
    InvalidMaxAttempts { kind: &'static str },
    #[error(
        "job kind {kind:?}: timeout must be between {} and {}",
        humantime::format_duration(MIN_TIMEOUT),
        humantime::format_duration(MAX_TIMEOUT)
    )]
    InvalidTimeout { kind: &'static str },
}

/// The kind-name grammar, the one check both [`KindsBuilder::validate`] and
/// enqueue call.
pub(crate) const fn is_valid_kind_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_KIND_LEN || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    let mut index = 1;
    while index < bytes.len() {
        if !matches!(bytes[index], b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-') {
            return false;
        }
        index += 1;
    }
    true
}

/// Assert a statically declared kind name at compile time.
///
/// Use `const _: () = infra_jobs::assert_valid_kind_name(MyKind::NAME);`.
/// Runtime registration and enqueue still validate names independently.
///
/// A name outside the grammar fails the build:
///
/// ```compile_fail
/// const _: () = infra_jobs::assert_valid_kind_name("Not.Valid");
/// ```
///
/// # Panics
/// Panics when `name` is outside the kind-name grammar.
pub const fn assert_valid_kind_name(name: &str) {
    assert!(is_valid_kind_name(name), "invalid job kind name");
}

pub(crate) struct Registered {
    pub(crate) name: &'static str,
    pub(crate) policy: Policy,
    pub(crate) running_slots: Option<Arc<Semaphore>>,
    pub(crate) dispatch: Box<dyn Dispatch>,
    /// Handles for every completed attempt. The engine registers them at start,
    /// under the installed recorder; [`Self::metrics`] registers them on first
    /// use for a registry that never started.
    metrics: std::sync::OnceLock<crate::attempt::KindMetrics>,
}

impl Registered {
    pub(crate) fn metrics(&self) -> &crate::attempt::KindMetrics {
        self.metrics
            .get_or_init(|| crate::attempt::KindMetrics::new(self.name))
    }
}

pub(crate) type HandlerFuture =
    Pin<Box<dyn Future<Output = Result<(), JobError>> + Send + 'static>>;

pub(crate) trait Dispatch: Send + Sync {
    /// Decode `payload` and build the handler's future.
    fn prepare(&self, attempt: Attempt, payload: &[u8])
    -> Result<HandlerFuture, serde_json::Error>;
}

struct Typed<K, H> {
    handler: Arc<H>,
    _kind: PhantomData<fn() -> K>,
}

impl<K, H> Dispatch for Typed<K, H>
where
    K: JobKind,
    H: Handler<K>,
{
    fn prepare(
        &self,
        attempt: Attempt,
        payload: &[u8],
    ) -> Result<HandlerFuture, serde_json::Error> {
        let decoded = serde_json::from_slice::<K>(payload)?;
        let job = Job {
            attempt,
            payload: decoded,
        };
        let handler = Arc::clone(&self.handler);
        Ok(Box::pin(async move { handler.run(job).await }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde::{Deserialize, Serialize};
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        n: u32,
        secret: String,
    }

    impl JobKind for Sample {
        const NAME: &'static str = "sample";
    }

    const _: () = assert_valid_kind_name(Sample::NAME);

    async fn accept(_: Job<Sample>) -> Result<(), JobError> {
        Ok(())
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Alpha;

    impl JobKind for Alpha {
        const NAME: &'static str = "alpha";
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Beta;

    impl JobKind for Beta {
        const NAME: &'static str = "beta";
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Bad;

    impl JobKind for Bad {
        const NAME: &'static str = "Bad";
    }

    fn job_id(text: &str) -> JobId {
        JobId(uuid::Uuid::try_parse(text).unwrap())
    }

    fn lazy_pool() -> PgPool {
        PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap()
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(256))]

        #[test]
        fn kind_name_check_matches_the_documented_pattern(
            name in "[a-zA-Z0-9._ /é-]{0,70}|[a-z][a-z0-9._-]{0,63}",
        ) {
            let documented = regex::Regex::new(r"^[a-z][a-z0-9._-]{0,63}$").unwrap();
            proptest::prop_assert_eq!(is_valid_kind_name(&name), documented.is_match(&name));
        }
    }

    #[test]
    fn kind_names() {
        assert!(is_valid_kind_name("a"));
        assert!(is_valid_kind_name("a.b_c-9"));
        assert!(is_valid_kind_name(&"a".repeat(64)));
        assert!(!is_valid_kind_name(""));
        assert!(!is_valid_kind_name(&"a".repeat(65)));
        assert!(!is_valid_kind_name("A"));
        assert!(!is_valid_kind_name("1a"));
        assert!(!is_valid_kind_name(".a"));
        assert!(!is_valid_kind_name("-a"));
        assert!(!is_valid_kind_name("a b"));
        assert!(!is_valid_kind_name("a/b"));
        assert!(!is_valid_kind_name("é"));
    }

    #[test]
    fn validate_refuses_empty_duplicate_and_invalid_name() {
        let err = KindsBuilder::new().validate().unwrap_err();
        assert_eq!(err, KindError::NoKinds);
        assert_eq!(err.to_string(), "no job kind is registered");
        assert_eq!(
            KindsBuilder::default().validate().unwrap_err(),
            KindError::NoKinds,
            "default is the same empty builder"
        );

        let mut kinds = KindsBuilder::new();
        kinds
            .register(Policy::default(), accept)
            .register(Policy::default(), accept);
        let err = kinds.validate().unwrap_err();
        assert_eq!(err, KindError::Duplicate("sample"));
        assert_eq!(err.to_string(), "job kind \"sample\" is registered twice");

        let mut kinds = KindsBuilder::new();
        kinds.register(Policy::default(), |_: Job<Bad>| async { Ok(()) });
        let err = kinds.validate().unwrap_err();
        assert_eq!(err, KindError::InvalidName("Bad"));
        assert_eq!(
            err.to_string(),
            "job kind name \"Bad\" must be 1 to 64 characters of a-z, 0-9, '.', '_', or '-', starting with a lowercase letter"
        );
    }

    #[test]
    fn validate_policy_bounds() {
        assert_eq!(
            refuse(Policy {
                max_attempts: 0,
                timeout: DEFAULT_TIMEOUT,
                ..Policy::default()
            }),
            KindError::InvalidMaxAttempts { kind: "sample" }
        );
        assert_eq!(
            refuse(Policy {
                max_attempts: 26,
                timeout: DEFAULT_TIMEOUT,
                ..Policy::default()
            })
            .to_string(),
            "job kind \"sample\": max_attempts must be between 1 and 25"
        );
        assert_eq!(
            refuse(Policy {
                max_attempts: 1,
                timeout: Duration::from_millis(999),
                ..Policy::default()
            }),
            KindError::InvalidTimeout { kind: "sample" }
        );
        assert_eq!(
            refuse(Policy {
                max_attempts: 1,
                timeout: MAX_TIMEOUT + Duration::from_nanos(1),
                ..Policy::default()
            })
            .to_string(),
            "job kind \"sample\": timeout must be between 1s and 1h"
        );
        admit(Policy {
            max_attempts: 1,
            timeout: MIN_TIMEOUT,
            ..Policy::default()
        });
        admit(Policy {
            max_attempts: MAX_ATTEMPTS,
            timeout: MAX_TIMEOUT,
            ..Policy::default()
        });
    }

    fn refuse(policy: Policy) -> KindError {
        let mut kinds = KindsBuilder::new();
        kinds.register(policy, accept);
        kinds.validate().unwrap_err()
    }

    fn admit(policy: Policy) {
        let mut kinds = KindsBuilder::new();
        kinds.register(policy, accept);
        assert!(kinds.validate().is_ok(), "{policy:?}");
    }

    #[test]
    fn registry_names_keep_registration_order() {
        let mut kinds = KindsBuilder::new();
        kinds
            .register(Policy::default(), |_: Job<Alpha>| async { Ok(()) })
            .register(Policy::default(), |_: Job<Beta>| async { Ok(()) });
        let registry = kinds.validate().unwrap();
        assert_eq!(registry.names().collect::<Vec<_>>(), ["alpha", "beta"]);
    }

    #[test]
    fn job_id_uuid_display_preserves_external_format() {
        let text = "01234567-89ab-cdef-fedc-ba9876543210";
        let id = job_id(text);
        assert_eq!(id.to_string(), text);
    }

    #[tokio::test(start_paused = true)]
    async fn dispatch_prepare_runs_the_handler_and_rejects_a_bad_payload() {
        let id = job_id("01234567-89ab-cdef-fedc-ba9876543210");
        let deadline = Instant::now() + Duration::from_secs(10);
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut kinds = KindsBuilder::new();
        let handler_ran = Arc::clone(&ran);
        kinds.register(Policy::default(), move |job: Job<Sample>| {
            let handler_ran = Arc::clone(&handler_ran);
            async move {
                assert_eq!(job.id(), id);
                assert_eq!(job.attempt(), 3);
                assert_eq!(job.deadline(), deadline);
                assert_eq!(
                    job.payload(),
                    &Sample {
                        n: 7,
                        secret: "payload-secret".to_owned(),
                    }
                );
                let context = job.context();
                assert_eq!(
                    context.deadline().and_then(Deadline::instant),
                    Some(deadline)
                );
                tokio::time::advance(Duration::from_secs(2)).await;
                let sibling = job.context();
                assert_eq!(context.remaining(), Some(Duration::from_secs(8)));
                assert_eq!(sibling.remaining(), Some(Duration::from_secs(8)));
                context.cancel();
                assert!(!job.cancellation().is_cancelled());
                assert!(sibling.check().is_ok());
                job.cancellation().cancel();
                assert_eq!(sibling.check(), Err(operation_context::Stopped::Cancelled));
                tokio::time::advance(Duration::from_secs(8)).await;
                assert_eq!(job.context().remaining(), Some(Duration::ZERO));
                handler_ran.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        });
        let registry = kinds.validate().unwrap();
        let registered = registry.get(Sample::NAME).unwrap();
        let token = CancellationToken::new();
        let future = registered
            .dispatch
            .prepare(
                Attempt {
                    job_id: id,
                    number: 3,
                    generation: 99,
                    attempt_deadline: deadline,
                    cancellation: token,
                    pool: lazy_pool(),
                },
                br#"{"n":7,"secret":"payload-secret"}"#,
            )
            .unwrap();
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        future.await.unwrap();
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));

        let err = registered.dispatch.prepare(
            Attempt {
                job_id: id,
                number: 1,
                generation: 99,
                attempt_deadline: Instant::now(),
                cancellation: CancellationToken::new(),
                pool: lazy_pool(),
            },
            b"null",
        );
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn job_debug_omits_payload() {
        let job = Job {
            attempt: Attempt {
                job_id: job_id("01234567-89ab-cdef-fedc-ba9876543210"),
                number: 4,
                generation: 99,
                attempt_deadline: Instant::now(),
                cancellation: CancellationToken::new(),
                pool: lazy_pool(),
            },
            payload: Sample {
                n: 7,
                secret: "payload-secret".to_owned(),
            },
        };
        let text = format!("{job:?}");
        assert!(!text.contains("payload-secret"));
        assert!(!text.contains("payload"));
        assert!(text.contains("01234567-89ab-cdef-fedc-ba9876543210"));
        assert!(text.contains("sample"));
        assert!(text.contains('4'));
    }

    #[test]
    fn job_error_from_std_is_retryable_and_permanent_is_permanent() {
        let err = JobError::from(std::io::Error::other("disk failed"));
        assert!(!err.is_permanent());
        assert_eq!(err.to_string(), "disk failed");

        let err = JobError::permanent("stop");
        assert!(err.is_permanent());
        assert_eq!(err.to_string(), "stop");
    }

    #[test]
    fn cyclic_error_source_remains_a_retryable_failure() {
        #[derive(Debug, derive_more::Display)]
        #[display("cyclic error")]
        struct CyclicError(AtomicUsize);

        impl std::error::Error for CyclicError {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                let calls = self.0.fetch_add(1, Ordering::Relaxed);
                assert!(calls < 256, "walking the cycle did not stop");
                Some(self)
            }
        }

        let error = JobError::from(CyclicError(AtomicUsize::new(0)));
        assert_eq!(error.disposition, Disposition::Retry);
        assert_eq!(error.to_string(), "cyclic error");
    }

    #[test]
    fn a_pool_timeout_in_the_chain_is_unavailable_not_a_failure() {
        let direct = JobError::from(sqlx::Error::PoolTimedOut);
        assert_eq!(
            direct.disposition,
            Disposition::Snooze {
                delay_micros: UNAVAILABLE_DELAY_MICROS,
            }
        );
        let wrapped = JobError::from(infra_postgres::TxError::Acquire(sqlx::Error::PoolTimedOut));
        assert_eq!(
            wrapped.disposition,
            Disposition::Snooze {
                delay_micros: UNAVAILABLE_DELAY_MICROS,
            }
        );
        assert_eq!(
            wrapped.to_string(),
            "postgres transaction: acquire connection: pool timed out while waiting for an open connection"
        );
        let other = JobError::from(sqlx::Error::PoolClosed);
        assert_eq!(other.disposition, Disposition::Retry);
        let explicit = JobError::retryable(sqlx::Error::PoolTimedOut);
        assert_eq!(explicit.disposition, Disposition::Retry);
        let refusal = JobError::unavailable("rpc refused");
        assert_eq!(
            refusal.disposition,
            Disposition::Snooze {
                delay_micros: UNAVAILABLE_DELAY_MICROS,
            }
        );
        assert_eq!(refusal.to_string(), "rpc refused");
        assert!(Duration::from_micros(UNAVAILABLE_DELAY_MICROS.unsigned_abs()) <= crate::MAX_DELAY);
    }

    #[derive(Debug, thiserror::Error)]
    enum Layered {
        #[error("commit outcome unknown")]
        Hidden(#[source] std::io::Error),
        #[error("complete job: {0}")]
        Printed(#[source] std::io::Error),
    }

    #[test]
    fn job_error_from_std_keeps_each_source_once() {
        let hidden = JobError::from(Layered::Hidden(std::io::Error::other("connection reset")));
        assert_eq!(
            hidden.to_string(),
            "commit outcome unknown: connection reset"
        );

        let printed = JobError::from(Layered::Printed(std::io::Error::other("connection reset")));
        assert_eq!(printed.to_string(), "complete job: connection reset");

        let explicit = JobError::retryable(Layered::Hidden(std::io::Error::other("reset")));
        assert_eq!(explicit.to_string(), "commit outcome unknown");
    }

    #[test]
    fn complete_error_keeps_the_sqlx_cause_as_its_only_source() {
        let error = CompleteError::Database(sqlx::Error::PoolClosed);
        assert_eq!(error.to_string(), "complete job");
        let job_error = JobError::from(error);
        assert_eq!(
            job_error.to_string(),
            "complete job: attempted to acquire a connection on a closed pool"
        );
        assert_eq!(job_error.disposition, Disposition::Retry);
    }
}
