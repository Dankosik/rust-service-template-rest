#![expect(
    clippy::unwrap_used,
    reason = "a broken fixture or setup fails the test"
)]

use integration_tests::commit_proxy;

mod effect {
    use domain_events::{Event, EventPayload};
    use infra_messaging::HandlerError;
    use infra_postgres::{Isolation, Pool, Tx, TxError, TxOptions};
    use serde::{Deserialize, Serialize};
    use time::format_description::well_known::Rfc3339;
    use tokio_util::sync::CancellationToken;

    pub(super) const CONSUMER_SCOPE: &str = "messaging-recovery-counter-v1";

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub(super) struct Increment {
        pub(super) counter_id: String,
        pub(super) delta: i64,
    }

    impl EventPayload for Increment {
        const EVENT_TYPE: &'static str = "recovery.counter.incremented";
        const SCHEMA_VERSION: u16 = 1;
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Applied {
        First,
        Duplicate,
    }

    #[derive(Debug, derive_more::From, derive_more::Error)]
    pub(super) enum EffectError {
        #[from(skip)]
        Conflict,
        #[from(skip)]
        Unresolved,
        Query(sqlx::Error),
        Transaction(TxError),
    }

    impl std::fmt::Display for EffectError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(match self {
                Self::Conflict => "logical event identity conflicts with its durable receipt",
                Self::Unresolved => "durable effect is unresolved",
                Self::Query(_) => "durable effect statement failed",
                Self::Transaction(TxError::CommitUnknown(_)) => {
                    "durable effect commit is unresolved"
                }
                Self::Transaction(_) => "durable effect transaction failed",
            })
        }
    }

    impl EffectError {
        #[must_use]
        pub(super) fn disposition(&self) -> HandlerError {
            match self {
                Self::Conflict => HandlerError::Permanent,
                Self::Unresolved | Self::Query(_) | Self::Transaction(_) => HandlerError::Retryable,
            }
        }
    }

    pub(super) async fn attempt(
        pool: &Pool,
        event: &Event<Increment>,
    ) -> Result<Applied, EffectError> {
        pool.in_tx_with(
            TxOptions {
                isolation: Isolation::ReadCommitted,
                read_only: false,
            },
            async |tx| apply(tx, event).await,
        )
        .await
    }

    pub(super) async fn apply(
        tx: &mut Tx<'_>,
        event: &Event<Increment>,
    ) -> Result<Applied, EffectError> {
        let occurred_at = event
            .occurred_at
            .format(&Rfc3339)
            .map_err(|_| EffectError::Unresolved)?;
        let inserted: Option<i32> = sqlx::query_scalar(
            "INSERT INTO recovery_effect_receipts \
             (consumer_scope, logical_id, event_type, schema_version, occurred_at, counter_id, delta) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (consumer_scope, logical_id) DO NOTHING RETURNING 1",
        )
        .bind(CONSUMER_SCOPE)
        .bind(&event.id)
        .bind(Increment::EVENT_TYPE)
        .bind(i32::from(Increment::SCHEMA_VERSION))
        .bind(&occurred_at)
        .bind(&event.payload.counter_id)
        .bind(event.payload.delta)
        .fetch_optional(&mut *tx)
        .await?;
        if inserted.is_some() {
            sqlx::query(
                "INSERT INTO recovery_counters (counter_id, value) VALUES ($1, $2) \
                 ON CONFLICT (counter_id) DO UPDATE \
                 SET value = recovery_counters.value + EXCLUDED.value",
            )
            .bind(&event.payload.counter_id)
            .bind(event.payload.delta)
            .execute(&mut *tx)
            .await?;
            return Ok(Applied::First);
        }
        let same: Option<bool> = sqlx::query_scalar(
            "SELECT event_type = $3 AND schema_version = $4 AND occurred_at = $5 \
             AND counter_id = $6 AND delta = $7 FROM recovery_effect_receipts \
             WHERE consumer_scope = $1 AND logical_id = $2",
        )
        .bind(CONSUMER_SCOPE)
        .bind(&event.id)
        .bind(Increment::EVENT_TYPE)
        .bind(i32::from(Increment::SCHEMA_VERSION))
        .bind(&occurred_at)
        .bind(&event.payload.counter_id)
        .bind(event.payload.delta)
        .fetch_optional(&mut *tx)
        .await?;
        match same {
            Some(true) => Ok(Applied::Duplicate),
            Some(false) => Err(EffectError::Conflict),
            None => Err(EffectError::Unresolved),
        }
    }

    pub(super) async fn handle(
        pool: &Pool,
        event: &Event<Increment>,
        cancel: &CancellationToken,
    ) -> Result<(), HandlerError> {
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(EffectError::Unresolved),
            result = attempt(pool, event) => result,
        };
        match result {
            Ok(_) => Ok(()),
            Err(error) => {
                tracing::warn!(error = %error, "messaging_recovery_effect_failed");
                Err(error.disposition())
            }
        }
    }
}

