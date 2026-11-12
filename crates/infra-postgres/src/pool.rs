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

use std::future::Future;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::time::Duration;

use sqlx::pool::{PoolConnection, PoolConnectionMetadata};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool, PgPoolOptions};
use sqlx::{ConnectOptions, Connection, Postgres};

use crate::dsn::Dsn;
use crate::observe::{acquisition, observed};
use crate::transaction::Isolation;

/// Bound on waiting for a pooled connection, including opening a new one.
/// The startup connection draws the same budget.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(3);

/// Client bound on the entire mandatory session readback, including
/// acquisition: the acquire budget plus room for the readback itself. Partial
/// replies do not restart it.
const SESSION_VERIFICATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Client bound on closing a pool whose session admission failed, so a
/// refused startup is reported instead of waiting on a remote socket close.
const SESSION_REJECTION_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Session default for `statement_timeout` on every pooled connection.
/// Matches the default HTTP request budget (`http.request_timeout` 8s). It
/// is not kept in lockstep with a configured request timeout; both stay
/// independent adapter vs operator values.
pub(crate) const STATEMENT_TIMEOUT: Duration = Duration::from_secs(8);

/// Session default for `idle_in_transaction_session_timeout` on every
/// pooled connection. Same duration as [`STATEMENT_TIMEOUT`] by policy, but
/// a separate constant so a later edit of one setting does not silently
/// retune the other.
///
/// It covers what `statement_timeout` cannot: a transaction that ran a fast
/// statement and then lost its client holds its locks while no statement is
/// running at all.
pub(crate) const IDLE_IN_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(8);

/// Statements slower than this are logged at `warn` with their SQL text and
/// duration. Routine statement logging defaults to off to avoid repeating
/// every query and carrying data into logs through driver error paths.
/// The `hotpath` feature enables routine statement logging at `debug`.
const SLOW_STATEMENT_THRESHOLD: Duration = Duration::from_secs(1);

/// A pooled connection idle longer than this is pinged before it is handed
/// out; a busier one is handed out as is. The `sqlx` default pings on every
/// acquire, which doubles the round trips of a single-statement request.
/// Same threshold as pgx's pool, which the Go template uses. A connection the
/// server dropped while it was busy fails its next statement either way.
const PING_IDLE_AFTER: Duration = Duration::from_secs(1);

/// A pooled connection older than this is retired when it is returned to the
/// pool or checked for idleness, instead of being reused; a session already
/// handed out is never interrupted. Retirement is how a pool picks up what
/// changed since a connection opened: a rotated password, a changed role or
/// database default, a DNS answer that moved to another server. The driver's
/// own default, named here because password rotation depends on it.
pub(crate) const MAX_CONNECTION_LIFETIME: Duration = Duration::from_mins(30);

/// A pooled connection unused for this long is closed, so a pool sized for a
/// peak returns its server slots after it. The driver's own default.
pub(crate) const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_mins(10);

/// Bound on the ping of a connection idle past [`PING_IDLE_AFTER`]. A peer
/// that vanished without a reset (a load balancer's idle cut-off, a failed
/// node) never answers, and an unbounded ping would spend the caller's whole acquire budget on one dead connection;
/// past this bound the connection is discarded and the acquire moves on to
/// the next one or opens a new one. Each dead connection still costs its
/// caller this bound, so a pool with two or more of them fails one acquire
/// before it is clean again. It is a hang guard, not a latency target: a slow
/// but live server is waited for, and every discard is logged as
/// `postgres_idle_ping_failed`. It stays below [`ACQUIRE_TIMEOUT`] so one
/// dead connection still leaves time to open a replacement.
const IDLE_PING_TIMEOUT: Duration = Duration::from_secs(2);

/// Why the pool could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// No connection could be established inside [`ACQUIRE_TIMEOUT`]; the
    /// pool retries a refused or unreachable target until the budget ends,
    /// so this is what an unreachable database looks like at startup.
    #[error("postgres connect: no connection established inside the {budget:?} acquire budget")]
    Timeout { budget: Duration },
    /// The mandatory session readback did not complete before its client deadline.
    #[error("postgres session verification: did not complete inside the {budget:?} budget")]
    SessionVerificationTimeout { budget: Duration },
    /// A driver error while acquiring a connection or reading the session settings.
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
        "postgres session: default_transaction_isolation is {found}, expected {expected}; {}",
        .source_of_budgets.remedy()
    )]
    SessionIsolation {
        found: String,
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
/// connect wait. The caller of [`connect_session`] bounds that wait.
#[derive(Clone, Debug)]
pub struct SessionOptions<'a> {
    pub application_name: &'a str,
    pub statement_timeout: Duration,
    pub idle_in_transaction_timeout: Duration,
    /// Includes the wait for the advisory lock.
    pub lock_timeout: Duration,
    pub extra: &'a [(&'a str, &'a str)],
}

