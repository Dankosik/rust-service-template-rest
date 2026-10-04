//! The pool and the template's session defaults.
//!
//! Budgets are constants rather than configuration keys: a service that
//! needs different ones changes them here, in one reviewed place, instead
//! of every deployment discovering its own. The pool size is the exception
//! and comes from configuration, because the right value depends on the
//! database and on how many instances share it.
//!
//! A budget the session does not actually carry is not a budget, so opening
//! the pool reads the effective values back and refuses a session without
//! them: a pooler in front of the database may drop what the startup packet
//! published.

use std::num::NonZeroU32;
use std::time::Duration;

use sqlx::ConnectOptions;
use sqlx::Connection;
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool, PgPoolOptions};
use tokio_util::sync::CancellationToken;

use crate::dsn::Dsn;
use crate::observe::{
    CONNECTION_COUNT_METRIC, CONNECTION_MAX_METRIC, acquire, acquisition, observed,
};
use crate::transaction::Isolation;

/// Bound on waiting for a pooled connection, including opening a new one.
/// The startup connection draws the same budget.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(3);

/// Successful acquisitions slower than this receive operation diagnostics.
pub(crate) const SLOW_ACQUIRE_THRESHOLD: Duration = Duration::from_secs(1);

/// Session default for `statement_timeout` on every pooled connection.
/// Matches the default HTTP request budget (`http.request_timeout` 8s). It
/// is not kept in lockstep with a configured request timeout; both stay
/// independent adapter vs operator values.
pub const STATEMENT_TIMEOUT: Duration = Duration::from_secs(8);

/// Session default for `idle_in_transaction_session_timeout` on every
/// pooled connection. Same duration as [`STATEMENT_TIMEOUT`] by policy, but
/// a separate constant so a later edit of one setting does not silently
/// retune the other.
pub const IDLE_IN_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(8);

/// Statements slower than this are logged at `warn` with their SQL text and
/// duration. Statement-level logging is otherwise off: it would repeat every
/// query at `debug` and, through bound values in error paths, risk carrying
/// data into logs.
const SLOW_STATEMENT_THRESHOLD: Duration = Duration::from_secs(1);

/// A pooled connection idle longer than this is pinged before it is handed
/// out; a busier one is handed out as is. The `sqlx` default pings on every
/// acquire, which doubles the round trips of a single-statement request.
/// Same threshold as pgx's pool, which the Go template uses. A connection the
/// server dropped while it was busy fails its next statement either way.
const PING_IDLE_AFTER: Duration = Duration::from_secs(1);

/// A pooled connection older than this is closed instead of reused. It is
/// the bound on how long a session outlives what it was opened with: a
/// rotated password, a changed role or database default, a DNS answer that
/// moved to another server. The driver's own default, named here because the
/// password rotation relies on it.
pub const MAX_CONNECTION_LIFETIME: Duration = Duration::from_mins(30);

/// A pooled connection unused for this long is closed, so a pool sized for a
/// peak returns its server slots after it. The driver's own default.
pub const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_mins(10);

/// Bound on the ping of a connection idle past [`PING_IDLE_AFTER`]. A peer
/// that vanished without a reset (a load balancer's idle cut-off, a failed
/// node) never answers, and an unbounded ping would spend the caller's whole acquire budget on one dead connection;
/// past this bound the connection is discarded and the acquire moves on to
/// the next one or opens a new one. Each dead connection still costs its
/// caller this bound, so a pool with three or more of them fails one acquire
/// before it is clean again.
const IDLE_PING_TIMEOUT: Duration = Duration::from_secs(1);

