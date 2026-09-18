//! The pool and the template's session defaults.
//!
//! Budgets are constants rather than configuration keys: a service that
//! needs different ones changes them here, in one reviewed place, instead
//! of every deployment discovering its own. The pool size is the exception
//! and comes from configuration, because the right value depends on the
//! database and on how many instances share it.

use std::time::Duration;

use sqlx::ConnectOptions;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use tokio_util::sync::CancellationToken;

use crate::dsn::Dsn;

/// Bound on waiting for a pooled connection, including opening a new one.
/// The startup connection draws the same budget.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(3);

/// Session default for `statement_timeout` and
/// `idle_in_transaction_session_timeout` on every pooled connection. Longer
/// than any request budget the HTTP layer allows, so the server cancels
/// only work whose caller has already given up.
pub const STATEMENT_TIMEOUT: Duration = Duration::from_secs(8);

/// Statements slower than this are logged at `warn` with their SQL text and
/// duration. Statement-level logging is otherwise off: it would repeat every
/// query at `debug` and, through bound values in error paths, risk carrying
/// data into logs.
pub const SLOW_STATEMENT_THRESHOLD: Duration = Duration::from_secs(1);

/// Metric name for pool occupancy, following the OpenTelemetry database
/// client semantic convention; labels `pool` and `state` (`idle`, `used`).
pub const CONNECTION_COUNT_METRIC: &str = "db_client_connection_count";

const POOL_NAME: &str = "postgres";

/// Why the pool could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("postgres pool size must be > 0")]
    PoolSize,
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
    /// `postgres.max_connections`.
    pub max_connections: u32,
    /// Reported as `application_name`, so `pg_stat_activity` attributes a
    /// session to the service instead of to an anonymous driver.
    pub application_name: &'a str,
}

/// Open the pool and establish its first connection.
///
/// # Errors
///
/// [`ConnectError::PoolSize`] for a zero size; [`ConnectError::Connect`]
/// when the first connection cannot be opened inside [`ACQUIRE_TIMEOUT`].
pub async fn connect(dsn: &Dsn, options: &PoolOptions<'_>) -> Result<PgPool, ConnectError> {
    if options.max_connections == 0 {
        return Err(ConnectError::PoolSize);
    }
    let connect_options = session_options(dsn.connect_options(), options.application_name);
    PgPoolOptions::new()
        .max_connections(options.max_connections)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .connect_with(connect_options)
        .await
        .map_err(|err| match err {
            sqlx::Error::PoolTimedOut => ConnectError::Timeout {
                budget: ACQUIRE_TIMEOUT,
            },
            other => ConnectError::Connect(other),
        })
}

/// Publish the template's budgets as session defaults through the startup
/// packet, so a connection the pool opens later carries them too.
///
/// `idle_in_transaction_session_timeout` covers what `statement_timeout`
/// cannot: a transaction that ran a fast statement and then lost its client
/// holds its locks while no statement is running at all.
#[must_use]
pub fn session_options(options: PgConnectOptions, application_name: &str) -> PgConnectOptions {
    let timeout = runtime_param_millis(STATEMENT_TIMEOUT);
    options
        .application_name(application_name)
        .options([
            ("statement_timeout", timeout.as_str()),
            ("idle_in_transaction_session_timeout", timeout.as_str()),
        ])
        .log_statements(log::LevelFilter::Off)
        .log_slow_statements(log::LevelFilter::Warn, SLOW_STATEMENT_THRESHOLD)
}

/// Render a duration as a PostgreSQL runtime-parameter value.
///
/// Rounded up rather than truncated so a caller never publishes less time
/// than its budget: one millisecond more cannot fail a statement that would
/// have succeeded; one millisecond less can cancel one. The unit is written
/// out because a bare integer is read against each setting's own default
/// unit.
#[must_use]
pub fn runtime_param_millis(duration: Duration) -> String {
    format!("{}ms", duration.as_nanos().div_ceil(1_000_000))
}

/// Close the pool inside `budget`, returning whether every connection was
/// released in time.
pub async fn close(pool: &PgPool, budget: Duration) -> bool {
    tokio::time::timeout(budget, pool.close()).await.is_ok()
}

/// Publish pool occupancy gauges once.
pub fn record_metrics(pool: &PgPool) {
    let size = f64::from(pool.size());
    // `num_idle` is a count of pooled connections and fits in f64 exactly.
    #[allow(clippy::cast_precision_loss)]
    let idle = pool.num_idle() as f64;
    metrics::gauge!(CONNECTION_COUNT_METRIC, "pool" => POOL_NAME, "state" => "idle").set(idle);
    metrics::gauge!(CONNECTION_COUNT_METRIC, "pool" => POOL_NAME, "state" => "used")
        .set((size - idle).max(0.0));
}

/// Publish the gauges every `interval` until `cancel` fires. Runs on the
/// composition root's metrics upkeep cadence.
pub async fn record_metrics_periodically(
    pool: PgPool,
    interval: Duration,
    cancel: CancellationToken,
) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = ticker.tick() => record_metrics(&pool),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_params_round_up_and_name_the_unit() {
        assert_eq!(runtime_param_millis(Duration::from_secs(8)), "8000ms");
        assert_eq!(runtime_param_millis(Duration::from_micros(1_500)), "2ms");
        assert_eq!(runtime_param_millis(Duration::from_nanos(1)), "1ms");
        assert_eq!(runtime_param_millis(Duration::ZERO), "0ms");
    }

    #[test]
    fn session_options_carry_the_budgets_and_the_name() {
        let dsn =
            Dsn::parse_with_environment("postgres://app:pw@h:5432/app?sslmode=disable", |_| None)
                .unwrap();
        let options = session_options(dsn.connect_options(), "svc");
        assert_eq!(options.get_application_name(), Some("svc"));
        let options = options.get_options().unwrap_or_default();
        assert!(options.contains("-c statement_timeout=8000ms"), "{options}");
        assert!(
            options.contains("-c idle_in_transaction_session_timeout=8000ms"),
            "{options}"
        );
    }

    #[tokio::test]
    async fn a_zero_pool_size_is_refused_before_connecting() {
        let dsn =
            Dsn::parse_with_environment("postgres://app:pw@h:5432/app?sslmode=disable", |_| None)
                .unwrap();
        let err = connect(
            &dsn,
            &PoolOptions {
                max_connections: 0,
                application_name: "svc",
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ConnectError::PoolSize));
    }

    #[tokio::test]
    async fn an_unreachable_host_fails_inside_the_acquire_budget() {
        // Port 1 on loopback is refused at once; the pool keeps retrying
        // until the acquire budget ends, which is the bound asserted here.
        let dsn = Dsn::parse_with_environment(
            "postgres://app:pw@127.0.0.1:1/app?sslmode=disable",
            |_| None,
        )
        .unwrap();
        let started = std::time::Instant::now();
        let err = connect(
            &dsn,
            &PoolOptions {
                max_connections: 1,
                application_name: "svc",
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ConnectError::Timeout { .. }), "{err}");
        assert!(started.elapsed() < ACQUIRE_TIMEOUT + Duration::from_secs(2));
        assert!(!err.to_string().contains("pw"), "{err}");
    }
}
