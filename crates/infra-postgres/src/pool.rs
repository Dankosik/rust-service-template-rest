//! The pool and the template's session defaults.
//!
//! Budgets are constants rather than configuration keys: a service that
//! needs different ones changes them here, in one reviewed place, instead
//! of every deployment discovering its own. The pool size is the exception
//! and comes from configuration, because the right value depends on the
//! database and on how many instances share it.

use std::num::NonZeroU32;
use std::time::Duration;

use sqlx::ConnectOptions;
use sqlx::Connection;
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool, PgPoolOptions};
use tokio_util::sync::CancellationToken;

use crate::dsn::Dsn;
use crate::observe::{CONNECTION_COUNT_METRIC, CONNECTION_MAX_METRIC};
use crate::transaction::Isolation;

/// Bound on waiting for a pooled connection, including opening a new one.
/// The startup connection draws the same budget.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(3);

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

/// Open the pool and establish its first connection.
///
/// # Errors
///
/// [`ConnectError::Timeout`] when no connection is established inside
/// [`ACQUIRE_TIMEOUT`]; [`ConnectError::Connect`] when the first attempt is
/// refused (credentials, TLS, or the server).
pub async fn connect(dsn: &Dsn, options: &PoolOptions<'_>) -> Result<PgPool, ConnectError> {
    crate::observe::describe();
    let mut settings = vec![
        ("statement_timeout", to_runtime_param(STATEMENT_TIMEOUT)),
        (
            "idle_in_transaction_session_timeout",
            to_runtime_param(IDLE_IN_TRANSACTION_TIMEOUT),
        ),
    ];
    if let Some(level) = options.default_isolation.as_sql() {
        settings.push(("default_transaction_isolation", level.to_owned()));
    }
    let connect_options = session(dsn, options.application_name, settings)
        .log_slow_statements(log::LevelFilter::Warn, SLOW_STATEMENT_THRESHOLD);
    PgPoolOptions::new()
        .max_connections(options.max_connections.get())
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .test_before_acquire(false)
        .before_acquire(|conn, meta| {
            Box::pin(async move {
                if meta.idle_for > PING_IDLE_AFTER {
                    conn.ping().await?;
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
        })
        .connect_with(connect_options)
        .await
        .map_err(|err| match err {
            sqlx::Error::PoolTimedOut => ConnectError::Timeout {
                budget: ACQUIRE_TIMEOUT,
            },
            other => ConnectError::Connect(other),
        })
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
fn session<'a>(
    dsn: &Dsn,
    application_name: &str,
    settings: impl IntoIterator<Item = (&'a str, String)>,
) -> PgConnectOptions {
    dsn.connect_options()
        .application_name(application_name)
        .options(settings)
        .log_statements(log::LevelFilter::Off)
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
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ConnectError::Timeout { .. }), "{err}");
        assert!(started.elapsed() < ACQUIRE_TIMEOUT + Duration::from_secs(2));
        assert!(!err.to_string().contains("pw"), "{err}");
    }
}
