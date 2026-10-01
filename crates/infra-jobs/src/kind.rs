//! Job kinds, their policies, and the erased handler a worker runs.

use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use infra_postgres::Tx;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::postgres::PgPool;
use tokio::time::Instant;

use crate::enqueue::{InvalidDelay, checked_delay_micros};
use tokio_util::sync::CancellationToken;

/// The longest kind name, in bytes.
pub(crate) const MAX_KIND_LEN: usize = 64;
/// The attempt budget a kind gets when its policy does not say otherwise.
pub(crate) const DEFAULT_MAX_ATTEMPTS: u16 = 25;
/// The largest attempt budget a policy may set.
pub(crate) const MAX_ATTEMPTS: u16 = 25;
/// The attempt timeout a kind gets when its policy does not say otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// The shortest attempt timeout a policy may set.
pub const MIN_TIMEOUT: Duration = Duration::from_secs(1);
/// The longest attempt timeout a policy may set.
pub(crate) const MAX_TIMEOUT: Duration = Duration::from_secs(3600);

/// A payload type a worker can run.
pub trait JobKind: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// 1 to 64 of `a-z`, `0-9`, `.`, `_`, `-`, starting with `a-z`.
    const NAME: &'static str;
}

/// A job's stable identifier, as enqueue generates it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub(crate) uuid::Uuid);

impl JobId {
    /// A time-ordered id: consecutive enqueues append to the primary key
    /// instead of writing random index pages.
    pub(crate) fn new_v7() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl fmt::Debug for JobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JobId(")?;
        fmt::Display::fmt(&self.0, formatter)?;
        formatter.write_str(")")
    }
}

/// Custody of one claimed attempt, assembled before the handler runs.
pub(crate) struct Attempt {
    pub(crate) id: JobId,
    /// Attempts used, including this one.
    pub(crate) number: u16,
    pub(crate) generation: i64,
    pub(crate) deadline: Instant,
    pub(crate) cancellation: CancellationToken,
    pub(crate) pool: PgPool,
}

/// One claimed attempt, as the handler receives it.
pub struct Job<K> {
    attempt: Attempt,
    payload: K,
}

impl<K: JobKind> Job<K> {
    /// The job's stable identifier.
    #[must_use]
    pub const fn id(&self) -> JobId {
        self.attempt.id
    }

    /// The kind name.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        K::NAME
    }

    /// Attempts used, including this one.
    #[must_use]
    pub const fn attempt(&self) -> u16 {
        self.attempt.number
    }

    /// The decoded payload.
    #[must_use]
    pub const fn payload(&self) -> &K {
        &self.payload
    }

    /// Fires at timeout or at the end of the drain.
    #[must_use]
    pub fn cancellation(&self) -> CancellationToken {
        self.attempt.cancellation.clone()
    }

    /// The supervisor's fixed deadline for this attempt.
    #[must_use]
    pub const fn deadline(&self) -> Instant {
        self.attempt.deadline
    }

    /// Complete this fenced claim on the caller's already-open transaction.
    ///
    /// Propagate this error out of the transaction closure so stale ownership
    /// rolls back earlier business writes. This method never commits or retries.
    ///
    /// # Errors
    ///
    /// [`CompleteError::StaleClaim`] when this running claim no longer exists,
    /// or [`CompleteError::Database`] when the statement fails.
    pub async fn complete_in_tx(&self, tx: &mut Tx<'_>) -> Result<(), CompleteError> {
        let affected = sqlx::query(crate::attempt::COMPLETE)
            .bind(self.attempt.id.0)
            .bind(self.attempt.generation)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if affected == 1 {
            Ok(())
        } else {
            Err(CompleteError::StaleClaim)
        }
    }

    /// The worker's pool.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.attempt.pool
    }
}

impl<K: JobKind> fmt::Debug for Job<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Job")
            .field("id", &self.attempt.id)
            .field("kind", &K::NAME)
            .field("attempt", &self.attempt.number)
            .finish_non_exhaustive()
    }
}

/// Why transactional completion could not establish current ownership.
#[derive(Debug, thiserror::Error)]
pub enum CompleteError {
    /// The running row no longer has this job's fencing generation.
    #[error("job claim is stale")]
    StaleClaim,
    /// The completion statement failed.
    #[error("complete job: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Disposition {
    Retry,
    Permanent,
    RetryAfterAtLeast(i64),
    Snooze(i64),
}

/// A handler disposition. Not a [`std::error::Error`]; [`Display`] is the summary.
#[derive(Debug)]
pub struct JobError {
    pub(crate) disposition: Disposition,
    pub(crate) summary: String,
}

impl JobError {
    /// A retryable failure. The summary is `error`'s [`Display`] text alone;
    /// `?` on an [`std::error::Error`] also keeps its sources.
    #[must_use]
    pub fn retryable(error: impl fmt::Display) -> Self {
        Self {
            disposition: Disposition::Retry,
            summary: error.to_string(),
        }
    }

