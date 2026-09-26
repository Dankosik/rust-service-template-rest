//! PostgreSQL adapter over `sqlx`.
//!
//! Owns what the driver leaves to the application: admission of the one
//! connection string ([`Dsn`]), the pool with the template's session
//! budgets ([`connect`]), readiness participation ([`PostgresProbe`]), and
//! the transaction seam with its commit-outcome policy ([`in_tx`]). Owns no
//! business rule and no process lifecycle: the composition root decides when
//! the pool opens and closes, features decide what runs inside a
//! transaction. Rationale and the decisions behind each budget:
//! `docs/architecture/persistence.md`.

mod dsn;
mod error;
mod pool;
mod probe;
mod transaction;

pub use dsn::{AMBIENT_ENVIRONMENT, Dsn, DsnError};
pub use error::{
    commit_definitely_failed, failure_cause, idempotency_transient, raw_sqlstate, retryable,
    sqlstate,
};
pub use pool::{
    ACQUIRE_TIMEOUT, CONNECTION_COUNT_METRIC, Closed, ConnectError, IDLE_IN_TRANSACTION_TIMEOUT,
    PoolOptions, SLOW_STATEMENT_THRESHOLD, STATEMENT_TIMEOUT, SessionOptions, close, connect,
    connect_session, record_metrics, record_metrics_periodically, to_runtime_param,
};
pub use probe::PostgresProbe;
pub use sqlx::postgres::PgPool;
pub use transaction::{
    Isolation, ROLLBACK_TIMEOUT, Tx, TxError, TxOptions, connection, in_tx, in_tx_with,
};
