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
//! what runs inside a transaction.

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
pub use error::{failure_cause, is_transient, sqlstate};
pub use maintenance::{
    Cleanup, CleanupError, CleanupPass, CleanupPassResult, CleanupSchedule,
    MAINTENANCE_OBSERVATION_INTERVAL, MaintenanceFailure, MaintenanceObserver,
    MaintenancePopulation, PassEnd, PopulationSample, in_observation_tx, run_scheduled_cleanup,
};
pub use observe::{HISTOGRAMS, observed, record_metrics, record_metrics_periodically};
pub use pool::{
    ACQUIRE_TIMEOUT, CloseOutcome, ConnectError, PoolOptions, SessionBudgets, SessionOptions,
    acquire, acquire_until, admit_pool, close, connect, connect_session, format_pg_duration,
    prepare_pool,
};
pub use probe::PostgresProbe;
pub use sqlx::postgres::PgPool;
pub use transaction::{
    Isolation, Tx, TxError, TxOptions, in_tx, in_tx_with, in_tx_with_acquire_deadline,
    set_statement_timeout,
};
