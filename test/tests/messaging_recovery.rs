//! Real-store proof reuses the executable example's effect, never a mock inbox.
#![cfg(feature = "integration")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

#[allow(
    dead_code,
    reason = "shared transport supports other transaction fault cases"
)]
#[path = "support/commit_proxy.rs"]
mod commit_proxy;
#[allow(
    dead_code,
    reason = "shared executable also exposes worker registration"
)]
#[path = "../examples/messaging_recovery/effect.rs"]
mod effect;

use domain_events::Event;
use effect::{Applied, EffectError, Increment};
use infra_postgres::{
    Dsn, Isolation, PgPool, PoolOptions, SessionBudgets, TxError, TxOptions, in_tx_with,
};
use integration_tests::{DATABASE_URL, dsn_for, url_for};
use std::num::NonZeroU32;
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const WAIT: Duration = Duration::from_secs(10);

async fn pool_at(dsn: &Dsn) -> PgPool {
    infra_postgres::connect(
        dsn,
        &PoolOptions {
            max_connections: NonZeroU32::new(3).unwrap(),
            application_name: "messaging-recovery-test",
            default_isolation: Isolation::ReadCommitted,
            session_budgets: SessionBudgets::Startup,
        },
    )
    .await
    .unwrap()
}

async fn schema(pool: &PgPool) {
    sqlx::raw_sql(include_str!("../fixtures/messaging_recovery.sql"))
        .execute(pool)
        .await
        .unwrap();
}

fn event(id: &str, delta: i64) -> Event<Increment> {
    Event {
        id: id.to_owned(),
        occurred_at: time::UtcDateTime::from_unix_timestamp_nanos(1_700_000_000_123_456_789)
            .unwrap(),
        payload: Increment {
            counter_id: "orders".to_owned(),
            delta,
        },
    }
}

