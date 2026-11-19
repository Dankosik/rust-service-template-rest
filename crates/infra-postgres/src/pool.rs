use std::num::NonZeroU32;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use crate::slots::{Queue, Slot, Slots};

use futures_util::future::BoxFuture;
use sqlx::pool::{PoolConnection, PoolConnectionMetadata};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool, PgPoolOptions};
use sqlx::{ConnectOptions, Connection, Postgres};
use tokio::time::{Instant, timeout_at};

use crate::dsn::Dsn;
use crate::observe::{PoolMetrics, observe_acquisition};
use crate::observed;
use crate::transaction::Isolation;

pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(3);

const SESSION_VERIFICATION_TIMEOUT: Duration = Duration::from_secs(5);

const SESSION_REJECTION_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) const STATEMENT_TIMEOUT: Duration = Duration::from_secs(8);

pub(crate) const IDLE_IN_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(8);

const SLOW_STATEMENT_THRESHOLD: Duration = Duration::from_secs(1);

const PING_IDLE_AFTER: Duration = Duration::from_secs(1);

pub(crate) const MAX_CONNECTION_LIFETIME: Duration = Duration::from_mins(30);

pub(crate) const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_mins(10);

const IDLE_PING_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("postgres connect: no connection established inside the {budget:?} acquire budget")]
    Timeout { budget: Duration },
    #[error("postgres session verification: did not complete inside the {budget:?} budget")]
    SessionVerificationTimeout { budget: Duration },
    #[error("postgres session admission: {0}")]
    Connect(#[source] sqlx::Error),
    #[error(
        "postgres session: {setting} is {}, expected a limit of at most {budget:?}; {}",
        format_timeout_ms(*.found_ms),
        .session_settings_source.remedy()
    )]
    SessionBudget {
        setting: &'static str,
        found_ms: i64,
        budget: Duration,
        session_settings_source: SessionSettingsSource,
    },
    #[error(
        "postgres session: default_transaction_isolation is {found}, expected {expected}; {}",
        .session_settings_source.remedy()
    )]
    SessionIsolation {
        found: String,
        expected: &'static str,
        session_settings_source: SessionSettingsSource,
    },
}