/// Why the pool could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// No connection could be established inside [`ACQUIRE_TIMEOUT`]; the
    /// pool retries a refused or unreachable target until the budget ends,
    /// so this is what an unreachable database looks like at startup.
    #[error("postgres connect: no connection established inside the {budget:?} acquire budget")]
    Timeout { budget: Duration },
    /// The first connection was refused: credentials, TLS, or the server.
    /// The message names the failure kind, not the target.
    #[error("postgres connect: {0}")]
    Connect(#[source] sqlx::Error),
    /// The session does not carry a timeout inside the template's budget.
    /// `found_ms` is the effective value; zero is PostgreSQL's "no limit".
    #[error(
        "postgres session: {setting} is {}, expected a limit of at most {budget:?}; {}",
        found(*.found_ms),
        .source_of_budgets.remedy()
    )]
    SessionBudget {
        setting: &'static str,
        found_ms: i64,
        budget: Duration,
        source_of_budgets: SessionBudgets,
    },
    /// The session's default isolation is not the one this process requires.
    #[error(
        "postgres session: default_transaction_isolation is not {expected}; {}",
        .source_of_budgets.remedy()
    )]
    SessionIsolation {
        expected: &'static str,
        source_of_budgets: SessionBudgets,
    },
}

fn found(milliseconds: i64) -> String {
    if milliseconds == 0 {
        "unlimited".to_owned()
    } else {
        format!("{milliseconds}ms")
    }
}

/// Where a pooled session's budgets and default isolation come from
/// (`postgres.session_budgets`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionBudgets {
    /// Published by this process in every connection's startup packet. Works
    /// against PostgreSQL directly and through a pooler that applies startup
    /// parameters.
    #[default]
    Startup,
    /// Nothing is published; the database role or the database carries them
    /// (`ALTER ROLE ... SET`). For a pooler that refuses startup parameters.
    Server,
}

impl SessionBudgets {
    const fn remedy(self) -> &'static str {
        match self {
            Self::Startup => {
                "a pooler in front of the database did not apply the startup parameter"
            }
            Self::Server => {
                "postgres.session_budgets = \"server\" needs it set on the role or database"
            }
        }
    }
}

/// What the composition root decides per process.
#[derive(Clone, Debug)]
pub struct PoolOptions<'a> {
    /// `postgres.max_connections`. Zero is unrepresentable; disable the
    /// profile instead of opening a pool.
    pub max_connections: NonZeroU32,
    /// Reported as `application_name`, so `pg_stat_activity` attributes a
    /// session to the service instead of to an anonymous driver.
    pub application_name: &'a str,
    /// The default isolation for every transaction on each physical
    /// connection. [`Isolation::ServerDefault`] leaves the server setting
    /// unchanged.
    pub default_isolation: Isolation,
    /// Whether this process publishes the session budgets or the server
    /// already carries them. Either way [`connect`] verifies them.
    pub session_budgets: SessionBudgets,
}

/// What a one-off session (the migrator) decides per connection.
///
/// The `Duration` fields are PostgreSQL session GUCs, not the client
/// connect wait. The caller bounds that wait; this function does not.
#[derive(Clone, Debug)]
pub struct SessionOptions<'a> {
    /// Reported as `application_name` in `pg_stat_activity`.
    pub application_name: &'a str,
    /// Session `statement_timeout`.
    pub statement_timeout: Duration,
    /// Session `idle_in_transaction_session_timeout`.
    pub idle_in_transaction_timeout: Duration,
    /// Session `lock_timeout`, including the wait for the advisory lock.
    pub lock_timeout: Duration,
    /// Extra startup-packet GUCs, already rendered as `(name, value)`.
    pub extra: &'a [(&'a str, &'a str)],
}