/// Constructs the native pool without connecting. Retain it before awaiting
/// [`admit_pool`] so cancellation or unwind cannot lose its cleanup owner.
#[must_use]
pub fn prepare_pool(dsn: &Dsn, options: &PoolOptions<'_>) -> PgPool {
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
        // sqlx grows a connection's read and write buffers to the largest
        // message it carried and keeps them until the connection closes; one
        // large body per connection would otherwise stay resident for the
        // connection's lifetime. Shrinking on release keeps idle connections
        // at the driver's default buffer size.
        .after_release(|conn, _meta| {
            conn.shrink_buffers();
            Box::pin(async { Ok(true) })
        });
    pool_options.connect_lazy_with(connect_options)
}

/// Acquire a native pooled connection with bounded operation diagnostics.
///
/// `operation` is a callsite-owned static label, never a request or SQL value.
/// Slow successes and native acquire timeouts are logged; cancellation leaves
/// the native future and its ownership to `SQLx` without inventing an outcome.
///
/// # Errors
///
/// The native acquisition error, unchanged.
#[expect(
    clippy::disallowed_methods,
    reason = "the pool adapter owns the native checkout"
)]
pub async fn acquire(
    pool: &PgPool,
    operation: &'static str,
) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    acquisition(
        operation,
        pool.options().get_acquire_timeout(),
        pool.acquire(),
    )
    .await
}

/// [`acquire`] for a caller whose own deadline bounds the work: the wait lasts
/// until `deadline`, not [`ACQUIRE_TIMEOUT`]. A caller
/// that waits is served once a connection frees, and cutting its wait short
/// refuses work the pool could still do. Pass the deadline of the request or
/// job, never an invented one; without a deadline use [`acquire`].
///
/// The pool bounds each native wait by its fixed acquire timeout, so this waits
/// again while `deadline` allows. Each wait queues behind those that arrived
/// meanwhile; a connection still frees in the order the pool serves it. A wait
/// that ends at `deadline` is the native `PoolTimedOut`, so callers keep one
/// failure to map.
///
/// # Errors
///
/// The native acquisition error, unchanged, or `PoolTimedOut` at `deadline`.
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
            loop {
                match tokio::time::timeout_at(deadline, pool.acquire()).await {
                    Err(_elapsed) => return Err(sqlx::Error::PoolTimedOut),
                    Ok(Err(sqlx::Error::PoolTimedOut))
                        if tokio::time::Instant::now() < deadline => {}
                    Ok(result) => return result,
                }
            }
        },
    )
    .await
}

/// Ping a connection idle past [`PING_IDLE_AFTER`] before it is handed out,
/// within [`IDLE_PING_TIMEOUT`]; an error discards it.
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
        // The acquire moves on to another connection; this line is the only
        // trace of the discarded one.
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

/// Verifies the session policy on one connection from the retained pool.
/// One client deadline covers acquisition and the complete readback;
/// [`ConnectError::SessionVerificationTimeout`] reports it. Cancellation
/// leaves cleanup to the caller.
///
/// # Errors
///
/// Returns a driver error from checkout or readback, an acquire timeout, a
/// session-verification timeout, or a session policy refusal. This function
/// never closes the caller's pool.
pub async fn admit_pool(pool: &PgPool, options: &PoolOptions<'_>) -> Result<(), ConnectError> {
    let budget = SESSION_VERIFICATION_TIMEOUT;
    tokio::time::timeout(budget, verify_session(pool, options))
        .await
        .map_err(|_| ConnectError::SessionVerificationTimeout { budget })?
}

/// Opens the pool and verifies that its first session carries the required policy.
/// Process roots use [`prepare_pool`] and [`admit_pool`] to retain cleanup ownership.
/// Verification and rejection cleanup each have their own client bound.
/// These sequential waits are not a whole-bootstrap or remote socket-close bound.
///
/// # Errors
///
/// Returns the admission refusal unchanged. Failed admission closes the pool
/// within its bound and records an incomplete close separately.
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
        check_budget(setting, found_ms, budget, options.session_budgets)?;
    }
    match options.default_isolation.as_sql() {
        Some(expected) if !expected.eq_ignore_ascii_case(&session.isolation) => {
            Err(ConnectError::SessionIsolation {
                found: session.isolation,
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
    PgConnection::connect_with(&session_connect_options(
        dsn,
        options.application_name,
        settings,
    ))
    .await
}

/// Connect options that publish `settings` through the startup packet, so a
/// connection opened later carries them too, without a round trip.
///
/// `extra_float_digits` is left to the server: the driver would otherwise
/// publish `2`, which every supported server already treats like its own
/// default, and which a pooler refuses as a startup parameter it does not
/// track.
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
pub fn to_runtime_param(duration: Duration) -> String {
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
        assert_eq!(to_runtime_param(Duration::from_secs(8)), "8000ms");
        assert_eq!(to_runtime_param(Duration::from_micros(1_500)), "2ms");
        assert_eq!(to_runtime_param(Duration::from_nanos(1)), "1ms");
        assert_eq!(to_runtime_param(Duration::ZERO), "0ms");
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
        let options = PoolOptions {
            max_connections: NonZeroU32::MIN,
            application_name: "svc",
            default_isolation: Isolation::ServerDefault,
            session_budgets: SessionBudgets::Startup,
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
}