fn format_timeout_ms(milliseconds: i64) -> String {
    if milliseconds == 0 {
        "unlimited".to_owned()
    } else {
        format!("{milliseconds}ms")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionSettingsSource {
    #[default]
    Startup,
    Server,
}

impl SessionSettingsSource {
    const fn remedy(self) -> &'static str {
        match self {
            Self::Startup => "if a pooler is in use, verify that it applies the startup parameter",
            Self::Server => {
                "postgres.session_budgets = \"server\" needs it set on the role or database"
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct PoolOptions<'a> {
    pub max_connections: NonZeroU32,
    pub application_name: &'a str,
    pub default_isolation: Isolation,
    pub session_settings_source: SessionSettingsSource,
    pub wait_order: WaitOrder,
    pub notifications: bool,
}

/// Who gets a freed connection when more callers wait than the pool holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOrder {
    /// The longest waiter. For callers that wait as long as it takes, such as
    /// a job writing its outcome: a caller that has waited is still there.
    Arrival,
    /// The newest waiter once the queue has stood for a moment. For callers
    /// that give up, such as a request with a client timeout: in a standing
    /// queue the longest waiter is the one most likely to be gone, and
    /// serving in arrival order answers every caller just after it left.
    NewestOnceStanding,
}

#[derive(Clone, Debug)]
pub struct SessionOptions<'a> {
    pub application_name: &'a str,
    pub statement_timeout: Duration,
    pub idle_in_transaction_timeout: Duration,
    pub lock_timeout: Duration,
    pub extra: &'a [(&'a str, &'a str)],
}

pub(crate) struct PoolPolicy {
    slots: Option<Arc<Slots>>,
    notifications: bool,
    metrics: PoolMetrics,
    default_isolation: Isolation,
    session_settings_source: SessionSettingsSource,
}

impl Default for PoolPolicy {
    fn default() -> Self {
        Self {
            slots: None,
            notifications: true,
            metrics: PoolMetrics::default(),
            default_isolation: Isolation::ServerDefault,
            session_settings_source: SessionSettingsSource::Startup,
        }
    }
}

impl PoolPolicy {
    pub(crate) fn metrics(&self) -> &PoolMetrics {
        &self.metrics
    }
}

#[derive(Clone)]
pub struct Pool {
    inner: PgPool,
    policy: Arc<PoolPolicy>,
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool")
            .field("inner", &self.inner)
            .field("notifications", &self.policy.notifications)
            .finish_non_exhaustive()
    }
}

impl From<PgPool> for Pool {
    fn from(inner: PgPool) -> Self {
        Self {
            inner,
            policy: Arc::new(PoolPolicy::default()),
        }
    }
}

impl Pool {
    #[must_use]
    pub fn size(&self) -> u32 {
        self.inner.size()
    }

    #[must_use]
    pub fn num_idle(&self) -> usize {
        self.inner.num_idle()
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    #[must_use]
    pub fn max_connections(&self) -> u32 {
        self.inner.options().get_max_connections()
    }

    #[must_use]
    pub fn connect_options(&self) -> Arc<PgConnectOptions> {
        self.inner.connect_options()
    }

    #[must_use]
    pub fn close_event(&self) -> sqlx::pool::CloseEvent {
        self.inner.close_event()
    }

    #[must_use]
    pub fn notifications_enabled(&self) -> bool {
        self.policy.notifications
    }

    pub(crate) fn inner(&self) -> &PgPool {
        &self.inner
    }

    pub(crate) fn policy(&self) -> &Arc<PoolPolicy> {
        &self.policy
    }

    pub(crate) fn set_connect_options(&self, options: PgConnectOptions) {
        self.inner.set_connect_options(options);
    }
}

#[must_use]
pub fn prepare_pool(dsn: &Dsn, options: &PoolOptions<'_>) -> Pool {
    crate::observe::describe();
    let mut settings = Vec::new();
    if options.session_settings_source == SessionSettingsSource::Startup {
        settings.push(("statement_timeout", format_pg_duration(STATEMENT_TIMEOUT)));
        settings.push((
            "idle_in_transaction_session_timeout",
            format_pg_duration(IDLE_IN_TRANSACTION_TIMEOUT),
        ));
        if let Some(level) = options.default_isolation.as_sql() {
            settings.push(("default_transaction_isolation", level.to_owned()));
        }
    }
    let connect_options = session_connect_options(dsn, options.application_name, settings)
        .log_slow_statements(log::LevelFilter::Warn, SLOW_STATEMENT_THRESHOLD);
    let pool_options = PgPoolOptions::new()
        .max_connections(options.max_connections.get())
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .acquire_time_level(log::LevelFilter::Off)
        .acquire_slow_level(log::LevelFilter::Off)
        .max_lifetime(MAX_CONNECTION_LIFETIME)
        .idle_timeout(IDLE_CONNECTION_TIMEOUT)
        .test_before_acquire(false)
        .before_acquire(ping_if_idle)
        .after_release(|conn, _meta| {
            conn.shrink_buffers();
            accept_connection()
        });
    let slots = match options.wait_order {
        WaitOrder::Arrival => None,
        WaitOrder::NewestOnceStanding => Some(Slots::new(options.max_connections.get() as usize)),
    };
    Pool {
        inner: pool_options.connect_lazy_with(connect_options),
        policy: Arc::new(PoolPolicy {
            slots,
            notifications: options.notifications,
            metrics: PoolMetrics::default(),
            default_isolation: options.default_isolation,
            session_settings_source: options.session_settings_source,
        }),
    }
}

async fn take_slot(
    slots: Option<&Arc<Slots>>,
    deadline: Instant,
    queue: Queue,
) -> Result<Option<Slot>, sqlx::Error> {
    let Some(slots) = slots else {
        return Ok(None);
    };
    Arc::clone(slots)
        .take(queue, deadline)
        .await
        .map(Some)
        .map_err(|_elapsed| sqlx::Error::PoolTimedOut)
}

pub async fn acquire(
    pool: &Pool,
    operation: &'static str,
) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    acquire_with_queue(pool, operation, Queue::Regular, None).await
}

pub(crate) async fn acquire_probe(pool: &Pool) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    acquire_with_queue(pool, "readiness", Queue::Probe, None).await
}

pub(super) async fn acquire_for_transaction(
    pool: &Pool,
    deadline: Option<Instant>,
) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    acquire_with_queue(pool, "transaction", Queue::Regular, deadline).await
}