/// Open the pool, establish its first connection, and verify that the
/// session carries the template's budgets.
///
/// # Errors
///
/// [`ConnectError::Timeout`] when no connection is established inside
/// [`ACQUIRE_TIMEOUT`]; [`ConnectError::Connect`] when the first attempt is
/// refused (credentials, TLS, or the server);
/// [`ConnectError::SessionBudget`] or [`ConnectError::SessionIsolation`]
/// when the session does not carry what this process requires.
pub async fn connect(dsn: &Dsn, options: &PoolOptions<'_>) -> Result<PgPool, ConnectError> {
    crate::observe::describe();
    let mut settings = Vec::new();
    if options.session_budgets == SessionBudgets::Startup {
        settings.push(("statement_timeout", to_runtime_param(STATEMENT_TIMEOUT)));
        settings.push((
            "idle_in_transaction_session_timeout",
            to_runtime_param(IDLE_IN_TRANSACTION_TIMEOUT),
        ));
        if let Some(level) = options.default_isolation.as_sql() {
            settings.push(("default_transaction_isolation", level.to_owned()));
        }
    }
    let connect_options = session(dsn, options.application_name, settings)
        .log_slow_statements(log::LevelFilter::Warn, SLOW_STATEMENT_THRESHOLD);
    let pool_options = PgPoolOptions::new()
        .max_connections(options.max_connections.get())
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .acquire_time_level(log::LevelFilter::Off)
        .acquire_slow_level(log::LevelFilter::Off)
        .max_lifetime(MAX_CONNECTION_LIFETIME)
        .idle_timeout(IDLE_CONNECTION_TIMEOUT)
        .test_before_acquire(false)
        .before_acquire(|conn, meta| {
            Box::pin(async move {
                if meta.idle_for > PING_IDLE_AFTER {
                    tokio::time::timeout(IDLE_PING_TIMEOUT, conn.ping())
                        .await
                        .map_err(|_elapsed| {
                            sqlx::Error::Io(std::io::ErrorKind::TimedOut.into())
                        })??;
                }
                Ok(true)
            })
        })
        // sqlx grows a connection's read and write buffers to the largest
        // message it carried and keeps them until the connection closes; one
        // large body per connection would otherwise stay resident for the
        // connection's lifetime. Shrinking on release keeps idle connections
        // at the driver's default buffer size.
        .after_release(|conn, _meta| {
            conn.shrink_buffers();
            Box::pin(async { Ok(true) })
        });
    let pool = acquisition(
        "connect",
        pool_options.get_acquire_timeout(),
        pool_options.connect_with(connect_options),
    )
    .await
    .map_err(|err| match err {
        sqlx::Error::PoolTimedOut => ConnectError::Timeout {
            budget: ACQUIRE_TIMEOUT,
        },
        other => ConnectError::Connect(other),
    })?;
    if let Err(refused) = verify_session(&pool, options).await {
        pool.close().await;
        return Err(refused);
    }
    Ok(pool)
}

/// Read the effective session settings back and refuse a session that lacks
/// a budget or the required default isolation.
///
/// Publishing a setting does not prove the session has it: PgBouncer before
/// 1.26 accepts a startup parameter it is told to track or ignore and drops
/// it, and with [`SessionBudgets::Server`] nothing is published at all. A
/// stricter limit than the template's is admitted; no limit, or a looser
/// one, is not.
async fn verify_session(pool: &PgPool, options: &PoolOptions<'_>) -> Result<(), ConnectError> {
    // The effective session budgets and default isolation, in milliseconds as
    // `pg_settings` stores both timeouts.
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
    .map_err(ConnectError::Connect)?;
    for (setting, found_ms, budget) in [
        ("statement_timeout", session.statement_ms, STATEMENT_TIMEOUT),
        (
            "idle_in_transaction_session_timeout",
            session.idle_in_transaction_ms,
            IDLE_IN_TRANSACTION_TIMEOUT,
        ),
    ] {
        check_budget(setting, found_ms, budget, options.session_budgets)?;
    }
    match options.default_isolation.as_sql() {
        Some(expected) if !expected.eq_ignore_ascii_case(&session.isolation) => {
            Err(ConnectError::SessionIsolation {
                expected,
                source_of_budgets: options.session_budgets,
            })
        }
        _ => Ok(()),
    }
}

