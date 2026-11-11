//! PostgreSQL adapter over `sqlx`.
//!
//! Owns what the driver leaves to the application: admission of the one
//! connection string ([`Dsn`]) and of a rotated password
//! ([`refresh_password_periodically`]), the pool with the template's session
//! budgets ([`connect`]), readiness participation ([`PostgresProbe`]), the
//! transaction seam with its commit-outcome policy ([`in_tx`]), the pool,
//! transaction and statement signals ([`observed`]), and maintenance
//! observation for the tables a profile cleans up ([`MaintenanceObserver`],
//! [`CleanupPass`]). Owns no business rule and no process lifecycle: the
//! composition root decides when the pool opens and closes, features decide
//! what runs inside a transaction. Rationale and the decisions behind each
//! budget: `docs/architecture/persistence.md`.

mod credentials;
mod dsn;
mod error;
mod maintenance;
mod observe;
mod pool;
mod probe;
mod transaction;

pub use credentials::{PASSWORD_REFRESH_INTERVAL, refresh_password_periodically};
pub use dsn::{Dsn, DsnError};
pub use error::{failure_cause, sqlstate, transient};
pub use maintenance::{
    CLEANUP_DURATION_BUCKETS, CLEANUP_DURATION_METRIC, Cleanup, CleanupPass, CleanupSchedule,
    MAINTENANCE_OBSERVATION_INTERVAL, MaintenanceFailure, MaintenanceObserver,
    MaintenancePopulation, PopulationSample, in_observation_tx,
};
pub use observe::{
    CONNECTION_WAIT_BUCKETS, CONNECTION_WAIT_METRIC, HISTOGRAMS, OPERATION_DURATION_BUCKETS,
    OPERATION_DURATION_METRIC, TRANSACTION_DURATION_BUCKETS, TRANSACTION_DURATION_METRIC, acquire,
    acquire_until, observed, record_metrics, record_metrics_periodically,
};
pub use pool::{
    ACQUIRE_TIMEOUT, Closed, ConnectError, IDLE_CONNECTION_TIMEOUT, IDLE_IN_TRANSACTION_TIMEOUT,
    MAX_CONNECTION_LIFETIME, PoolOptions, STATEMENT_TIMEOUT, SessionBudgets, SessionOptions,
    admit_pool, close, connect, connect_session, prepare_pool, to_runtime_param,
};
pub use probe::PostgresProbe;
pub use sqlx::postgres::PgPool;
pub use transaction::{
    Isolation, Tx, TxError, TxOptions, connection, in_tx, in_tx_until, in_tx_with,
};
