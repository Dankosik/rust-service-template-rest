use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
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
pub const MAX_ATTEMPTS: u16 = 25;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_mins(1);
pub(crate) const UNAVAILABLE_DELAY_MICROS: i64 = 10_000_000;
pub const MIN_TIMEOUT: Duration = Duration::from_secs(1);
pub const MAX_TIMEOUT: Duration = Duration::from_hours(1);

const MAX_DELAY_DAYS: u64 = 36_500;
pub const MAX_DELAY: Duration = Duration::from_hours(MAX_DELAY_DAYS * 24);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, thiserror::Error)]
#[error("delay exceeds the maximum of {MAX_DELAY_DAYS} days")]
pub struct InvalidDelay;

pub(crate) fn checked_delay_micros(duration: Duration) -> Result<i64, InvalidDelay> {
    if duration > MAX_DELAY {
        return Err(InvalidDelay);
    }
    Ok(saturating_micros(duration))
}

pub(crate) fn saturating_micros(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}

pub(crate) const MAX_ERROR_SUMMARY_BYTES: usize = 1024;

pub trait JobKind: Serialize + DeserializeOwned + Send + Sync + 'static {
    const NAME: &'static str;
}

#[derive(
    Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Debug, derive_more::Display,
)]
#[debug("JobId({_0})")]
pub struct JobId(pub(crate) uuid::Uuid);

impl JobId {
    pub(crate) fn new_v7() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

pub(crate) struct Attempt {
    pub(crate) fence: crate::attempt::ClaimFence,
    pub(crate) number: u16,
    #[expect(
        clippy::struct_field_names,
        reason = "the name contrasts with the supervisor deadline every caller compares it to"
    )]
    pub(crate) attempt_deadline: Instant,
    pub(crate) cancellation: CancellationToken,
}

pub struct Job<K, P = PgPool> {
    attempt: Attempt,
    payload: K,
    pool: P,
}

pub type TransactionalJob<K> = Job<K, ()>;

impl<K, P> Job<K, P> {
    #[must_use]
    pub const fn id(&self) -> JobId {
        self.attempt.fence.job_id
    }

    #[must_use]
    pub const fn attempt(&self) -> u16 {
        self.attempt.number
    }

    #[must_use]
    pub const fn payload(&self) -> &K {
        &self.payload
    }

    #[must_use]
    pub fn cancellation(&self) -> CancellationToken {
        self.attempt.cancellation.clone()
    }

    #[must_use]
    pub const fn deadline(&self) -> Instant {
        self.attempt.attempt_deadline
    }

    #[must_use]
    pub fn context(&self) -> OperationContext {
        OperationContext::new(
            Some(Deadline::at(self.attempt.attempt_deadline)),
            self.attempt.cancellation.child_token(),
        )
    }
}

impl<K> Job<K> {
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }
}

impl<K: JobKind, P> fmt::Debug for Job<K, P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Job")
            .field("id", &self.attempt.fence.job_id)
            .field("kind", &K::NAME)
            .field("attempt", &self.attempt.number)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Disposition {
    Retry,
    Permanent,
    RetryAfterAtLeast { floor_micros: i64 },
    Snooze { delay_micros: i64 },
}

#[derive(Debug, thiserror::Error)]
#[error("{summary}")]
pub struct JobError {
    pub(crate) disposition: Disposition,
    pub(crate) summary: Cow<'static, str>,
}

impl JobError {
    #[must_use]
    pub const fn retryable(reason: &'static str) -> Self {
        Self {
            disposition: Disposition::Retry,
            summary: Cow::Borrowed(reason),
        }
    }

    #[must_use]
    pub const fn permanent(reason: &'static str) -> Self {
        Self {
            disposition: Disposition::Permanent,
            summary: Cow::Borrowed(reason),
        }
    }