#[cfg(test)]
pub(crate) async fn acquire_until(
    pool: &Pool,
    operation: &'static str,
    deadline: Instant,
) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    acquire_with_queue(pool, operation, Queue::Regular, Some(deadline)).await
}

async fn acquire_with_queue(
    pool: &Pool,
    operation: &'static str,
    queue: Queue,
    caller_deadline: Option<Instant>,
) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    let (deadline, budget) = if let Some(deadline) = caller_deadline {
        (deadline, deadline.saturating_duration_since(Instant::now()))
    } else {
        let budget = pool.inner.options().get_acquire_timeout();
        (Instant::now() + budget, budget)
    };
    let enforce_caller_deadline = caller_deadline.is_some();
    observe_acquisition(operation, budget, async {
        let _slot = take_slot(pool.policy.slots.as_ref(), deadline, queue).await?;
        while !enforce_caller_deadline || Instant::now() < deadline {
            match timeout_at(deadline, pool.inner.acquire()).await {
                Err(_elapsed) => break,
                Ok(Ok(_)) if enforce_caller_deadline && Instant::now() >= deadline => break,
                Ok(Err(sqlx::Error::PoolTimedOut)) if enforce_caller_deadline => {}
                Ok(result) => return result,
            }
        }
        Err(sqlx::Error::PoolTimedOut)
    })
    .await
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "the signature `PoolOptions::before_acquire` requires"
)]
fn ping_if_idle(
    conn: &mut PgConnection,
    meta: PoolConnectionMetadata,
) -> BoxFuture<'_, Result<bool, sqlx::Error>> {
    if meta.idle_for <= PING_IDLE_AFTER {
        accept_connection()
    } else {
        Box::pin(ping_idle_connection(conn, meta.idle_for))
    }
}

fn accept_connection() -> BoxFuture<'static, Result<bool, sqlx::Error>> {
    Box::pin(std::future::poll_fn(|_| Poll::Ready(Ok(true))))
}

async fn ping_idle_connection(
    conn: &mut PgConnection,
    idle_for: Duration,
) -> Result<bool, sqlx::Error> {
    let started = Instant::now();
    let (cause, error) = match tokio::time::timeout(IDLE_PING_TIMEOUT, conn.ping()).await {
        Ok(Ok(())) => return Ok(true),
        Ok(Err(error)) => (crate::error::failure_cause(&error), error),
        Err(_elapsed) => (
            "timeout",
            sqlx::Error::Io(std::io::ErrorKind::TimedOut.into()),
        ),
    };
    tracing::warn!(
        ?idle_for,
        elapsed = ?started.elapsed(),
        budget = ?IDLE_PING_TIMEOUT,
        error.type = cause,
        "postgres_idle_ping_failed"
    );
    Err(error)
}

pub async fn admit_pool(pool: &Pool) -> Result<(), ConnectError> {
    let budget = SESSION_VERIFICATION_TIMEOUT;
    tokio::time::timeout(budget, verify_session(pool))
        .await
        .map_err(|_| ConnectError::SessionVerificationTimeout { budget })?
}

pub async fn connect(dsn: &Dsn, options: &PoolOptions<'_>) -> Result<Pool, ConnectError> {
    let pool = prepare_pool(dsn, options);
    if let Err(refused) = admit_pool(&pool).await {
        if close(&pool, SESSION_REJECTION_CLOSE_TIMEOUT).await == CloseOutcome::TimedOut {
            tracing::warn!(
                budget = ?SESSION_REJECTION_CLOSE_TIMEOUT,
                "postgres_session_rejection_close_timed_out"
            );
        }
        return Err(refused);
    }
    Ok(pool)
}

