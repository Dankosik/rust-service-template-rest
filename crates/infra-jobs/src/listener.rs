//! The `LISTEN` connection that wakes claim loops when enqueue commits a due job.

use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgListener, PgPool, PgPoolOptions};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::engine::{Operation, POLL_INTERVAL, Shared, observe_failure, observe_recovery};

/// Leave room in the worker's background join budget for the other tasks.
const LISTENER_CLOSE_BUDGET: Duration = Duration::from_secs(1);

/// Wake the claim loop of every engine in this process that registers the
/// kind of a due job enqueue committed.
///
/// The listener holds one connection of its own, outside the engine pool,
/// opened with the pool's connect options. Polling stays the recovery path:
/// a lost connection or notification only delays a claim until the next tick.
pub(crate) async fn run_listener(shared: Arc<Shared>, cancel: CancellationToken) {
    let pool = listener_pool(&shared.pool);
    let _ = Box::pin(cancel.run_until_cancelled(listen(&shared, &pool))).await;
    // SQLx returns the connection from a detached UNLISTEN. A silent peer can
    // leave that cleanup pending even after the listener's own task ends.
    if tokio::time::timeout(LISTENER_CLOSE_BUDGET, pool.close())
        .await
        .is_err()
    {
        tracing::warn!("jobs_listener_close_unconfirmed");
    }
}

fn listener_pool(engine_pool: &PgPool) -> PgPool {
    PgPoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_lazy_with(engine_pool.connect_options().as_ref().clone())
}

/// Give the listener's pool the engine pool's current connect options before
/// it opens a connection. The engine pool is the one a rotated password
/// reaches (`infra_postgres::refresh_password_periodically`); the options
/// copied when the listener started would be refused after a rotation.
fn follow_connect_options(engine_pool: &PgPool, listener_pool: &PgPool) {
    listener_pool.set_connect_options(engine_pool.connect_options().as_ref().clone());
}

async fn listen(shared: &Shared, pool: &PgPool) {
    loop {
        // At most one subscription per poll interval: a connection that
        // served longer is replaced at once, and one the server keeps closing
        // costs no more connections or wake-ups than polling does.
        let next = Instant::now() + POLL_INTERVAL;
        follow_connect_options(&shared.pool, pool);
        let ended = match subscribe(pool).await {
            Ok(mut listener) => {
                observe_recovery(shared, Operation::Listen);
                // Anything committed while no listener was attached is due now.
                shared.peers.wake(None);
                loop {
                    match listener.try_recv().await {
                        Ok(Some(notification)) => {
                            shared.peers.wake(Some(notification.payload()));
                        }
                        Ok(None) => break connection_lost(),
                        Err(error) => break error,
                    }
                }
            }
            Err(error) => error,
        };
        observe_failure(shared, Operation::Listen, &ended.into());
        tokio::time::sleep_until(next).await;
    }
}

/// The driver reports a closed listener connection as no notification and
/// keeps the I/O error to itself; this stands in for it so that the loss is
/// counted and logged like any other listener failure.
fn connection_lost() -> sqlx::Error {
    sqlx::Error::Io(std::io::ErrorKind::ConnectionAborted.into())
}

/// The listener leaves reconnecting to [`listen`], which first takes the
/// engine pool's current connect options; the driver's own reconnect would
/// use the ones this connection was opened with.
async fn subscribe(pool: &PgPool) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.eager_reconnect(false);
    listener.listen(WAKE_CHANNEL).await?;
    Ok(listener)
}

/// The channel enqueue notifies with the kind name of a job due at once.
pub(crate) const WAKE_CHANNEL: &str = "background_jobs";

#[cfg(test)]
mod tests {
    use sqlx::ConnectOptions as _;
    use sqlx::postgres::PgConnectOptions;

    use super::*;

    fn password(pool: &PgPool) -> Option<String> {
        pool.connect_options()
            .to_url_lossy()
            .password()
            .map(str::to_owned)
    }

    #[tokio::test]
    async fn the_listener_connects_with_the_engine_pools_rotated_password() {
        let options: PgConnectOptions = "postgres://app:first@127.0.0.1:1/app".parse().unwrap();
        let engine_pool = PgPoolOptions::new().connect_lazy_with(options.clone());
        let listener = listener_pool(&engine_pool);
        assert_eq!(password(&listener).as_deref(), Some("first"));

        engine_pool.set_connect_options(options.password("second"));
        follow_connect_options(&engine_pool, &listener);
        assert_eq!(password(&listener).as_deref(), Some("second"));
    }
}