fn check_budget(
    setting: &'static str,
    found_ms: i64,
    budget: Duration,
    source_of_budgets: SessionBudgets,
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
            source_of_budgets,
        })
    }
}

/// Open one connection with the caller's session parameters.
///
/// Used by the migration runner, whose budgets and extra GUCs differ from
/// the pool's. [`PgConnectOptions`] stay in this crate.
///
/// # Errors
///
/// The driver's connect error; the caller bounds the wait.
pub async fn connect_session(
    dsn: &Dsn,
    options: &SessionOptions<'_>,
) -> Result<PgConnection, sqlx::Error> {
    let settings = [
        (
            "statement_timeout",
            to_runtime_param(options.statement_timeout),
        ),
        (
            "idle_in_transaction_session_timeout",
            to_runtime_param(options.idle_in_transaction_timeout),
        ),
        ("lock_timeout", to_runtime_param(options.lock_timeout)),
    ]
    .into_iter()
    .chain(
        options
            .extra
            .iter()
            .map(|&(name, value)| (name, value.to_owned())),
    );
    PgConnection::connect_with(&session(dsn, options.application_name, settings)).await
}

/// Connect options that publish `settings` through the startup packet, so a
/// connection opened later carries them too, without a round trip.
///
/// `idle_in_transaction_session_timeout` covers what `statement_timeout`
/// cannot: a transaction that ran a fast statement and then lost its client
/// holds its locks while no statement is running at all. Statement logging
/// is off: it would repeat every query and could carry bound values.
///
/// `extra_float_digits` is left to the server: the driver would otherwise
/// publish `2`, which every supported server already treats like its own
/// default, and which a pooler refuses as a startup parameter it does not
/// track.
fn session<'a>(
    dsn: &Dsn,
    application_name: &str,
    settings: impl IntoIterator<Item = (&'a str, String)>,
) -> PgConnectOptions {
    let options = dsn
        .connect_options()
        .application_name(application_name)
        .extra_float_digits(None)
        .log_statements(log::LevelFilter::Off);
    // The driver sends an `options` parameter even for an empty list, and a
    // pooler that refuses startup parameters refuses that one too.
    let mut settings = settings.into_iter().peekable();
    if settings.peek().is_some() {
        options.options(settings)
    } else {
        options
    }
}

/// Render a duration as a PostgreSQL runtime-parameter value.
///
/// Rounded up rather than truncated so a non-zero caller never publishes
/// less time than its budget: one millisecond more cannot fail a statement
/// that would have succeeded; one millisecond less can cancel one. The unit
/// is written out because a bare integer is read against each setting's own
/// default unit.
///
/// `Duration::ZERO` renders `0ms`. PostgreSQL treats `0` as disable for
/// `statement_timeout`, `idle_in_transaction_session_timeout`, and
/// `lock_timeout` — that is not a zero-length bound.
#[must_use]
fn to_runtime_param(duration: Duration) -> String {
    format!("{}ms", duration.as_nanos().div_ceil(1_000_000))
}

/// Close the pool inside `budget`.
pub async fn close(pool: &PgPool, budget: Duration) -> Closed {
    match tokio::time::timeout(budget, pool.close()).await {
        Ok(()) => Closed::Complete,
        Err(_elapsed) => Closed::TimedOut,
    }
}

/// Whether [`close`] released every connection inside its budget.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    /// Every connection was released in time.
    Complete,
    /// The wait expired; the pool was asked to close and the budget ended.
    TimedOut,
}

