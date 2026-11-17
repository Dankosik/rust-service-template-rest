mod credentials;
mod dsn;
mod error;
mod maintenance;
mod observe;
mod pool;
mod probe;
mod slots;
#[cfg(test)]
mod test_metrics;
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
    ACQUIRE_TIMEOUT, CloseOutcome, ConnectError, PoolOptions, SessionOptions,
    SessionSettingsSource, WaitOrder, acquire, acquire_until, admit_pool, close, connect,
    connect_session, format_pg_duration, prepare_pool,
};
pub use probe::PostgresProbe;
pub use sqlx::Error as SqlError;
pub use sqlx::postgres::PgPool;
pub use transaction::{
    Isolation, Tx, TxError, TxOptions, in_tx, in_tx_with, in_tx_with_acquire_deadline,
    set_statement_timeout,
};