use domain_events::Event;
use effect::{Applied, EffectError, Increment};
use infra_postgres::{CloseOutcome, Dsn, Isolation, Pool, TxError, TxOptions};
use integration_tests::dsn_for;
use sqlx::PgPool;
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const WAIT: Duration = Duration::from_secs(10);

async fn pool_at(dsn: &Dsn) -> Pool {
    integration_tests::template_pool(dsn, "messaging-recovery-test", 3, Isolation::ReadCommitted)
        .await
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
    assert_eq!(first.close(WAIT).await, CloseOutcome::Complete);
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
        std::assert_matches!(error, EffectError::Conflict);
        assert_eq!(
            error.disposition(),
            infra_messaging::HandlerError::Permanent
        );
    }
    for sql in [
        "UPDATE recovery_effect_receipts SET event_type = 'other.kind'",
        "UPDATE recovery_effect_receipts SET event_type = 'recovery.counter.incremented', schema_version = 2",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
        std::assert_matches!(
            effect::attempt(&restarted, &original).await,
            Err(EffectError::Conflict)
        );
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
    assert_eq!(restarted.close(WAIT).await, CloseOutcome::Complete);
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
    std::assert_matches!(
        effect::attempt(&business, &event("overflow", 1)).await,
        Err(EffectError::Query(_))
    );
    let rolled_back = event("rollback", -1);
    let result = business
        .in_tx_with(
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
    std::assert_matches!(result, Err(EffectError::Unresolved));
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
    assert_eq!(business.close(WAIT).await, CloseOutcome::Complete);
}

async fn waiting_for_arbitration(pool: &PgPool) {
    integration_tests::poll_until(
        "the competing INSERT waits for the original transaction",
        WAIT,
        Duration::from_millis(10),
        async || {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() \
                 AND application_name = 'messaging-recovery-test' AND wait_event = 'transactionid')",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            waiting.then_some(())
        },
    )
    .await;
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
            first_pool
                .in_tx_with(
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
        timeout(WAIT, reached)
            .await
            .expect("first arbitration enters transaction within budget")
            .expect("first arbitration signals readiness");
        let second_pool = business.clone();
        let second = tokio::spawn(async move { effect::attempt(&second_pool, &original).await });
        waiting_for_arbitration(&pool).await;
        release.send(()).expect("release first arbitration");
        let first_result = timeout(WAIT, first)
            .await
            .expect("first arbitration completes within budget")
            .expect("first arbitration task does not panic");
        assert_eq!(first_result.is_ok(), commit);
        let second_outcome = timeout(WAIT, second)
            .await
            .expect("second arbitration completes within budget")
            .expect("second arbitration task does not panic")
            .expect("second arbitration succeeds");
        assert_eq!(
            second_outcome,
            if commit {
                Applied::Duplicate
            } else {
                Applied::First
            }
        );
    }
    assert_eq!(value(&pool).await, 6);
    assert_eq!(business.close(WAIT).await, CloseOutcome::Complete);
}

#[sqlx::test]
async fn unknown_commit_is_unresolved_until_later_same_identity_arbitration(pool: PgPool) {
    schema(&pool).await;
    for (index, fault) in [
        commit_proxy::Fault::ForwardThenDrop,
        commit_proxy::Fault::DropBeforeForward,
    ]
    .into_iter()
    .enumerate()
    {
        let (proxy, proxied) = commit_proxy::CommitProxy::start_for(&pool).await;
        let business = pool_at(&proxied).await;
        let original = event(&format!("uncertain-{index}"), 5);
        proxy.arm(fault, commit_proxy::Boundary::Commit(None));
        let error = timeout(WAIT, effect::attempt(&business, &original))
            .await
            .unwrap()
            .unwrap_err();
        std::assert_matches!(
            error,
            EffectError::Transaction(TxError::CommitUnknown(_)),
            "{error:?}"
        );
        assert_eq!(
            error.disposition(),
            infra_messaging::HandlerError::Retryable
        );
        assert_eq!(proxy.fired(), Some(fault));
        let committed = fault == commit_proxy::Fault::ForwardThenDrop;
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
        assert_eq!(business.close(WAIT).await, CloseOutcome::Complete);
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
        first_pool
            .in_tx_with(
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
    std::assert_matches!(
        timeout(WAIT, first).await.unwrap().unwrap(),
        Err(EffectError::Unresolved)
    );
    assert_eq!(
        effect::attempt(&business, &original).await.unwrap(),
        Applied::First
    );
    assert_eq!(value(&pool).await, 2);
    assert_eq!(business.close(WAIT).await, CloseOutcome::Complete);
}