/// Publish pool occupancy gauges once.
pub fn record_metrics(pool: &PgPool) {
    let size = f64::from(pool.size());
    // `num_idle` is a count of pooled connections and fits in f64 exactly.
    #[allow(clippy::cast_precision_loss)]
    let idle = pool.num_idle() as f64;
    // Literal labels select the metrics facade's static-label fast path.
    metrics::gauge!(
        CONNECTION_COUNT_METRIC,
        "db.client.connection.pool.name" => "postgres",
        "db.client.connection.state" => "idle"
    )
    .set(idle);
    metrics::gauge!(
        CONNECTION_COUNT_METRIC,
        "db.client.connection.pool.name" => "postgres",
        "db.client.connection.state" => "used"
    )
    .set((size - idle).max(0.0));
    metrics::gauge!(CONNECTION_MAX_METRIC, "db.client.connection.pool.name" => "postgres")
        .set(f64::from(pool.options().get_max_connections()));
}

/// Publish the gauges every `interval` until `cancel` fires. Same missed-tick
/// policy as histogram upkeep (`Delay`): a late tick is skipped rather than
/// replayed. Runs on the composition root's metrics upkeep cadence.
pub async fn record_metrics_periodically(
    pool: PgPool,
    interval: Duration,
    cancel: CancellationToken,
) {
    // An already cancelled token never polls the work; no detached task
    // is created.
    let _ = cancel
        .run_until_cancelled(async {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                record_metrics(&pool);
            }
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_params_round_up_and_name_the_unit() {
        assert_eq!(to_runtime_param(Duration::from_secs(8)), "8000ms");
        assert_eq!(to_runtime_param(Duration::from_micros(1_500)), "2ms");
        assert_eq!(to_runtime_param(Duration::from_nanos(1)), "1ms");
        assert_eq!(to_runtime_param(Duration::ZERO), "0ms");
    }

    #[test]
    fn session_publishes_every_setting_in_the_startup_packet() {
        let dsn =
            Dsn::admit_with_environment("postgres://app:pw@h:5432/app?sslmode=disable", |_| false)
                .unwrap();
        let options = session(
            &dsn,
            "svc",
            [
                ("statement_timeout", to_runtime_param(STATEMENT_TIMEOUT)),
                ("lock_timeout", to_runtime_param(Duration::from_secs(15))),
            ],
        );
        assert_eq!(options.get_application_name(), Some("svc"));
        assert_eq!(
            options.get_options(),
            Some("-c statement_timeout=8000ms -c lock_timeout=15000ms")
        );
        // Nothing to publish means no `options` parameter at all.
        assert_eq!(session(&dsn, "svc", []).get_options(), None);
    }

    #[test]
    fn a_session_budget_is_a_limit_no_looser_than_the_template() {
        let check = |found_ms| {
            check_budget(
                "statement_timeout",
                found_ms,
                STATEMENT_TIMEOUT,
                SessionBudgets::Server,
            )
        };
        assert!(check(8_000).is_ok());
        assert!(check(1).is_ok());
        for found_ms in [0, 8_001, -1] {
            assert!(
                matches!(check(found_ms), Err(ConnectError::SessionBudget { .. })),
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
            SessionBudgets::Startup,
        )
        .unwrap_err()
        .to_string();
        assert!(dropped.contains("is 30000ms"), "{dropped}");
        assert!(dropped.contains("pooler"), "{dropped}");
    }

    #[tokio::test]
    async fn an_unreachable_host_fails_inside_the_acquire_budget() {
        // Port 1 on loopback is refused at once; the pool keeps retrying
        // until the acquire budget ends, which is the bound asserted here.
        let dsn = Dsn::admit_with_environment(
            "postgres://app:pw@127.0.0.1:1/app?sslmode=disable",
            |_| false,
        )
        .unwrap();
        let started = std::time::Instant::now();
        let err = connect(
            &dsn,
            &PoolOptions {
                max_connections: NonZeroU32::MIN,
                application_name: "svc",
                default_isolation: Isolation::ServerDefault,
                session_budgets: SessionBudgets::Startup,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ConnectError::Timeout { .. }), "{err}");
        assert!(started.elapsed() < ACQUIRE_TIMEOUT + Duration::from_secs(2));
        assert!(!err.to_string().contains("pw"), "{err}");
    }
}