async fn value(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT value FROM recovery_counters WHERE counter_id = 'orders'")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn durable_receipt_survives_restart_and_rejects_changed_meaning(pool: PgPool) {
    schema(&pool).await;
    let dsn = dsn_for(&pool).await;
    let first = pool_at(&dsn).await;
    let original = event("stable-id", 7);
    assert_eq!(
        effect::attempt(&first, &original).await.unwrap(),
        Applied::First
    );
    timeout(WAIT, first.close()).await.unwrap();
    let restarted = pool_at(&dsn).await;
    let mut equivalent = original.clone();
    equivalent.payload = serde_json::from_str(r#"{ "delta": 7, "counter_id": "orders" }"#).unwrap();
    assert_eq!(
        effect::attempt(&restarted, &equivalent).await.unwrap(),
        Applied::Duplicate
    );
    for changed in [
        Event {
            payload: Increment {
                delta: 8,
                ..original.payload.clone()
            },
            ..original.clone()
        },
        Event {
            payload: Increment {
                counter_id: "elsewhere".to_owned(),
                ..original.payload.clone()
            },
            ..original.clone()
        },
        Event {
            occurred_at: original.occurred_at + time::Duration::nanoseconds(1),
            ..original.clone()
        },
    ] {
        let error = effect::attempt(&restarted, &changed).await.unwrap_err();
        assert!(matches!(error, EffectError::Conflict));
        assert_eq!(
            error.disposition(),
            infra_messaging::HandlerError::Permanent
        );
    }
    // A receipt from another contract version cannot stand in for this meaning.
    for sql in [
        "UPDATE recovery_effect_receipts SET event_type = 'other.kind'",
        "UPDATE recovery_effect_receipts SET event_type = 'recovery.counter.incremented', schema_version = 2",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
        assert!(matches!(
            effect::attempt(&restarted, &original).await,
            Err(EffectError::Conflict)
        ));
    }
    assert_eq!(value(&pool).await, 7);
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM recovery_effect_receipts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(receipts, 1);
    assert!(
        serde_json::from_str::<Increment>(r#"{"counter_id":"orders","delta":7,"ignored":true}"#)
            .is_err()
    );
    timeout(WAIT, restarted.close()).await.unwrap();
}

#[sqlx::test]
async fn failed_mutation_and_caller_rollback_leave_no_receipt(pool: PgPool) {
    schema(&pool).await;
    let business = pool_at(&dsn_for(&pool).await).await;
    assert_eq!(
        effect::attempt(&business, &event("full", i64::MAX))
            .await
            .unwrap(),
        Applied::First
    );
    assert!(matches!(
        effect::attempt(&business, &event("overflow", 1)).await,
        Err(EffectError::Query(_))
    ));
    let rolled_back = event("rollback", -1);
    let result = in_tx_with(
        &business,
        TxOptions {
            isolation: Isolation::ReadCommitted,
            read_only: false,
        },
        async |tx| {
            effect::apply(tx, &rolled_back).await?;
            Err::<(), _>(EffectError::Unresolved)
        },
    )
    .await;
    assert!(matches!(result, Err(EffectError::Unresolved)));
    assert_eq!(value(&pool).await, i64::MAX);
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT logical_id FROM recovery_effect_receipts ORDER BY logical_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(ids, ["full"]);
    assert_eq!(
        effect::attempt(&business, &rolled_back).await.unwrap(),
        Applied::First
    );
    assert_eq!(value(&pool).await, i64::MAX - 1);
    timeout(WAIT, business.close()).await.unwrap();
}

async fn waiting_for_arbitration(pool: &PgPool) {
    timeout(WAIT, async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() \
                 AND application_name = 'messaging-recovery-test' AND wait_event = 'transactionid')",
            ).fetch_one(pool).await.unwrap();
            if waiting { return; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("the competing INSERT waits for the original transaction");
}

#[sqlx::test]
async fn concurrent_identity_waits_for_commit_or_rollback_before_deciding(pool: PgPool) {
    schema(&pool).await;
    let business = pool_at(&dsn_for(&pool).await).await;
    for commit in [true, false] {
        let original = event(
            if commit {
                "commit-race"
            } else {
                "rollback-race"
            },
            3,
        );
        let first_pool = business.clone();
        let first_event = original.clone();
        let (inserted, reached) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let first = tokio::spawn(async move {
            in_tx_with(
                &first_pool,
                TxOptions {
                    isolation: Isolation::ReadCommitted,
                    read_only: false,
                },
                async |tx| {
                    let result = effect::apply(tx, &first_event).await?;
                    inserted.send(()).unwrap();
                    released.await.unwrap();
                    if commit {
                        Ok(result)
                    } else {
                        Err(EffectError::Unresolved)
                    }
                },
            )
            .await
        });
        timeout(WAIT, reached).await.unwrap().unwrap();
        let second_pool = business.clone();
        let second = tokio::spawn(async move { effect::attempt(&second_pool, &original).await });
        waiting_for_arbitration(&pool).await;
        release.send(()).unwrap();
        let first_result = timeout(WAIT, first).await.unwrap().unwrap();
        assert_eq!(first_result.is_ok(), commit);
        assert_eq!(
            timeout(WAIT, second).await.unwrap().unwrap().unwrap(),
            if commit {
                Applied::Duplicate
            } else {
                Applied::First
            }
        );
    }
    assert_eq!(value(&pool).await, 6);
    timeout(WAIT, business.close()).await.unwrap();
}

#[sqlx::test]
async fn unknown_commit_is_unresolved_until_later_same_identity_arbitration(pool: PgPool) {
    schema(&pool).await;
    let direct = dsn_for(&pool).await;
    assert_eq!(
        direct.ssl_mode_name(),
        "disable",
        "the existing wire proxy requires plaintext fixture PostgreSQL"
    );
    let host = direct.host().trim_start_matches('[').trim_end_matches(']');
    let server = tokio::net::lookup_host((host, direct.port()))
        .await
        .unwrap()
        .next()
        .unwrap();
    for (index, fault) in [
        commit_proxy::Fault::ForwardThenDrop,
        commit_proxy::Fault::DropBeforeForward,
    ]
    .into_iter()
    .enumerate()
    {
        let proxy = commit_proxy::CommitProxy::start(server).await;
        let mut url = url_for(&pool, DATABASE_URL).await;
        url.set_ip_host(proxy.address().ip()).unwrap();
        url.set_port(Some(proxy.address().port())).unwrap();
        let business = pool_at(&Dsn::admit(url.as_str()).unwrap()).await;
        let original = event(&format!("uncertain-{index}"), 5);
        proxy.arm(fault);
        let error = timeout(WAIT, effect::attempt(&business, &original))
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(error, EffectError::Transaction(TxError::CommitUnknown(_))),
            "{error:?}"
        );
        assert_eq!(
            error.disposition(),
            infra_messaging::HandlerError::Retryable
        );
        assert_eq!(proxy.fired(), Some(fault));
        let committed = fault == commit_proxy::Fault::ForwardThenDrop;
        // Observe durable state independently before the explicit reconciliation attempt.
        let receipts: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM recovery_effect_receipts WHERE logical_id = $1",
        )
        .bind(&original.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(receipts, i64::from(committed));
        assert_eq!(
            timeout(WAIT, effect::attempt(&business, &original))
                .await
                .unwrap()
                .unwrap(),
            if committed {
                Applied::Duplicate
            } else {
                Applied::First
            }
        );
        timeout(WAIT, business.close()).await.unwrap();
        proxy.shutdown().await;
    }
    assert_eq!(value(&pool).await, 10);
}

#[sqlx::test]
async fn cancelled_arbitration_is_retryable_and_later_delivery_can_apply(pool: PgPool) {
    schema(&pool).await;
    let business = pool_at(&dsn_for(&pool).await).await;
    let original = event("cancelled", 2);
    let first_pool = business.clone();
    let first_event = original.clone();
    let (inserted, reached) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let first = tokio::spawn(async move {
        in_tx_with(
            &first_pool,
            TxOptions {
                isolation: Isolation::ReadCommitted,
                read_only: false,
            },
            async |tx| {
                effect::apply(tx, &first_event).await?;
                inserted.send(()).unwrap();
                released.await.unwrap();
                Err::<(), _>(EffectError::Unresolved)
            },
        )
        .await
    });
    timeout(WAIT, reached).await.unwrap().unwrap();
    let cancel = CancellationToken::new();
    let second_pool = business.clone();
    let second_event = original.clone();
    let second_cancel = cancel.clone();
    let second =
        tokio::spawn(
            async move { effect::handle(&second_pool, &second_event, &second_cancel).await },
        );
    waiting_for_arbitration(&pool).await;
    cancel.cancel();
    assert_eq!(
        timeout(WAIT, second).await.unwrap().unwrap(),
        Err(infra_messaging::HandlerError::Retryable)
    );
    release.send(()).unwrap();
    assert!(matches!(
        timeout(WAIT, first).await.unwrap().unwrap(),
        Err(EffectError::Unresolved)
    ));
    assert_eq!(
        effect::attempt(&business, &original).await.unwrap(),
        Applied::First
    );
    assert_eq!(value(&pool).await, 2);
    timeout(WAIT, business.close()).await.unwrap();
}