async fn verify_session(pool: &Pool) -> Result<(), ConnectError> {
    let session = observed!("check session budgets", async {
        let mut connection = acquire(pool, "check session budgets").await?;
        sqlx::query!(
            "SELECT \
             (SELECT setting::bigint FROM pg_settings WHERE name = 'statement_timeout') \
                 AS \"statement_ms!\", \
             (SELECT setting::bigint FROM pg_settings \
               WHERE name = 'idle_in_transaction_session_timeout') \
                 AS \"idle_in_transaction_ms!\", \
             current_setting('default_transaction_isolation') AS \"isolation!\""
        )
        .fetch_one(&mut *connection)
        .await
    })
    .await
    .map_err(|error| match error {
        sqlx::Error::PoolTimedOut => ConnectError::Timeout {
            budget: ACQUIRE_TIMEOUT,
        },
        other => ConnectError::Connect(other),
    })?;
    let source = pool.policy.session_settings_source;
    for (setting, found_ms, budget) in [
        ("statement_timeout", session.statement_ms, STATEMENT_TIMEOUT),
        (
            "idle_in_transaction_session_timeout",
            session.idle_in_transaction_ms,
            IDLE_IN_TRANSACTION_TIMEOUT,
        ),
    ] {
        check_budget(setting, found_ms, budget, source)?;
    }
    match pool.policy.default_isolation.as_sql() {
        Some(expected) if !expected.eq_ignore_ascii_case(&session.isolation) => {
            Err(ConnectError::SessionIsolation {
                found: session.isolation,
                expected,
                session_settings_source: source,
            })
        }
        _ => Ok(()),
    }
}

fn check_budget(
    setting: &'static str,
    found_ms: i64,
    budget: Duration,
    session_settings_source: SessionSettingsSource,
) -> Result<(), ConnectError> {
    let within =
        u128::try_from(found_ms).is_ok_and(|found| found > 0 && found <= budget.as_millis());
    if within {
        Ok(())
    } else {
        Err(ConnectError::SessionBudget {
            setting,
            found_ms,
            budget,
            session_settings_source,
        })
    }
}

pub async fn connect_session(
    dsn: &Dsn,
    options: &SessionOptions<'_>,
) -> Result<PgConnection, sqlx::Error> {
    let settings = [
        (
            "statement_timeout",
            format_pg_duration(options.statement_timeout),
        ),
        (
            "idle_in_transaction_session_timeout",
            format_pg_duration(options.idle_in_transaction_timeout),
        ),
        ("lock_timeout", format_pg_duration(options.lock_timeout)),
    ]
    .into_iter()
    .chain(
        options
            .extra
            .iter()
            .map(|&(name, value)| (name, value.to_owned())),
    );
    PgConnection::connect_with(&session_connect_options(
        dsn,
        options.application_name,
        settings,
    ))
    .await
}

fn session_connect_options<'a>(
    dsn: &Dsn,
    application_name: &str,
    settings: impl IntoIterator<Item = (&'a str, String)>,
) -> PgConnectOptions {
    let options = dsn
        .connect_options()
        .application_name(application_name)
        .extra_float_digits(None)
        .log_statements(log::LevelFilter::Off);
    #[cfg(feature = "hotpath")]
    let options = options.log_statements(log::LevelFilter::Debug);
    let mut settings = settings.into_iter().peekable();
    if settings.peek().is_some() {
        options.options(settings)
    } else {
        options
    }
}

#[must_use]
pub fn format_pg_duration(duration: Duration) -> String {
    format!("{}ms", duration.as_nanos().div_ceil(1_000_000))
}