    pub fn retry_after_at_least(
        reason: &'static str,
        delay: Duration,
    ) -> Result<Self, InvalidDelay> {
        let whole_micros = checked_delay_micros(delay)?;
        let has_fraction = !delay.subsec_nanos().is_multiple_of(1_000);
        let floor_micros = whole_micros + i64::from(has_fraction);
        Ok(Self {
            disposition: Disposition::RetryAfterAtLeast { floor_micros },
            summary: Cow::Borrowed(reason),
        })
    }

    pub fn snooze(delay: Duration) -> Result<Self, InvalidDelay> {
        Ok(Self {
            disposition: Disposition::Snooze {
                delay_micros: checked_delay_micros(delay)?,
            },
            summary: Cow::Borrowed(""),
        })
    }

    #[must_use]
    pub const fn unavailable(reason: &'static str) -> Self {
        Self {
            disposition: Disposition::Snooze {
                delay_micros: UNAVAILABLE_DELAY_MICROS,
            },
            summary: Cow::Borrowed(reason),
        }
    }

    #[must_use]
    pub const fn is_permanent(&self) -> bool {
        matches!(self.disposition, Disposition::Permanent)
    }

    fn postgres(context: &'static str, error: &sqlx::Error) -> Self {
        let diagnostic = infra_postgres::sqlstate(error)
            .unwrap_or_else(|| Cow::Borrowed(infra_postgres::failure_cause(error)));
        Self {
            disposition: Disposition::Retry,
            summary: Cow::Owned(format!("{context} {diagnostic}")),
        }
    }
}

impl From<sqlx::Error> for JobError {
    fn from(error: sqlx::Error) -> Self {
        Self::postgres("postgres", &error)
    }
}

pub trait TransactionalHandler<K: JobKind>: Send + Sync + 'static {
    fn run(
        &self,
        job: TransactionalJob<K>,
        tx: &mut Tx<'_>,
    ) -> impl Future<Output = Result<(), JobError>> + Send;
}

struct TransactionFailure(JobError);

impl TransactionFailure {
    fn completion(error: &sqlx::Error) -> Self {
        Self(JobError::postgres("job completion failed: postgres", error))
    }
}

