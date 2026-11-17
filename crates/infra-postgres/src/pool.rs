use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::time::Duration;

use crate::slots::{Queue, Slot, Slots};

use parking_lot::Mutex;
use sqlx::pool::{PoolConnection, PoolConnectionMetadata};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool, PgPoolOptions};
use sqlx::{ConnectOptions, Connection, Postgres};

use crate::dsn::Dsn;
use crate::observe::{acquisition, observed};
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
        found(*.found_ms),
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

fn found(milliseconds: i64) -> String {
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

#[must_use]
pub fn prepare_pool(dsn: &Dsn, options: &PoolOptions<'_>) -> PgPool {
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
    let alive = Arc::new(());
    let pool_options = PgPoolOptions::new()
        .max_connections(options.max_connections.get())
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .acquire_time_level(log::LevelFilter::Off)
        .acquire_slow_level(log::LevelFilter::Off)
        .max_lifetime(MAX_CONNECTION_LIFETIME)
        .idle_timeout(IDLE_CONNECTION_TIMEOUT)
        .test_before_acquire(false)
        .before_acquire(ping_if_idle)
        .after_release({
            let alive = Arc::clone(&alive);
            move |conn, _meta| {
                let _ = &alive;
                conn.shrink_buffers();
                Box::pin(async { Ok(true) })
            }
        });
    let pool = pool_options.connect_lazy_with(connect_options);
    let address = std::ptr::from_ref(pool.options()).addr();
    let mut known = POOL_SLOTS.lock();
    known.retain(|entry| entry.alive.strong_count() > 0 && entry.options_addr != address);
    match options.wait_order {
        WaitOrder::Arrival => {}
        WaitOrder::NewestOnceStanding => {
            known.push(PoolSlots {
                options_addr: address,
                alive: Arc::downgrade(&alive),
                slots: Slots::new(options.max_connections.get() as usize),
            });
        }
    }
    drop(known);
    pool
}

/// The driver's own queue is first come, first served and cannot be told
/// otherwise, so only as many callers as the pool has connections wait in
/// it; the rest wait for a slot here, in the order the pool asked for.
struct PoolSlots {
    options_addr: usize,
    alive: Weak<()>,
    slots: Arc<Slots>,
}

/// A pool has no place for caller state, so its slots are found by the
/// address of its options. `alive` is owned by a callback stored in those
/// options: while it lives the address belongs to that pool, and a pool
/// prepared at an address forgets whatever was recorded there before it.
static POOL_SLOTS: Mutex<Vec<PoolSlots>> = Mutex::new(Vec::new());

fn pool_slots(pool: &PgPool) -> Option<Arc<Slots>> {
    let options_addr = std::ptr::from_ref(pool.options()).addr();
    POOL_SLOTS
        .lock()
        .iter()
        .find(|entry| entry.options_addr == options_addr && entry.alive.strong_count() > 0)
        .map(|entry| Arc::clone(&entry.slots))
}

async fn take_slot(
    pool: &PgPool,
    deadline: tokio::time::Instant,
    queue: Queue,
) -> Result<Option<Slot>, sqlx::Error> {
    let Some(slots) = pool_slots(pool) else {
        return Ok(None);
    };
    if let Some(slot) = slots.try_take() {
        return Ok(Some(slot));
    }
    tokio::time::timeout_at(deadline, slots.take(queue))
        .await
        .map(Some)
        .map_err(|_elapsed| sqlx::Error::PoolTimedOut)
}

pub async fn acquire(
    pool: &PgPool,
    operation: &'static str,
) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    acquire_with_queue(pool, operation, Queue::Regular).await
}

pub(crate) async fn acquire_probe(pool: &PgPool) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    acquire_with_queue(pool, "readiness", Queue::Probe).await
}

#[expect(
    clippy::disallowed_methods,
    reason = "the pool adapter owns the native checkout"
)]
async fn acquire_with_queue(
    pool: &PgPool,
    operation: &'static str,
    queue: Queue,
) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    let budget = pool.options().get_acquire_timeout();
    let deadline = tokio::time::Instant::now() + budget;
    acquisition(operation, budget, async {
        let _slot = take_slot(pool, deadline, queue).await?;
        tokio::time::timeout_at(deadline, pool.acquire())
            .await
            .unwrap_or(Err(sqlx::Error::PoolTimedOut))
    })
    .await
}

#[expect(
    clippy::disallowed_methods,
    reason = "the pool adapter owns the native checkout"
)]
pub async fn acquire_until(
    pool: &PgPool,
    operation: &'static str,
    deadline: tokio::time::Instant,
) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    acquisition(
        operation,
        deadline.saturating_duration_since(tokio::time::Instant::now()),
        async {
            let _slot = take_slot(pool, deadline, Queue::Regular).await?;
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout_at(deadline, pool.acquire()).await {
                    Err(_elapsed) => break,
                    Ok(Ok(_)) if tokio::time::Instant::now() >= deadline => break,
                    Ok(Err(sqlx::Error::PoolTimedOut)) => {}
                    Ok(result) => return result,
                }
            }
            Err(sqlx::Error::PoolTimedOut)
        },
    )
    .await
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "the signature `PoolOptions::before_acquire` requires"
)]
fn ping_if_idle(
    conn: &mut PgConnection,
    meta: PoolConnectionMetadata,
) -> Pin<Box<dyn Future<Output = Result<bool, sqlx::Error>> + Send + '_>> {
    Box::pin(async move {
        if meta.idle_for <= PING_IDLE_AFTER {
            return Ok(true);
        }
        let started = tokio::time::Instant::now();
        let (cause, error) = match tokio::time::timeout(IDLE_PING_TIMEOUT, conn.ping()).await {
            Ok(Ok(())) => return Ok(true),
            Ok(Err(error)) => (crate::error::failure_cause(&error), error),
            Err(_elapsed) => (
                "timeout",
                sqlx::Error::Io(std::io::ErrorKind::TimedOut.into()),
            ),
        };
        tracing::warn!(
            idle_for = ?meta.idle_for,
            elapsed = ?started.elapsed(),
            budget = ?IDLE_PING_TIMEOUT,
            error.type = cause,
            "postgres_idle_ping_failed"
        );
        Err(error)
    })
}

