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
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
mod transaction;

pub use dsn::{Dsn, DsnError, SslPolicy};
pub use error::{failure_cause, is_transient, sqlstate};
pub use observe::HISTOGRAMS;
pub use pool::{
    ACQUIRE_TIMEOUT, CloseOutcome, ConnectError, ListenerPool, Pool, PoolOptions, PoolStartup,
    SessionOptions, SessionSettingsSource, WaitOrder, connect_session, format_pg_duration,
};
pub use probe::{PostgresProbe, SessionProperties, session_properties};
pub use sqlx::Error as SqlError;
pub use transaction::{
    BudgetedTx, Isolation, OperationDeadline, OperationDeadlineElapsed, SavepointEnd,
    SavepointError, Tx, TxError, TxMode, TxOptions,
};

#[doc(hidden)]
pub mod __statement_observation {
    pub use metrics::{Key, Label};

    pub use crate::observe::{OPERATION_DURATION_METRIC, observed_statement};
}
