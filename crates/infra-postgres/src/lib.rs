mod credentials;
mod dsn;
mod error;
pub mod maintenance;
mod observe;
mod pool;
mod probe;
mod slots;
#[cfg(test)]
mod test_metrics;
mod transaction;

pub use credentials::{PASSWORD_REFRESH_INTERVAL, refresh_password_periodically};
pub use dsn::{Dsn, DsnError, SslPolicy};
pub use error::{failure_cause, is_transient, sqlstate};
pub use observe::{HISTOGRAMS, record_metrics, record_metrics_periodically};
pub use pool::{
    ACQUIRE_TIMEOUT, CloseOutcome, ConnectError, Pool, PoolOptions, SessionOptions,
    SessionSettingsSource, WaitOrder, acquire, admit_pool, close, connect, connect_session,
    format_pg_duration, prepare_pool,
};
pub use probe::{PostgresProbe, SessionProperties, session_properties};
pub use sqlx::Error as SqlError;
pub use transaction::{
    BudgetedTx, Isolation, OperationDeadline, OperationDeadlineElapsed, SavepointEnd,
    SavepointError, Tx, TxError, TxMode, TxOptions, in_tx, in_tx_with, in_tx_with_acquire_deadline,
    in_tx_with_deadline, set_statement_timeout,
};

#[doc(hidden)]
pub mod __statement_observation {
    pub use metrics::{Key, Label};

    pub use crate::observe::{OPERATION_DURATION_METRIC, observed_with_success_key};
}