    /// A permanent failure.
    #[must_use]
    pub fn permanent(error: impl fmt::Display) -> Self {
        Self {
            disposition: Disposition::Permanent,
            summary: error.to_string(),
        }
    }

    /// Retry no sooner than `delay`, spending this attempt.
    ///
    /// The queue keeps its jittered backoff and uses this delay as a floor.
    /// The floor never bypasses exhaustion.
    ///
    /// # Errors
    ///
    /// [`InvalidDelay`] if the floor exceeds [`crate::MAX_DELAY`].
    pub fn retry_after_at_least(
        error: impl fmt::Display,
        delay: Duration,
    ) -> Result<Self, InvalidDelay> {
        Ok(Self {
            disposition: Disposition::RetryAfterAtLeast(checked_delay_micros(delay)?),
            summary: error.to_string(),
        })
    }

    /// Schedule again without a failure, refunding this attempt exactly once.
    ///
    /// # Errors
    /// [`InvalidDelay`] if the delay exceeds [`crate::MAX_DELAY`].
    pub fn snooze(delay: Duration) -> Result<Self, InvalidDelay> {
        Ok(Self {
            disposition: Disposition::Snooze(checked_delay_micros(delay)?),
            summary: String::new(),
        })
    }

    /// Whether the worker must not retry this failure.
    #[must_use]
    pub const fn is_permanent(&self) -> bool {
        matches!(self.disposition, Disposition::Permanent)
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.summary)
    }
}

/// A retryable failure whose summary is `error`'s text and its sources.
impl<E> From<E> for JobError
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn from(error: E) -> Self {
        Self {
            disposition: Disposition::Retry,
            summary: with_sources(&error),
        }
    }
}

/// `error`'s text, then each source whose text the summary does not already
/// contain: an error that prints its cause (`"complete job: {0}"`) is not
/// repeated, and one that keeps its cause only in `source()` does not lose it.
fn with_sources(error: &(dyn std::error::Error + 'static)) -> String {
    let mut summary = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source
        && summary.len() < crate::attempt::ERROR_SUMMARY_MAX_BYTES
    {
        let text = cause.to_string();
        if !text.is_empty() && !summary.contains(&text) {
            summary.push_str(": ");
            summary.push_str(&text);
        }
        source = cause.source();
    }
    summary
}

/// A handler for one job kind.
pub trait Handler<K: JobKind>: Send + Sync + 'static {
    /// Run one attempt.
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

/// The claiming worker's attempt budget and timeout for one kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// `1..=MAX_ATTEMPTS`.
    pub max_attempts: u16,
    /// `MIN_TIMEOUT..=MAX_TIMEOUT`.
    pub timeout: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// Registered kinds, not yet checked.
#[derive(Default)]
pub struct Kinds {
    entries: Vec<Registered>,
}

impl Kinds {
    /// An empty, unchecked set.
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
    pub fn validate(self) -> Result<Registry, KindError> {
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
                return Err(KindError::InvalidPolicy {
                    kind: entry.name,
                    reason: "max_attempts must be between 1 and 25",
                });
            }
            if !(MIN_TIMEOUT..=MAX_TIMEOUT).contains(&entry.policy.timeout) {
                return Err(KindError::InvalidPolicy {
                    kind: entry.name,
                    reason: "timeout must be between 1s and 1h",
                });
            }
        }
        Ok(Registry {
            entries: self.entries,
        })
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

/// A checked kind set, in registration order.
pub struct Registry {
    entries: Vec<Registered>,
}

impl Registry {
    /// Kind names in registration order.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.entries.iter().map(|entry| entry.name)
    }

    pub(crate) fn get(&self, kind: &str) -> Option<&Registered> {
        self.entries.iter().find(|entry| entry.name == kind)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Registered> + '_ {
        self.entries.iter()
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Registry")
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

/// Why a kind set cannot start a worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KindError {
    /// Nothing was registered.
    #[error("no job kind is registered")]
    NoKinds,
    /// The same name was registered twice.
    #[error("job kind {0:?} is registered twice")]
    Duplicate(&'static str),
    /// The name is outside the kind-name grammar.
    #[error(
        "job kind name {0:?} must be 1 to 64 characters of a-z, 0-9, '.', '_', or '-', starting with a letter"
    )]
    InvalidName(&'static str),
    /// `max_attempts` or `timeout` is outside its range.
    #[error("job kind {kind:?}: {reason}")]
    InvalidPolicy {
        /// The kind that failed the check.
        kind: &'static str,
        /// `max_attempts must be between 1 and 25` or `timeout must be between 1s and 1h`.
        reason: &'static str,
    },
}

/// The kind-name grammar, the one check both [`Kinds::validate`] and enqueue call.
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
/// # Panics
/// Panics when `name` is outside the kind-name grammar.
#[allow(
    clippy::panic,
    reason = "an opt-in const assertion must reject invalid literals"
)]
pub const fn assert_valid_kind_name(name: &str) {
    assert!(is_valid_kind_name(name), "invalid job kind name");
}

