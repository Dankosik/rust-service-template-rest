use std::time::Duration;

use sqlx::postgres::{PgListener, PgPool, PgPoolOptions};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::engine::{Engine, Operation, POLL_INTERVAL, Shared, observe_failure, observe_recovery};

const LISTENER_CLOSE_BUDGET: Duration = Duration::from_secs(1);

pub(crate) async fn run_listener(shared: &Shared, engine: &Engine, cancel: CancellationToken) {
    let pool = listener_pool(&shared.pool);
    let _ = Box::pin(cancel.run_until_cancelled(listen(shared, engine, &pool))).await;
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

fn follow_connect_options(engine_pool: &PgPool, listener_pool: &PgPool) {
    listener_pool.set_connect_options(engine_pool.connect_options().as_ref().clone());
}

async fn listen(shared: &Shared, engine: &Engine, pool: &PgPool) -> ! {
    loop {
        let next = Instant::now() + POLL_INTERVAL;
        follow_connect_options(&shared.pool, pool);
        let ended = match subscribe(pool).await {
            Ok(mut listener) => {
                observe_recovery(shared, Operation::Listen);
                for group in engine.groups() {
                    group.claim_wake.notify_one();
                }
                loop {
                    match listener.try_recv().await {
                        Ok(Some(notification)) => {
                            if let Some(group) = engine.group_for_kind(notification.payload()) {
                                group.claim_wake.notify_one();
                            }
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

fn connection_lost() -> sqlx::Error {
    sqlx::Error::Io(std::io::ErrorKind::ConnectionAborted.into())
}

async fn subscribe(pool: &PgPool) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.eager_reconnect(false);
    listener.listen(WAKE_CHANNEL).await?;
    Ok(listener)
}

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