pub async fn admit_pool(pool: &PgPool, options: &PoolOptions<'_>) -> Result<(), ConnectError> {
    let budget = SESSION_VERIFICATION_TIMEOUT;
    tokio::time::timeout(budget, verify_session(pool, options))
        .await
        .map_err(|_| ConnectError::SessionVerificationTimeout { budget })?
}

pub async fn connect(dsn: &Dsn, options: &PoolOptions<'_>) -> Result<PgPool, ConnectError> {
    let pool = prepare_pool(dsn, options);
    if let Err(refused) = admit_pool(&pool, options).await {
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

async fn verify_session(pool: &PgPool, options: &PoolOptions<'_>) -> Result<(), ConnectError> {
    let session = observed("check session budgets", async {
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
    for (setting, found_ms, budget) in [
        ("statement_timeout", session.statement_ms, STATEMENT_TIMEOUT),
        (
            "idle_in_transaction_session_timeout",
            session.idle_in_transaction_ms,
            IDLE_IN_TRANSACTION_TIMEOUT,
        ),
    ] {
        check_budget(setting, found_ms, budget, options.session_settings_source)?;
    }
    match options.default_isolation.as_sql() {
        Some(expected) if !expected.eq_ignore_ascii_case(&session.isolation) => {
            Err(ConnectError::SessionIsolation {
                found: session.isolation,
                expected,
                session_settings_source: options.session_settings_source,
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

pub async fn close(pool: &PgPool, budget: Duration) -> CloseOutcome {
    match tokio::time::timeout(budget, pool.close()).await {
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
        };
        let pool = prepare_pool(&dsn, &options);
        assert_eq!(pool.size(), 0, "preparation must not open a connection");
        let started = std::time::Instant::now();
        let err = admit_pool(&pool, &options).await.unwrap_err();
        std::assert_matches!(err, ConnectError::Timeout { .. }, "{err}");
        assert!(started.elapsed() < ACQUIRE_TIMEOUT + Duration::from_secs(2));
        assert!(!err.to_string().contains("pw"), "{err}");
        assert!(
            !pool.is_closed(),
            "admission must leave cleanup to its owner"
        );
        assert_eq!(close(&pool, ACQUIRE_TIMEOUT).await, CloseOutcome::Complete);
    }
    fn unreachable_pool(wait_order: WaitOrder) -> PgPool {
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
            },
        )
    }

    #[tokio::test(start_paused = true)]
    async fn only_a_pool_that_asked_takes_slots_and_a_slot_is_bounded_by_the_deadline() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let in_arrival_order = unreachable_pool(WaitOrder::Arrival);
        assert!(
            take_slot(&in_arrival_order, deadline, Queue::Regular)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            take_slot(&in_arrival_order, deadline, Queue::Probe)
                .await
                .unwrap()
                .is_none()
        );

        let newest_first = unreachable_pool(WaitOrder::NewestOnceStanding);
        let held = take_slot(&newest_first, deadline, Queue::Regular)
            .await
            .unwrap();
        assert!(held.is_some());
        let beyond_the_pool = take_slot(&newest_first, deadline, Queue::Regular).await;
        std::assert_matches!(beyond_the_pool, Err(sqlx::Error::PoolTimedOut));
        assert!(tokio::time::Instant::now() >= deadline);
        let probe_deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let probe_beyond_the_pool = take_slot(&newest_first, probe_deadline, Queue::Probe).await;
        std::assert_matches!(probe_beyond_the_pool, Err(sqlx::Error::PoolTimedOut));
        assert!(tokio::time::Instant::now() >= probe_deadline);

        drop(held);
        let later = tokio::time::Instant::now() + Duration::from_secs(1);
        assert!(
            take_slot(&newest_first, later, Queue::Probe)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pool_slots_outlive_a_rotated_password_and_end_with_the_pool() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let pool = unreachable_pool(WaitOrder::NewestOnceStanding);
        let options_addr = std::ptr::from_ref(pool.options()).addr();
        pool.set_connect_options((*pool.connect_options()).clone().password("rotated"));
        assert!(
            take_slot(&pool, deadline, Queue::Regular)
                .await
                .unwrap()
                .is_some()
        );

        drop(pool);
        let known = POOL_SLOTS.lock();
        assert!(
            known
                .iter()
                .all(|entry| entry.options_addr != options_addr || entry.alive.strong_count() == 0)
        );
    }
}
