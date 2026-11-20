use std::time::Duration;

use backon::BackoffBuilder as _;
use infra_postgres::Pool;
use sqlx::postgres::{PgListener, PgPool, PgPoolOptions};
use tokio_util::sync::CancellationToken;

use crate::engine::{
    Engine, Operation, POLL_INTERVAL, WorkerGroup, observe_failure, observe_recovery,
};

const LISTENER_CLOSE_BUDGET: Duration = Duration::from_secs(1);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);
const RECONNECT_BASE_CAP: Duration = Duration::from_secs(15);

type ReconnectBackoff = <backon::ExponentialBuilder as backon::BackoffBuilder>::Backoff;

pub(crate) async fn run_listener(engine: &Engine, cancel: CancellationToken) {
    let primary_group = engine.primary_group();
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

fn listener_pool(engine_pool: &Pool) -> PgPool {
    PgPoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_lazy_with(engine_pool.connect_options().as_ref().clone())
}

fn follow_connect_options(engine_pool: &Pool, listener_pool: &PgPool) {
    listener_pool.set_connect_options(engine_pool.connect_options().as_ref().clone());
}

fn reconnect_backoff() -> ReconnectBackoff {
    backon::ExponentialBuilder::default()
        .with_min_delay(POLL_INTERVAL)
        .with_max_delay(RECONNECT_BASE_CAP)
        .with_jitter()
        .without_max_times()
        .build()
}

fn next_reconnect_delay(backoff: &mut ReconnectBackoff) -> Duration {
    backoff
        .next()
        .unwrap_or(RECONNECT_MAX_DELAY)
        .min(RECONNECT_MAX_DELAY)
}

async fn listen(primary_group: &WorkerGroup, engine: &Engine, listener_pool: &PgPool) -> ! {
    let mut backoff = reconnect_backoff();
    loop {
        follow_connect_options(&primary_group.pool, listener_pool);
        let error = match subscribe(listener_pool).await {
            Ok(listener) => {
                backoff = reconnect_backoff();
                observe_recovery(primary_group, Operation::Listen);
                drain_notifications(engine, listener).await
            }
            Err(error) => error,
        };
        observe_failure(primary_group, Operation::Listen, &error.into());
        tokio::time::sleep(next_reconnect_delay(&mut backoff)).await;
    }
}

async fn drain_notifications(engine: &Engine, mut listener: PgListener) -> sqlx::Error {
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
            Ok(None) => return connection_lost(),
            Err(error) => return error,
        }
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
    use super::*;

    #[test]
    fn reconnect_backoff_starts_at_poll_interval_and_stays_bounded_without_ending() {
        let mut backoff = reconnect_backoff();
        let first = next_reconnect_delay(&mut backoff);
        assert!(
            first >= POLL_INTERVAL && first <= POLL_INTERVAL * 2,
            "{first:?}"
        );
        let mut last = first;
        for _ in 0..16 {
            last = next_reconnect_delay(&mut backoff);
            assert!(
                last <= RECONNECT_MAX_DELAY,
                "jittered cap stays bounded: {last:?}"
            );
        }
        assert!(
            last >= RECONNECT_BASE_CAP && last <= RECONNECT_MAX_DELAY,
            "{last:?}"
        );
    }
}
