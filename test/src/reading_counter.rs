use std::fmt;

use domain_events::{Event, EventPayload};
use infra_jobs::{JobKind, KindName};
use infra_messaging::{
    PreparedEvent, PublicationPolicy, PublicationRoute, Registry, RegistryError, Route,
};
use infra_postgres::{Pool, Tx, TxError, TxMode};
use infra_webhooks::outbound::{JSON_CONTENT_TYPE, Producer};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::{UtcDateTime, format_description::well_known::Rfc3339};

pub const SCHEMA: &str =
    include_str!("../fixtures/migrations/reading_counter/0001_reading_counter.sql");
pub const MAX_OPERATION_BYTES: usize = 1024;
const PUBLICATIONS: PublicationPolicy = PublicationPolicy {
    max_payload_bytes: MAX_OPERATION_BYTES,
};
pub const SUBJECT: &str = "reading.accepted";
const PUBLISHER_ID: uuid::NonNilUuid = uuid::NonNilUuid::new(uuid::Uuid::from_u128(
    0x2a42fbe7_9da4_48ba_a6e9_84447b8d6310,
))
.unwrap();

pub fn publication(
    registry: &mut Registry,
    subject: &str,
) -> Result<PublicationRoute<Operation>, RegistryError> {
    PUBLICATIONS.declare(registry, &Route::new(subject))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub scope: String,
    pub operation_id: String,
    pub article_id: String,
    pub content_version: u32,
    pub content: String,
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().all(|byte| !byte.is_ascii_uppercase())
        && uuid::Uuid::try_parse(value).is_ok()
}

impl Operation {
    fn local_event_id(&self) -> String {
        format!("{}:{}", self.scope, self.operation_id)
    }

    pub fn validate(&self) -> Result<(), Error> {
        for id in [&self.scope, &self.operation_id, &self.article_id] {
            if !valid_uuid(id) {
                return Err(Error::InvalidOperation);
            }
        }
        if self.content_version == 0
            || self.content.len() > MAX_OPERATION_BYTES
            || self.content.contains('\0')
        {
            return Err(Error::InvalidOperation);
        }
        if serde_json::to_vec(self)?.len() > MAX_OPERATION_BYTES {
            return Err(Error::InvalidOperation);
        }
        Ok(())
    }
}

impl JobKind for Operation {
    const NAME: KindName = KindName::new("reading.record");
}
impl EventPayload for Operation {
    const EVENT_TYPE: &'static str = "reading.accepted";
    const SCHEMA_VERSION: u16 = 1;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Local,
    Outbox,
    Webhook,
}
impl Channel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Outbox => "outbox",
            Self::Webhook => "webhook",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
    pub operation: Operation,
    pub channel: Channel,
    pub read_count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub operation: Operation,
    pub event_id: String,
    pub event_time: String,
    pub local_job_id: String,
    pub outbox_job_id: String,
    pub webhook_job_id: String,
}

#[derive(Clone, Debug)]
pub struct PreparedRequest {
    operation: Operation,
    event: PreparedEvent,
    event_time: String,
}
impl PreparedRequest {
    pub fn try_new(
        operation: Operation,
        publication: &PublicationRoute<Operation>,
    ) -> Result<Self, Error> {
        operation.validate()?;
        let event = Event {
            id: operation.local_event_id(),
            occurred_at: UtcDateTime::now(),
            payload: operation.clone(),
        };
        let event_time = event
            .occurred_at
            .format(&Rfc3339)
            .map_err(|_| Error::InvalidOperation)?;
        let event = publication
            .clone()
            .for_outbox(PUBLISHER_ID)
            .prepare(&event)
            .map_err(|_| Error::InvalidOperation)?;
        Ok(Self {
            operation,
            event,
            event_time,
        })
    }
    #[must_use]
    pub const fn operation(&self) -> &Operation {
        &self.operation
    }
}