impl From<infra_postgres::TxError> for TransactionFailure {
    fn from(error: infra_postgres::TxError) -> Self {
        use infra_postgres::TxError;
        Self(match error {
            TxError::Acquire(sqlx::Error::PoolTimedOut) => {
                JobError::unavailable("postgres pool timed out")
            }
            TxError::Acquire(_) => JobError::retryable("postgres connection unavailable"),
            TxError::Begin(_) => JobError::retryable("postgres transaction did not begin"),
            TxError::CommitFailed(_) => JobError::retryable("postgres transaction not committed"),
            TxError::CommitUnknown(_) => JobError::retryable("postgres commit outcome unknown"),
        })
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub max_attempts: u16,
    pub timeout: Duration,
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

#[derive(Default)]
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

    pub fn register<K: JobKind>(&mut self, policy: Policy, handler: impl Handler<K>) -> &mut Self {
        let handler = Arc::new(handler);
        self.register_dispatch::<K>(policy, move |job| {
            let handler = Arc::clone(&handler);
            Box::pin(async move { handler.run(job).await.map(|()| Completion::Pending) })
        })
    }

    pub fn register_transactional<K: JobKind>(
        &mut self,
        policy: Policy,
        handler: impl TransactionalHandler<K>,
    ) -> &mut Self {
        let handler = Arc::new(handler);
        self.register_dispatch::<K>(policy, move |job| {
            Box::pin(run_transactional(Arc::clone(&handler), job))
        })
    }

    fn register_dispatch<K: JobKind>(
        &mut self,
        policy: Policy,
        run: impl Fn(Job<K>) -> HandlerFuture + Send + Sync + 'static,
    ) -> &mut Self {
        self.entries.push(Registered {
            name: K::NAME,
            policy,
            is_outbox: crate::RESERVED_OUTBOX_KIND == Some(K::NAME),
            running_slots: policy
                .max_running
                .map(|limit| Arc::new(crate::claim::admission_slots(limit))),
            dispatch: Box::new(move |attempt, pool, payload| {
                let payload = serde_json::from_slice::<K>(payload)?;
                Ok(run(Job {
                    attempt,
                    payload,
                    pool,
                }))
            }),
            metrics: std::sync::OnceLock::new(),
        });
        self
    }

    pub fn validate(self) -> Result<Kinds, KindError> {
        // template:begin outbox:jobs-outbox-registry-guard
        if self.entries.iter().any(|entry| entry.is_outbox) {
            return Err(KindError::OutboxKind);
        }
        // template:end outbox:jobs-outbox-registry-guard
        self.validate_kinds()
    }

    // template:begin outbox:jobs-outbox-registry
    pub(crate) fn validate_outbox(self) -> Result<Kinds, KindError> {
        match self.entries.as_slice() {
            [entry] if entry.is_outbox => self.validate_kinds(),
            _ => Err(KindError::OutboxKind),
        }
    }
    // template:end outbox:jobs-outbox-registry

    fn validate_kinds(self) -> Result<Kinds, KindError> {
        if self.entries.is_empty() {
            return Err(KindError::NoKinds);
        }
        let mut seen = HashSet::with_capacity(self.entries.len());
        for entry in &self.entries {
            if !is_valid_kind_name(entry.name) {
                return Err(KindError::InvalidName(entry.name));
            }
            if !seen.insert(entry.name) {
                return Err(KindError::Duplicate(entry.name));
            }
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

#[warn(clippy::map_err_ignore)]
async fn run_transactional<K: JobKind>(
    handler: Arc<impl TransactionalHandler<K>>,
    job: Job<K>,
) -> Result<Completion, JobError> {
    let Job {
        attempt,
        payload,
        pool,
    } = job;
    let job = Job {
        attempt,
        payload,
        pool: (),
    };
    let fence = job.attempt.fence;
    infra_postgres::in_tx_with_acquire_deadline(
        &pool,
        job.deadline(),
        infra_postgres::TxOptions::default(),
        async move |tx| {
            handler.run(job, tx).await.map_err(TransactionFailure)?;
            let complete = crate::attempt::complete_query(fence);
            let completed = observed!("complete job", complete.execute(&mut *tx))
                .await
                .map_err(|error| TransactionFailure::completion(&error))?;
            if completed.rows_affected() == 0 {
                return Err(TransactionFailure(JobError::retryable(
                    "job claim is stale",
                )));
            }
            Ok(Completion::Committed)
        },
    )
    .await
    .map_err(|failure| failure.0)
}

impl fmt::Debug for KindsBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = fmt::from_fn(|f| {
            f.debug_list()
                .entries(self.entries.iter().map(|entry| entry.name))
                .finish()
        });
        formatter
            .debug_struct("KindsBuilder")
            .field("names", &names)
            .finish_non_exhaustive()
    }
}

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
                &fmt::from_fn(|f| f.debug_list().entries(self.names()).finish()),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KindError {
    // template:begin outbox:jobs-outbox-registry-error
    #[error(
        "{} requires a dedicated single-kind outbox publisher registry",
        crate::outbox::KIND
    )]
    OutboxKind,
    // template:end outbox:jobs-outbox-registry-error
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
        jiff::fmt::friendly::SpanPrinter::new().unsigned_duration_to_string(&MIN_TIMEOUT),
        jiff::fmt::friendly::SpanPrinter::new().unsigned_duration_to_string(&MAX_TIMEOUT)
    )]
    InvalidTimeout { kind: &'static str },
}

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

/// ```compile_fail
/// const _: () = infra_jobs::assert_valid_kind_name("Not.Valid");
/// ```
pub const fn assert_valid_kind_name(name: &str) {
    assert!(is_valid_kind_name(name), "invalid job kind name");
}