pub async fn close(pool: &Pool, budget: Duration) -> CloseOutcome {
    match tokio::time::timeout(budget, pool.inner.close()).await {
        Ok(()) => CloseOutcome::Complete,
        Err(_elapsed) => CloseOutcome::TimedOut,
    }
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    Complete,
    TimedOut,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_params_round_up_and_name_the_unit() {
        assert_eq!(format_pg_duration(Duration::from_secs(8)), "8000ms");
        assert_eq!(format_pg_duration(Duration::from_micros(1_500)), "2ms");
        assert_eq!(format_pg_duration(Duration::from_nanos(1)), "1ms");
        assert_eq!(format_pg_duration(Duration::ZERO), "0ms");
    }

    #[test]
    fn a_session_budget_is_a_limit_no_looser_than_the_template() {
        let check = |found_ms| {
            check_budget(
                "statement_timeout",
                found_ms,
                STATEMENT_TIMEOUT,
                SessionSettingsSource::Server,
            )
        };
        assert!(check(8_000).is_ok());
        assert!(check(1).is_ok());
        for found_ms in [0, 8_001, -1] {
            std::assert_matches!(
                check(found_ms),
                Err(ConnectError::SessionBudget { .. }),
                "{found_ms}"
            );
        }
        let unlimited = check(0).unwrap_err().to_string();
        assert!(
            unlimited.contains("statement_timeout is unlimited"),
            "{unlimited}"
        );
        assert!(unlimited.contains("on the role or database"), "{unlimited}");
        let dropped = check_budget(
            "statement_timeout",
            30_000,
            STATEMENT_TIMEOUT,
            SessionSettingsSource::Startup,
        )
        .unwrap_err()
        .to_string();
        assert!(dropped.contains("is 30000ms"), "{dropped}");
        assert!(dropped.contains("pooler"), "{dropped}");
    }

    #[tokio::test]
    async fn an_unreachable_host_fails_inside_the_acquire_budget() {
        let dsn = Dsn::admit_with_environment(
            "postgres://app:pw@127.0.0.1:1/app?sslmode=disable",
            |_| false,
        )
        .unwrap();
        let options = PoolOptions {
            max_connections: NonZeroU32::MIN,
            application_name: "svc",
            default_isolation: Isolation::ServerDefault,
            session_settings_source: SessionSettingsSource::Startup,
            wait_order: WaitOrder::Arrival,
            notifications: true,
        };
        let pool = prepare_pool(&dsn, &options);
        assert_eq!(pool.size(), 0, "preparation must not open a connection");
        let started = std::time::Instant::now();
        let err = admit_pool(&pool).await.unwrap_err();
        std::assert_matches!(err, ConnectError::Timeout { .. }, "{err}");
        assert!(started.elapsed() < ACQUIRE_TIMEOUT + Duration::from_secs(2));
        assert!(!err.to_string().contains("pw"), "{err}");
        assert!(
            !pool.is_closed(),
            "admission must leave cleanup to its owner"
        );
        assert_eq!(close(&pool, ACQUIRE_TIMEOUT).await, CloseOutcome::Complete);
    }

    fn unreachable_pool(wait_order: WaitOrder) -> Pool {
        let dsn = Dsn::admit_with_environment(
            "postgres://app:pw@127.0.0.1:1/app?sslmode=disable",
            |_| false,
        )
        .unwrap();
        prepare_pool(
            &dsn,
            &PoolOptions {
                max_connections: NonZeroU32::MIN,
                application_name: "svc",
                default_isolation: Isolation::ServerDefault,
                session_settings_source: SessionSettingsSource::Startup,
                wait_order,
                notifications: true,
            },
        )
    }

    #[tokio::test(start_paused = true)]
    async fn only_a_pool_that_asked_takes_slots_and_a_slot_is_bounded_by_the_deadline() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let in_arrival_order = unreachable_pool(WaitOrder::Arrival);
        for queue in [Queue::Regular, Queue::Probe] {
            assert!(
                take_slot(in_arrival_order.policy.slots.as_ref(), deadline, queue)
                    .await
                    .unwrap()
                    .is_none()
            );
        }

        let newest_first = unreachable_pool(WaitOrder::NewestOnceStanding);
        let slots = newest_first.policy.slots.as_ref();
        let held = take_slot(slots, deadline, Queue::Regular).await.unwrap();
        assert!(held.is_some());
        let beyond_the_pool = take_slot(slots, deadline, Queue::Regular).await;
        std::assert_matches!(beyond_the_pool, Err(sqlx::Error::PoolTimedOut));
        assert!(tokio::time::Instant::now() >= deadline);
        let probe_deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let probe_beyond_the_pool = take_slot(slots, probe_deadline, Queue::Probe).await;
        std::assert_matches!(probe_beyond_the_pool, Err(sqlx::Error::PoolTimedOut));
        assert!(tokio::time::Instant::now() >= probe_deadline);

        drop(held);
        let later = tokio::time::Instant::now() + Duration::from_secs(1);
        assert!(
            take_slot(slots, later, Queue::Probe)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pool_slots_outlive_a_rotated_password_and_end_with_the_pool() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let pool = unreachable_pool(WaitOrder::NewestOnceStanding);
        let policy = Arc::downgrade(&pool.policy);
        pool.set_connect_options((*pool.connect_options()).clone().password("rotated"));
        assert!(
            take_slot(pool.policy.slots.as_ref(), deadline, Queue::Regular)
                .await
                .unwrap()
                .is_some()
        );

        drop(pool);
        assert!(policy.upgrade().is_none());
    }
}
