//! Cancellation ownership and bounded return of a query-pool connection.

use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, PgPool, Postgres};
use tokio::time::{Instant, timeout_at};

// The same protocol-round-trip allowance as the pool's idle ping. Release
// includes queued rollback, buffer shrinking, the driver's ping and close.
const RETURN_BUDGET: Duration = Duration::from_secs(1);

/// Run one operation with a connection from the shared query pool.
///
/// Cancellation discards the connection and releases its local pool permit.
/// Ordinary completion returns it through the driver's cleanup within one
/// second, preserving `work`'s result even if that cleanup cannot finish.
/// This bounds local capacity recovery, not server rollback confirmation.
///
/// The callback borrows a connection, not transaction ownership. Use
/// [`crate::in_tx`] for work that needs a transaction.
///
/// # Errors
///
/// Only acquisition returns the outer error. The callback's output, including
/// any operation error it contains, remains the inner result.
pub async fn with_connection<T, F>(pool: &PgPool, work: F) -> Result<T, sqlx::Error>
where
    F: AsyncFnOnce(&mut PgConnection) -> T,
{
    let mut checkout = Checkout::acquire(pool).await?;
    let result = work(checkout.connection()).await;
    checkout.release().await;
    Ok(result)
}

/// Owns the connection through BEGIN, callback work and COMMIT. In particular,
/// SQLx 0.9.0 has not yet armed its rollback guard during a pending BEGIN.
pub(crate) struct Checkout {
    connection: Option<PoolConnection<Postgres>>,
}

impl Checkout {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guarded checkout owns pool acquisition"
    )]
    pub(crate) async fn acquire(pool: &PgPool) -> Result<Self, sqlx::Error> {
        Ok(Self {
            connection: Some(pool.acquire().await?),
        })
    }

    #[expect(
        clippy::expect_used,
        reason = "only consuming release takes the live connection"
    )]
    pub(crate) fn connection(&mut self) -> &mut PgConnection {
        self.connection.as_mut().expect("checkout is live")
    }

    pub(crate) async fn release(mut self) {
        let deadline = Instant::now() + RETURN_BUDGET;
        if let Some(mut connection) = self.connection.take() {
            // SQLx 0.9.0's public, doc-hidden future takes the live connection
            // and its decrement-size guard before polling. Dropping the future
            // closes the socket and releases the permit, with no cleanup task.
            // Recheck this ownership on an upgrade; our pool keeps min = 0.
            let returning = connection.return_to_pool();
            if timeout_at(deadline, returning).await.is_err() {
                tracing::warn!("postgres connection return exceeded its budget; discarded");
            }
        }
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            // Detach releases capacity synchronously. Dropping the raw
            // connection closes it without waiting for the peer to respond.
            drop(connection.detach());
        }
    }
}