pub(crate) struct Registered {
    pub(crate) name: &'static str,
    pub(crate) policy: Policy,
    pub(crate) dispatch: Box<dyn Dispatch>,
    /// Handles for every completed attempt, registered once the engine starts
    /// under the installed recorder.
    pub(crate) metrics: std::sync::OnceLock<crate::attempt::KindMetrics>,
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
        let err = Kinds::new().validate().unwrap_err();
        assert_eq!(err, KindError::NoKinds);
        assert_eq!(err.to_string(), "no job kind is registered");

        let mut kinds = Kinds::new();
        kinds
            .register(Policy::default(), accept)
            .register(Policy::default(), accept);
        let err = kinds.validate().unwrap_err();
        assert_eq!(err, KindError::Duplicate("sample"));
        assert_eq!(err.to_string(), "job kind \"sample\" is registered twice");

        let mut kinds = Kinds::new();
        kinds.register(Policy::default(), |_: Job<Bad>| async { Ok(()) });
        let err = kinds.validate().unwrap_err();
        assert_eq!(err, KindError::InvalidName("Bad"));
        assert_eq!(
            err.to_string(),
            "job kind name \"Bad\" must be 1 to 64 characters of a-z, 0-9, '.', '_', or '-', starting with a letter"
        );
    }

    #[test]
    fn validate_policy_bounds() {
        let attempts = "max_attempts must be between 1 and 25";
        let timeout = "timeout must be between 1s and 1h";
        assert_eq!(
            refuse(Policy {
                max_attempts: 0,
                timeout: DEFAULT_TIMEOUT,
            }),
            KindError::InvalidPolicy {
                kind: "sample",
                reason: attempts,
            }
        );
        assert_eq!(
            refuse(Policy {
                max_attempts: 26,
                timeout: DEFAULT_TIMEOUT,
            })
            .to_string(),
            "job kind \"sample\": max_attempts must be between 1 and 25"
        );
        assert_eq!(
            refuse(Policy {
                max_attempts: 1,
                timeout: Duration::from_millis(999),
            }),
            KindError::InvalidPolicy {
                kind: "sample",
                reason: timeout,
            }
        );
        assert_eq!(
            refuse(Policy {
                max_attempts: 1,
                timeout: MAX_TIMEOUT + Duration::from_nanos(1),
            })
            .to_string(),
            "job kind \"sample\": timeout must be between 1s and 1h"
        );
        admit(Policy {
            max_attempts: 1,
            timeout: MIN_TIMEOUT,
        });
        admit(Policy {
            max_attempts: MAX_ATTEMPTS,
            timeout: MAX_TIMEOUT,
        });
    }

    fn refuse(policy: Policy) -> KindError {
        let mut kinds = Kinds::new();
        kinds.register(policy, accept);
        kinds.validate().unwrap_err()
    }

    fn admit(policy: Policy) {
        let mut kinds = Kinds::new();
        kinds.register(policy, accept);
        assert!(kinds.validate().is_ok(), "{policy:?}");
    }

    #[test]
    fn registry_names_keep_registration_order() {
        let mut kinds = Kinds::new();
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
        assert_eq!(
            format!("{id:?}"),
            "JobId(01234567-89ab-cdef-fedc-ba9876543210)"
        );
    }

    #[tokio::test]
    async fn dispatch_prepare_runs_the_handler_and_rejects_a_bad_payload() {
        let id = job_id("01234567-89ab-cdef-fedc-ba9876543210");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut kinds = Kinds::new();
        kinds.register(Policy::default(), move |job: Job<Sample>| async move {
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
            assert!(job.cancellation().is_cancelled());
            Ok(())
        });
        let registry = kinds.validate().unwrap();
        let registered = registry.get(Sample::NAME).unwrap();
        let token = CancellationToken::new();
        token.cancel();
        let future = registered
            .dispatch
            .prepare(
                Attempt {
                    id,
                    number: 3,
                    generation: 99,
                    deadline,
                    cancellation: token,
                    pool: lazy_pool(),
                },
                br#"{"n":7,"secret":"payload-secret"}"#,
            )
            .unwrap();
        future.await.unwrap();

        let err = registered.dispatch.prepare(
            Attempt {
                id,
                number: 1,
                generation: 99,
                deadline: Instant::now(),
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
                id: job_id("01234567-89ab-cdef-fedc-ba9876543210"),
                number: 4,
                generation: 99,
                deadline: Instant::now(),
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
}
