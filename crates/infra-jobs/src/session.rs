//! The PostgreSQL session preconditions the engine and the operator share:
//! the startup check and transaction-local statement bounds.

use std::time::Duration;

use infra_postgres::{Tx, observed};
use sqlx::PgPool;

/// Why the jobs store cannot start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StartupError {
    #[error("the jobs store requires UTF8 server encoding")]
    UnsupportedEncoding,
    #[error("the PostgreSQL session is not writable")]
    NotWritable,
    #[error("the jobs store requires READ COMMITTED session isolation")]
    UnsupportedIsolation,
    /// Anything else, including the check's bound.
    #[error("the jobs store is unavailable")]
    Unavailable,
}

pub(crate) const STARTUP_CHECK_BUDGET: Duration = Duration::from_secs(5);

/// Bound every later statement of `tx` by `timeout`, in PostgreSQL interval
/// syntax. `set_config(..., is_local = true)` lasts until the transaction
/// ends, so PostgreSQL restores the session timeout itself on commit,
/// rollback, or a dropped connection.
pub(crate) async fn set_statement_timeout(
    tx: &mut Tx<'_>,
    timeout: &'static str,
) -> Result<(), sqlx::Error> {
    observed(
        "set statement timeout",
        sqlx::query!("SELECT set_config('statement_timeout', $1, true)", timeout)
            .fetch_one(&mut *tx),
    )
    .await?;
    Ok(())
}

/// Check UTF8, READ COMMITTED and writability, bounded by
/// [`STARTUP_CHECK_BUDGET`].
///
/// # Errors
///
/// [`StartupError::UnsupportedEncoding`] when PostgreSQL is not UTF8,
/// [`StartupError::NotWritable`] when `writable` is false, and
/// [`StartupError::UnsupportedIsolation`] when the pool default is not read
/// committed, and [`StartupError::Unavailable`] for anything else, including
/// the bound; that one is logged with a bounded cause.
pub(crate) async fn check_startup(pool: &PgPool) -> Result<(), StartupError> {
    let read = async {
        let mut connection = infra_postgres::acquire(pool, "check jobs startup").await?;
        // Whether the current session has the worker's required defaults. Migration-history
        // admission owns schema compatibility; this check keeps only live session properties.
        let session = observed(
            "check jobs session",
            sqlx::query!("SELECT current_setting('server_encoding') AS \"server_encoding!\", \
     NOT pg_is_in_recovery() AND current_setting('transaction_read_only') = 'off' AS \"writable!\", \
     current_setting('default_transaction_isolation') = 'read committed' AS \"read_committed!\"")
            .fetch_one(&mut *connection),
        )
        .await
        ?;
        Ok::<_, sqlx::Error>(session)
    };
    let session = match tokio::time::timeout(STARTUP_CHECK_BUDGET, read).await {
        Ok(Ok(session)) => session,
        Ok(Err(err)) => {
            tracing::warn!(
                sqlstate = infra_postgres::sqlstate(&err).as_deref(),
                cause = infra_postgres::failure_cause(&err),
                "jobs_startup_check_failed"
            );
            return Err(StartupError::Unavailable);
        }
        Err(_elapsed) => {
            tracing::warn!(cause = "timeout", "jobs_startup_check_failed");
            return Err(StartupError::Unavailable);
        }
    };
    if session.server_encoding != "UTF8" {
        return Err(StartupError::UnsupportedEncoding);
    }
    if !session.writable {
        return Err(StartupError::NotWritable);
    }
    if !session.read_committed {
        return Err(StartupError::UnsupportedIsolation);
    }
    Ok(())
}
