//! Durable recipe proof. The source-only Python carrier owns process crashes,
//! backup/restore, resource observations and the initialized-service upgrade.
//! These cases cover the separate marker/aggregate and acceptance transactions
//! that the older queue-effect fixtures do not exercise.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt as _;
use infra_postgres::{Dsn, PgPool, in_tx};
use infra_webhooks::outbound::Outbound;
use integration_tests::reading_counter::{
    self as reading, Channel, Error, Operation, PreparedRequest,
};
use tokio::sync::Notify;

use super::commit_proxy::{CommitProxy, Fault};

fn operation(index: u128) -> Operation {
    Operation {
        scope: uuid::Uuid::from_u128(1).to_string(),
        operation_id: uuid::Uuid::from_u128(index).to_string(),
        article_id: uuid::Uuid::from_u128(2).to_string(),
        content_version: 1,
        content: "A fixture article".to_owned(),
    }
}

async fn setup(pool: &PgPool) -> PgPool {
    let admitted = super::template_pool(&integration_tests::dsn_for(pool).await, 4).await;
    reading::migrate(&admitted).await.expect("fixture schema");
    admitted
}

async fn counts(pool: &PgPool) -> (i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM reading_requests), \
                (SELECT count(*) FROM reading_effects), \
                (SELECT coalesce(sum(read_count),0)::bigint FROM reading_articles)",
    )
    .fetch_one(pool)
    .await
    .expect("independent business readback")
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn acceptance_commits_three_intents_once_or_rolls_them_all_back(pool: PgPool) {
    let admitted = setup(&pool).await;
    let outbound = Outbound::new(["reading".to_owned()]);
    let prepared = PreparedRequest::new(operation(10), "reading.accepted").expect("valid request");
    let rolled_back: Result<(), Error> = in_tx(&admitted, async |tx| {
        reading::accept_in_tx(tx, &prepared, &outbound, "reading").await?;
        Err(Error::Control)
    })
    .await;
    assert!(matches!(rolled_back, Err(Error::Control)));
    assert_eq!(counts(&pool).await, (0, 0, 0));
    assert_eq!(super::job_count(&pool).await, 0);
    assert!(
        reading::read_request(&admitted, prepared.operation())
            .await
            .unwrap()
            .is_none()
    );

    let (first, duplicate) = tokio::join!(
        reading::accept(&admitted, &prepared, &outbound, "reading"),
        reading::accept(&admitted, &prepared, &outbound, "reading"),
    );
    let accepted = first.expect("acceptance");
    assert_eq!(duplicate.expect("equal concurrent acceptance"), accepted);
    assert_eq!(counts(&pool).await, (1, 0, 0));
    assert_eq!(super::job_count(&pool).await, 3);
    let mut changed = prepared.operation().clone();
    changed.content = "Different immutable content".to_owned();
    let changed = PreparedRequest::new(changed, "reading.accepted").unwrap();
    assert!(matches!(
        reading::accept(&admitted, &changed, &outbound, "reading").await,
        Err(Error::Conflict)
    ));
    assert_eq!(super::job_count(&pool).await, 3);
    assert_eq!(
        reading::read_request(&admitted, prepared.operation())
            .await
            .unwrap(),
        Some(accepted)
    );
    super::close(&[&admitted]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn marker_arbitration_keeps_aggregate_atomic_and_returns_the_first_result(pool: PgPool) {
    let admitted = setup(&pool).await;
    for (index, commit) in [(20, true), (21, false)] {
        let operation = operation(index);
        let reached = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let first_pool = admitted.clone();
        let first_operation = operation.clone();
        let first_reached = Arc::clone(&reached);
        let first_release = Arc::clone(&release);
        let first = tokio::spawn(async move {
            in_tx(&first_pool, async |tx| {
                let effect =
                    reading::apply_effect_in_tx(tx, Channel::Local, &first_operation).await?;
                first_reached.notify_one();
                first_release.notified().await;
                if commit {
                    Ok(effect)
                } else {
                    Err(Error::Control)
                }
            })
            .await
        });
        super::bounded("first marker is uncommitted", reached.notified()).await;
        let second_pool = admitted.clone();
        let second_operation = operation.clone();
        let second = tokio::spawn(async move {
            reading::apply_effect(&second_pool, Channel::Local, &second_operation).await
        });
        let observed = AssertUnwindSafe(super::wait_for_lock_waiter(&pool))
            .catch_unwind()
            .await;
        release.notify_one();
        let first = super::bounded("first transaction joins", first)
            .await
            .unwrap();
        let second = super::bounded("waiting duplicate joins", second)
            .await
            .unwrap()
            .unwrap();
        if let Err(panic) = observed {
            std::panic::resume_unwind(panic);
        }
        if commit {
            assert_eq!(first.unwrap(), second);
        } else {
            assert!(matches!(first, Err(Error::Control)));
        }
        assert_eq!(second.read_count, i64::try_from(index - 19).unwrap());
        assert_eq!(
            reading::read_effect(&admitted, Channel::Local, &operation)
                .await
                .unwrap(),
            Some(second)
        );
    }
    assert_eq!(counts(&pool).await, (0, 2, 2));
    let first = operation(20);
    let repeated = reading::apply_effect(&admitted, Channel::Local, &first)
        .await
        .unwrap();
    assert_eq!(
        repeated.read_count, 1,
        "stored first result survives later reads of the article"
    );
    let mut conflict = first.clone();
    conflict.content_version = 2;
    assert!(matches!(
        reading::apply_effect(&admitted, Channel::Local, &conflict).await,
        Err(Error::Conflict)
    ));
    assert_eq!(counts(&pool).await, (0, 2, 2));
    // Receiver channels have separate effects even for the same logical operation.
    reading::apply_effect(&admitted, Channel::Outbox, &first)
        .await
        .unwrap();
    reading::apply_effect(&admitted, Channel::Webhook, &first)
        .await
        .unwrap();
    assert_eq!(counts(&pool).await, (0, 4, 4));
    super::close(&[&admitted]).await;
}

async fn proxied_pool(pool: &PgPool) -> (CommitProxy, PgPool) {
    let dsn = integration_tests::dsn_for(pool).await;
    assert_eq!(dsn.ssl_mode_name(), "disable");
    let server = tokio::net::lookup_host((dsn.host(), dsn.port()))
        .await
        .unwrap()
        .next()
        .unwrap();
    let proxy = CommitProxy::start(server).await;
    let mut url = integration_tests::url_for(pool, integration_tests::DATABASE_URL).await;
    url.set_ip_host(proxy.address().ip()).unwrap();
    url.set_port(Some(proxy.address().port())).unwrap();
    let proxied = Dsn::admit(url.as_str()).unwrap();
    let admitted = super::template_pool(&proxied, 2).await;
    (proxy, admitted)
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn lost_commit_ack_requires_same_identity_readback_before_replay(pool: PgPool) {
    let admitted = setup(&pool).await;
    let outbound = Outbound::new(["reading".to_owned()]);
    for (index, fault, persisted) in [
        (30, Fault::ForwardThenDrop, true),
        (31, Fault::DropBeforeForward, false),
    ] {
        let (proxy, proxied) = proxied_pool(&pool).await;
        let prepared = PreparedRequest::new(operation(index), "reading.accepted").unwrap();
        proxy.arm(fault);
        let result = reading::accept(&proxied, &prepared, &outbound, "reading").await;
        assert!(result.unwrap_err().commit_unknown());
        assert_eq!(proxy.fired(), Some(fault));
        let readback = reading::read_request(&admitted, prepared.operation())
            .await
            .unwrap();
        assert_eq!(
            readback.is_some(),
            persisted,
            "unknown is not claimed as committed or absent"
        );
        let accepted = reading::accept(&admitted, &prepared, &outbound, "reading")
            .await
            .unwrap();
        if let Some(original) = readback {
            assert_eq!(accepted, original);
        }
        super::close(&[&proxied]).await;
        proxy.shutdown().await;
    }
    assert_eq!(counts(&pool).await, (2, 0, 0));
    assert_eq!(super::job_count(&pool).await, 6);

    let (proxy, proxied) = proxied_pool(&pool).await;
    let operation = operation(30);
    proxy.arm(Fault::ForwardThenDrop);
    let effect = reading::apply_effect(&proxied, Channel::Outbox, &operation)
        .await
        .unwrap();
    assert_eq!(proxy.fired(), Some(Fault::ForwardThenDrop));
    assert_eq!(effect.read_count, 1);
    assert_eq!(
        reading::read_effect(&admitted, Channel::Outbox, &operation)
            .await
            .unwrap(),
        Some(effect)
    );
    assert_eq!(counts(&pool).await, (2, 1, 1));
    super::close(&[&proxied, &admitted]).await;
    proxy.shutdown().await;
}