#[derive(Debug, derive_more::From)]
pub enum Error {
    InvalidOperation,
    Conflict,
    MissingIntent,
    Control,
    #[from]
    Database(sqlx::Error),
    #[from]
    Transaction(TxError),
    #[from]
    Json(serde_json::Error),
    #[from]
    Enqueue(infra_jobs::EnqueueError),
    #[from]
    Outbox(infra_messaging::outbox::OutboxEnqueueError),
    #[from]
    Webhook(infra_webhooks::outbound::OutboundError),
}
impl Error {
    #[must_use]
    pub const fn commit_unknown(&self) -> bool {
        matches!(self, Self::Transaction(TxError::CommitUnknown(_)))
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidOperation => "reading operation is invalid",
            Self::Conflict => "reading identity conflicts",
            Self::MissingIntent => "reading acceptance has no established intent",
            Self::Control => "reading fixture control failed",
            Self::Transaction(TxError::CommitUnknown(_)) => "reading commit outcome is unknown",
            Self::Database(_) | Self::Transaction(_) => "reading database operation failed",
            Self::Json(_) => "reading encoding failed",
            Self::Enqueue(_) | Self::Outbox(_) | Self::Webhook(_) => {
                "reading intent enqueue failed"
            }
        })
    }
}
impl std::error::Error for Error {}

pub async fn migrate(pool: &Pool) -> Result<(), Error> {
    pool.in_tx(async |tx| {
        sqlx::raw_sql(SCHEMA).execute(&mut *tx).await?;
        Ok(())
    })
    .await
}

pub async fn accept(
    pool: &Pool,
    prepared: &PreparedRequest,
    outbound: &Producer,
    endpoint: &str,
) -> Result<Accepted, Error> {
    pool.in_tx(async |tx| accept_in_tx(tx, prepared, outbound, endpoint).await)
        .await
}

pub async fn accept_in_tx(
    tx: &mut Tx<'_, impl TxMode>,
    prepared: &PreparedRequest,
    outbound: &Producer,
    endpoint: &str,
) -> Result<Accepted, Error> {
    let operation = &prepared.operation;
    let encoded = serde_json::to_string(operation)?;
    let inserted = sqlx::query("INSERT INTO reading_requests (scope,operation_id,operation,event_id,event_time,event_subject) VALUES ($1::uuid,$2::uuid,$3,$4,$5,$6) ON CONFLICT DO NOTHING")
        .bind(&operation.scope).bind(&operation.operation_id).bind(&encoded).bind(prepared.event.message_id()).bind(&prepared.event_time).bind(prepared.event.subject())
        .execute(&mut *tx).await?.rows_affected();
    if inserted == 0 {
        let row = sqlx::query("SELECT operation,event_id,event_time,local_job_id::text,outbox_job_id::text,webhook_job_id::text FROM reading_requests WHERE scope=$1::uuid AND operation_id=$2::uuid")
            .bind(&operation.scope).bind(&operation.operation_id).fetch_one(&mut *tx).await?;
        return accepted_row(&row, operation);
    }
    let accepted = enqueue_intents(tx, prepared, outbound, endpoint).await?;
    sqlx::query("UPDATE reading_requests SET local_job_id=$3::uuid,outbox_job_id=$4::uuid,webhook_job_id=$5::uuid WHERE scope=$1::uuid AND operation_id=$2::uuid")
        .bind(&operation.scope).bind(&operation.operation_id).bind(&accepted.local_job_id).bind(&accepted.outbox_job_id).bind(&accepted.webhook_job_id).execute(&mut *tx).await?;
    Ok(accepted)
}

async fn enqueue_intents(
    tx: &mut Tx<'_, impl TxMode>,
    prepared: &PreparedRequest,
    outbound: &Producer,
    endpoint: &str,
) -> Result<Accepted, Error> {
    let operation = &prepared.operation;
    let local_job_id = infra_jobs::enqueue(tx, operation).await?.to_string();
    let _ = prepared.event.enqueue(tx).await?;
    let outbox_job_id: String = sqlx::query_scalar("SELECT id::text FROM background_jobs WHERE kind='publish_domain_event' AND COALESCE(payload_text::jsonb, payload)->>'message_id'=$1 AND state IN ('pending','running')")
        .bind(prepared.event.message_id()).fetch_one(&mut *tx).await?;
    let webhook_job_id = outbound
        .enqueue(
            tx,
            endpoint,
            prepared.event.payload().clone(),
            JSON_CONTENT_TYPE,
        )
        .await?
        .to_string();
    Ok(Accepted {
        operation: operation.clone(),
        event_id: prepared.event.message_id().to_owned(),
        event_time: prepared.event_time.clone(),
        local_job_id,
        outbox_job_id,
        webhook_job_id,
    })
}