pub(crate) struct Registered {
    pub(crate) name: &'static str,
    pub(crate) policy: Policy,
    pub(crate) is_outbox: bool,
    pub(crate) running_slots: Option<Arc<Semaphore>>,
    pub(crate) dispatch: Box<Dispatch>,
    metrics: std::sync::OnceLock<crate::attempt::KindMetrics>,
}

impl Registered {
    pub(crate) fn metrics(&self) -> &crate::attempt::KindMetrics {
        self.metrics
            .get_or_init(|| crate::attempt::KindMetrics::new(self.name))
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Completion {
    Pending,
    Committed,
}

pub(crate) type HandlerFuture = BoxFuture<'static, Result<Completion, JobError>>;

pub(crate) type Dispatch =
    dyn Fn(Attempt, PgPool, &[u8]) -> Result<HandlerFuture, serde_json::Error> + Send + Sync;

#[cfg(test)]
mod tests {

    use serde::{Deserialize, Serialize};
    use sqlx::postgres::PgPoolOptions;

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
            let documented = regex::regex!("^[a-z][a-z0-9._-]{0,63}$");
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
        let builder_output = [format!("{kinds:?}"), format!("{kinds:#?}")];
        let registry = kinds.validate().unwrap();
        assert_eq!(registry.names().collect::<Vec<_>>(), ["alpha", "beta"]);
        for output in builder_output
            .into_iter()
            .chain([format!("{registry:?}"), format!("{registry:#?}")])
        {
            let (_, after_alpha) = output
                .split_once("\"alpha\"")
                .expect("first registered kind");
            assert!(after_alpha.contains("\"beta\""), "{output}");
            assert!(!output.contains("policy"), "{output}");
            assert!(!output.contains("dispatch"), "{output}");
        }
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
        let future = (registered.dispatch)(
            Attempt {
                fence: crate::attempt::ClaimFence {
                    job_id: id,
                    generation: 99,
                    token: uuid::Uuid::nil(),
                },
                number: 3,
                attempt_deadline: deadline,
                cancellation: token,
            },
            lazy_pool(),
            br#"{"n":7,"secret":"payload-secret"}"#,
        )
        .unwrap();
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        future.await.unwrap();
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));

        let err = (registered.dispatch)(
            Attempt {
                fence: crate::attempt::ClaimFence {
                    job_id: id,
                    generation: 99,
                    token: uuid::Uuid::nil(),
                },
                number: 1,
                attempt_deadline: Instant::now(),
                cancellation: CancellationToken::new(),
            },
            lazy_pool(),
            b"null",
        );
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn job_debug_omits_payload() {
        let job = Job {
            attempt: Attempt {
                fence: crate::attempt::ClaimFence {
                    job_id: job_id("01234567-89ab-cdef-fedc-ba9876543210"),
                    generation: 99,
                    token: uuid::Uuid::nil(),
                },
                number: 4,
                attempt_deadline: Instant::now(),
                cancellation: CancellationToken::new(),
            },
            pool: lazy_pool(),
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
    fn transaction_diagnostics_preserve_the_stage_and_withhold_driver_text() {
        use infra_postgres::TxError;
        let TransactionFailure(completion) =
            TransactionFailure::completion(&sqlx::Error::Protocol("private-test-value".to_owned()));
        assert_eq!(completion.disposition, Disposition::Retry);
        assert_eq!(
            completion.to_string(),
            "job completion failed: postgres protocol"
        );
        let TransactionFailure(overload) = TxError::Acquire(sqlx::Error::PoolTimedOut).into();
        assert_eq!(
            overload.disposition,
            Disposition::Snooze {
                delay_micros: UNAVAILABLE_DELAY_MICROS
            }
        );
        let TransactionFailure(unknown) =
            TxError::CommitUnknown(sqlx::Error::Protocol("private-test-value".to_owned())).into();
        assert_eq!(unknown.disposition, Disposition::Retry);
        assert_eq!(unknown.to_string(), "postgres commit outcome unknown");
    }
}
