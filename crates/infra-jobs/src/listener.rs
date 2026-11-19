use std::time::Duration;

use sqlx::postgres::{PgListener, PgPool, PgPoolOptions};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::engine::{
    Engine, Operation, POLL_INTERVAL, WorkerGroup, observe_failure, observe_recovery,
};

const LISTENER_CLOSE_BUDGET: Duration = Duration::from_secs(1);

pub(crate) async fn run_listener(
    primary_group: &WorkerGroup,
    engine: &Engine,
    cancel: CancellationToken,
) {
    let listener_pool = listener_pool(&primary_group.pool);
    let _ =
        Box::pin(cancel.run_until_cancelled(listen(primary_group, engine, &listener_pool))).await;
    if tokio::time::timeout(LISTENER_CLOSE_BUDGET, listener_pool.close())
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

async fn listen(primary_group: &WorkerGroup, engine: &Engine, listener_pool: &PgPool) -> ! {
    loop {
        let retry_at = Instant::now() + POLL_INTERVAL;
        follow_connect_options(&primary_group.pool, listener_pool);
        let Err(error) = listen_once(primary_group, engine, listener_pool).await;
        observe_failure(primary_group, Operation::Listen, &error.into());
        tokio::time::sleep_until(retry_at).await;
    }
}

async fn listen_once(
    primary_group: &WorkerGroup,
    engine: &Engine,
    listener_pool: &PgPool,
) -> Result<std::convert::Infallible, sqlx::Error> {
    let mut listener = subscribe(listener_pool).await?;
    observe_recovery(primary_group, Operation::Listen);
    for group in engine.groups() {
        group.claim_wake.notify_one();
    }
    while let Some(notification) = listener.try_recv().await? {
        if let Some(group) = engine.group_for_kind(notification.payload()) {
            group.claim_wake.notify_one();
        }
    }
    Err(connection_lost())
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
