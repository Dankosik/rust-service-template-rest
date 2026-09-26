use std::{collections::BTreeMap, num::NonZeroU32};

use infra_postgres::{PgPool, TxError, in_tx};
use infra_webhooks::outbound::{Endpoint, Outbound, OutboundError};
use sqlx::Row;

const ENDPOINT: &str = "partner/a?#";

#[derive(Debug)]
enum Step {
    Delivery(OutboundError),
    Rejected,
    Transaction(TxError),
}

impl From<TxError> for Step {
    fn from(error: TxError) -> Self {
        Self::Transaction(error)
    }
}

fn explain(error: &Step) -> String {
    match error {
        Step::Delivery(error) => format!("delivery: {error}"),
        Step::Rejected => "caller rejected the transaction".to_owned(),
        Step::Transaction(error) => format!("transaction: {error}"),
    }
}

fn outbound() -> Outbound {
    outbound_with(
        "https://partner.example/events?source=template",
        "partner_v2",
        Some("partner_v1"),
    )
}

fn outbound_with(destination: &str, active_key: &str, previous_key: Option<&str>) -> Outbound {
    Outbound::new(
        BTreeMap::from([(
            ENDPOINT.to_owned(),
            Endpoint::new(
                destination.to_owned(),
                active_key.to_owned(),
                previous_key.map(str::to_owned),
            ),
        )]),
        NonZeroU32::new(1).expect("one worker"),
    )
    .expect("static HTTPS endpoint")
}

async fn job_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM background_jobs WHERE kind = 'webhooks.deliver'")
        .fetch_one(pool)
        .await
        .expect("delivery count")
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn producer_commit_durably_enqueues_one_unkeyed_delivery(pool: PgPool) {
    let prepared = outbound()
        .prepare(
            ENDPOINT,
            b"\0{\"event\":\"created\"}\xff".to_vec(),
            Some("application/webhook+json".to_owned()),
        )
        .expect("prepared delivery");
    let id = in_tx(&pool, async |tx| -> Result<_, Step> {
        prepared.enqueue(tx).await.map_err(Step::Delivery)
    })
    .await
    .expect("business transaction commits");
    let row = sqlx::query(
        "SELECT kind, state, unique_key IS NULL AS unkeyed, \
         payload->>'destination' AS destination, payload->>'active_key' AS active_key, \
         payload->>'previous_key' AS previous_key \
         FROM background_jobs WHERE id::text = $1",
    )
    .bind(id.to_string())
    .fetch_one(&pool)
    .await
    .expect("delivery row");
    assert_eq!(
        row.try_get::<String, _>("kind").expect("kind"),
        "webhooks.deliver"
    );
    assert_eq!(row.try_get::<String, _>("state").expect("state"), "pending");
    assert!(row.try_get::<bool, _>("unkeyed").expect("unkeyed"));
    assert_eq!(
        row.try_get::<String, _>("destination")
            .expect("destination"),
        "https://partner.example/events?source=template"
    );
    assert_eq!(
        row.try_get::<String, _>("active_key").expect("active key"),
        "partner_v2"
    );
    assert_eq!(
        row.try_get::<Option<String>, _>("previous_key")
            .expect("previous key")
            .as_deref(),
        Some("partner_v1")
    );
    let changed = outbound_with(
        "https://partner.example/changed",
        "partner_v3",
        Some("partner_v2"),
    );
    let _new_delivery = changed
        .prepare(ENDPOINT, b"{\"event\":\"new-config\"}".to_vec(), None)
        .expect("new configuration prepares separately");
    assert_eq!(job_count(&pool).await, 1);
    super::close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn caller_rollback_leaves_no_delivery(pool: PgPool) {
    let prepared = outbound()
        .prepare(ENDPOINT, b"{\"event\":\"rolled-back\"}".to_vec(), None)
        .expect("prepared delivery");
    let result = in_tx(&pool, async |tx| -> Result<(), Step> {
        let _id = prepared.enqueue(tx).await.map_err(Step::Delivery)?;
        Err(Step::Rejected)
    })
    .await;

    let explanation = result.as_ref().err().map(explain);
    assert!(matches!(result, Err(Step::Rejected)), "{explanation:?}");
    assert_eq!(job_count(&pool).await, 0);
    super::close(&[&pool]).await;
}