fn accepted_row(row: &sqlx::postgres::PgRow, operation: &Operation) -> Result<Accepted, Error> {
    same_operation(row.try_get("operation")?, operation)?;
    Ok(Accepted {
        operation: operation.clone(),
        event_id: row.try_get("event_id")?,
        event_time: row.try_get("event_time")?,
        local_job_id: row
            .try_get::<Option<String>, _>("local_job_id")?
            .ok_or(Error::MissingIntent)?,
        outbox_job_id: row
            .try_get::<Option<String>, _>("outbox_job_id")?
            .ok_or(Error::MissingIntent)?,
        webhook_job_id: row
            .try_get::<Option<String>, _>("webhook_job_id")?
            .ok_or(Error::MissingIntent)?,
    })
}

pub async fn read_request(pool: &Pool, operation: &Operation) -> Result<Option<Accepted>, Error> {
    operation.validate()?;
    let mut conn = pool.acquire("read reading request").await?;
    let row = sqlx::query("SELECT operation,event_id,event_time,local_job_id::text,outbox_job_id::text,webhook_job_id::text FROM reading_requests WHERE scope=$1::uuid AND operation_id=$2::uuid")
        .bind(&operation.scope).bind(&operation.operation_id).fetch_optional(&mut *conn).await?;
    row.as_ref()
        .map(|row| accepted_row(row, operation))
        .transpose()
}

fn same_operation(encoded: &str, operation: &Operation) -> Result<(), Error> {
    if serde_json::from_str::<Operation>(encoded)? == *operation {
        Ok(())
    } else {
        Err(Error::Conflict)
    }
}

pub async fn apply_effect(
    pool: &Pool,
    channel: Channel,
    operation: &Operation,
) -> Result<Effect, Error> {
    let result = pool
        .in_tx(async |tx| apply_effect_in_tx(tx, channel, operation).await)
        .await;
    match result {
        Err(error)
            if error.commit_unknown()
                && let Ok(Some(effect)) = read_effect(pool, channel, operation).await =>
        {
            Ok(effect)
        }
        result => result,
    }
}

pub async fn apply_effect_in_tx(
    tx: &mut Tx<'_, impl TxMode>,
    channel: Channel,
    operation: &Operation,
) -> Result<Effect, Error> {
    operation.validate()?;
    let encoded = serde_json::to_string(operation)?;
    let inserted = sqlx::query("INSERT INTO reading_effects (scope,channel,operation_id,operation,read_count) VALUES ($1::uuid,$2,$3::uuid,$4,0) ON CONFLICT DO NOTHING")
        .bind(&operation.scope).bind(channel.as_str()).bind(&operation.operation_id).bind(encoded).execute(&mut *tx).await?.rows_affected();
    if inserted == 0 {
        let row = sqlx::query("SELECT operation,read_count FROM reading_effects WHERE scope=$1::uuid AND channel=$2 AND operation_id=$3::uuid")
            .bind(&operation.scope).bind(channel.as_str()).bind(&operation.operation_id).fetch_one(&mut *tx).await?;
        same_operation(row.try_get("operation")?, operation)?;
        return Ok(Effect {
            operation: operation.clone(),
            channel,
            read_count: row.try_get("read_count")?,
        });
    }
    let read_count: i64 = sqlx::query_scalar("INSERT INTO reading_articles (scope,channel,article_id,read_count) VALUES ($1::uuid,$2,$3::uuid,1) ON CONFLICT (scope,channel,article_id) DO UPDATE SET read_count=reading_articles.read_count+1 RETURNING read_count")
        .bind(&operation.scope).bind(channel.as_str()).bind(&operation.article_id).fetch_one(&mut *tx).await?;
    sqlx::query("UPDATE reading_effects SET read_count=$4 WHERE scope=$1::uuid AND channel=$2 AND operation_id=$3::uuid")
        .bind(&operation.scope).bind(channel.as_str()).bind(&operation.operation_id).bind(read_count).execute(&mut *tx).await?;
    Ok(Effect {
        operation: operation.clone(),
        channel,
        read_count,
    })
}

pub async fn read_effect(
    pool: &Pool,
    channel: Channel,
    operation: &Operation,
) -> Result<Option<Effect>, Error> {
    operation.validate()?;
    let mut conn = pool.acquire("read reading effect").await?;
    let row = sqlx::query("SELECT operation,read_count FROM reading_effects WHERE scope=$1::uuid AND channel=$2 AND operation_id=$3::uuid")
        .bind(&operation.scope).bind(channel.as_str()).bind(&operation.operation_id).fetch_optional(&mut *conn).await?;
    row.map(|row| {
        same_operation(row.try_get("operation")?, operation)?;
        Ok(Effect {
            operation: operation.clone(),
            channel,
            read_count: row.try_get("read_count")?,
        })
    })
    .transpose()
}
