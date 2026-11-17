use std::{
    fmt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use domain_events::{Event, EventPayload};
use infra_jobs::{
    EnqueueOptions, Enqueued, Job, JobError, JobKind, Policy, TransactionalHandler,
    TransactionalJob,
};
use infra_messaging::{PreparedEvent, PublicationRoute, Registry, RegistryError, Route};
use infra_postgres::{Dsn, Tx, TxError, in_tx};
use infra_webhooks::outbound::{JSON_CONTENT_TYPE, Producer};
use secrecy::ExposeSecret as _;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use time::{UtcDateTime, format_description::well_known::Rfc3339};

pub const SCHEMA: &str =
    include_str!("../fixtures/migrations/reading_counter/0001_reading_counter.sql");
pub const MAX_OPERATION_BYTES: usize = 1024;
pub const SUBJECT: &str = "reading.accepted";

pub fn publication(
    registry: &mut Registry,
    subject: &str,
) -> Result<PublicationRoute<Operation>, RegistryError> {
    registry.declare_publication(&Route::new(subject), MAX_OPERATION_BYTES)
}
pub const POOL_PRESSURE_DIR: &str = "READING_POOL_PRESSURE_DIR";
pub const LOCAL_CHECKPOINT_DIR: &str = "READING_LOCAL_CHECKPOINT_DIR";
pub const FAIL_OPERATION: &str = "READING_FAIL_OPERATION";

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
    const NAME: &'static str = "reading.record";
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
            id: format!("{}:{}", operation.scope, operation.operation_id),
            occurred_at: UtcDateTime::now(),
            payload: operation.clone(),
        };
        let event_time = event
            .occurred_at
            .format(&Rfc3339)
            .map_err(|_| Error::InvalidOperation)?;
        let event = publication
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
    Enqueue(infra_jobs::EnqueueError),
    Outbox(infra_messaging::outbox::OutboxEnqueueError),
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

pub async fn migrate(pool: &PgPool) -> Result<(), Error> {
    in_tx(pool, async |tx| {
        sqlx::raw_sql(SCHEMA).execute(&mut *tx).await?;
        Ok(())
    })
    .await
}

pub async fn accept(
    pool: &PgPool,
    prepared: &PreparedRequest,
    outbound: &Producer,
    endpoint: &str,
) -> Result<Accepted, Error> {
    in_tx(pool, async |tx| {
        accept_in_tx(tx, prepared, outbound, endpoint).await
    })
    .await
}

pub async fn accept_in_tx(
    tx: &mut Tx<'_>,
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
    tx: &mut Tx<'_>,
    prepared: &PreparedRequest,
    outbound: &Producer,
    endpoint: &str,
) -> Result<Accepted, Error> {
    let operation = &prepared.operation;
    let local_job_id = match infra_jobs::enqueue(tx, operation, EnqueueOptions::default())
        .await
        .map_err(Error::Enqueue)?
    {
        Enqueued::Created(id) => id.to_string(),
        Enqueued::Duplicate => return Err(Error::MissingIntent),
    };
    prepared.event.enqueue(tx).await.map_err(Error::Outbox)?;
    let outbox_job_id: String = sqlx::query_scalar("SELECT id::text FROM background_jobs WHERE kind='publish_domain_event' AND COALESCE(payload_text::jsonb, payload)->>'message_id'=$1 AND state IN ('pending','running')")
        .bind(prepared.event.message_id()).fetch_one(&mut *tx).await?;
    let webhook_job_id = outbound
        .enqueue(
            tx,
            endpoint,
            prepared.event.payload().to_vec(),
            JSON_CONTENT_TYPE,
        )
        .await
        .map_err(Error::Webhook)?
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

pub async fn replay(
    pool: &PgPool,
    operation: &Operation,
    outbound: &Producer,
    endpoint: &str,
) -> Result<Accepted, Error> {
    operation.validate()?;
    in_tx(pool, async |tx| {
        let row = sqlx::query("SELECT operation,event_id,event_time,event_subject,local_job_id::text,outbox_job_id::text,webhook_job_id::text FROM reading_requests WHERE scope=$1::uuid AND operation_id=$2::uuid FOR UPDATE")
            .bind(&operation.scope).bind(&operation.operation_id).fetch_optional(&mut *tx).await?.ok_or(Error::MissingIntent)?;
        let original = accepted_row(&row, operation)?;
        let remaining: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM background_jobs WHERE id IN ($1::uuid,$2::uuid,$3::uuid) OR (kind='publish_domain_event' AND COALESCE(payload_text::jsonb, payload)->>'message_id'=$4 AND state IN ('pending','running')))")
            .bind(&original.local_job_id).bind(&original.outbox_job_id).bind(&original.webhook_job_id).bind(&original.event_id).fetch_one(&mut *tx).await?;
        if remaining { return Err(Error::MissingIntent); }
        let occurred_at = UtcDateTime::parse(&original.event_time, &Rfc3339).map_err(|_| Error::InvalidOperation)?;
        let subject: String = row.try_get("event_subject")?;
        let event = Event { id: original.event_id, occurred_at, payload: operation.clone() };
        let publication = publication(&mut Registry::default(), &subject).map_err(|_| Error::InvalidOperation)?;
        let prepared = PreparedRequest { operation: operation.clone(), event: publication.prepare(&event).map_err(|_| Error::InvalidOperation)?, event_time: original.event_time };
        enqueue_intents(tx, &prepared, outbound, endpoint).await
    }).await
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

pub async fn read_request(pool: &PgPool, operation: &Operation) -> Result<Option<Accepted>, Error> {
    operation.validate()?;
    let row = sqlx::query("SELECT operation,event_id,event_time,local_job_id::text,outbox_job_id::text,webhook_job_id::text FROM reading_requests WHERE scope=$1::uuid AND operation_id=$2::uuid")
        .bind(&operation.scope).bind(&operation.operation_id).fetch_optional(pool).await?;
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
    pool: &PgPool,
    channel: Channel,
    operation: &Operation,
) -> Result<Effect, Error> {
    let result = in_tx(pool, async |tx| {
        apply_effect_in_tx(tx, channel, operation).await
    })
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
    tx: &mut Tx<'_>,
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
    pool: &PgPool,
    channel: Channel,
    operation: &Operation,
) -> Result<Option<Effect>, Error> {
    operation.validate()?;
    let row = sqlx::query("SELECT operation,read_count FROM reading_effects WHERE scope=$1::uuid AND channel=$2 AND operation_id=$3::uuid")
        .bind(&operation.scope).bind(channel.as_str()).bind(&operation.operation_id).fetch_optional(pool).await?;
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

#[derive(Default)]
struct Controls {
    pressure: Option<PathBuf>,
    checkpoint: Option<PathBuf>,
    fail_operation: Option<String>,
    pressure_released: AtomicBool,
    checkpoint_used: AtomicBool,
}

impl Controls {
    async fn hold_connections(&self, pool: &PgPool) -> Result<(), Error> {
        let Some(directory) = &self.pressure else {
            return Ok(());
        };
        let until = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut held = Vec::with_capacity(8);
        for _ in 0..8 {
            held.push(
                tokio::time::timeout_at(
                    until,
                    infra_postgres::acquire(pool, "reading fixture pressure"),
                )
                .await
                .map_err(|_| Error::Control)??,
            );
        }
        tokio::time::timeout_at(until, tokio::fs::write(directory.join("held"), b"8\n"))
            .await
            .map_err(|_| Error::Control)?
            .map_err(|_| Error::Control)?;
        wait_release(directory, until).await?;
        drop(held);
        self.pressure_released.store(true, Ordering::Release);
        Ok(())
    }

    async fn before_completion(&self) -> Result<(), Error> {
        let Some(directory) = &self.checkpoint else {
            return Ok(());
        };
        if self.checkpoint_used.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let until = tokio::time::Instant::now() + Duration::from_secs(5);
        tokio::time::timeout_at(
            until,
            tokio::fs::write(directory.join("reached"), b"uncommitted_effect\n"),
        )
        .await
        .map_err(|_| Error::Control)?
        .map_err(|_| Error::Control)?;
        wait_release(directory, until).await?;
        Ok(())
    }
}

async fn wait_release(
    directory: &std::path::Path,
    until: tokio::time::Instant,
) -> Result<(), Error> {
    while tokio::time::Instant::now() < until {
        match tokio::time::timeout_at(until, tokio::fs::try_exists(directory.join("release"))).await
        {
            Ok(Ok(true)) | Err(_) => return Ok(()),
            Ok(Ok(false)) => {}
            Ok(Err(_)) => return Err(Error::Control),
        }
        tokio::time::sleep_until(
            until.min(tokio::time::Instant::now() + Duration::from_millis(10)),
        )
        .await;
    }
    Ok(())
}

impl TransactionalHandler<Operation> for Arc<Controls> {
    async fn run(&self, job: TransactionalJob<Operation>, tx: &mut Tx<'_>) -> Result<(), JobError> {
        if self.pressure.is_some() && !self.pressure_released.load(Ordering::Acquire) {
            return Err(JobError::snooze(Duration::from_millis(100)).expect("valid fixture delay"));
        }
        if self.fail_operation.as_deref() == Some(job.payload().operation_id.as_str()) {
            return Err(JobError::permanent(
                "reading fixture selected permanent failure",
            ));
        }
        apply_effect_in_tx(tx, Channel::Local, job.payload())
            .await
            .map_err(|error| match error {
                Error::InvalidOperation => JobError::permanent("reading operation is invalid"),
                Error::Conflict => JobError::permanent("reading identity conflicts"),
                Error::Database(error) => error.into(),
                _ => JobError::retryable("reading effect failed"),
            })?;
        self.before_completion()
            .await
            .map_err(|_| JobError::retryable("reading fixture control failed"))
    }
}

#[derive(Serialize, Deserialize)]
struct HoldConnections;

impl JobKind for HoldConnections {
    const NAME: &'static str = "test.reading_hold_connections";
}

pub fn register(
    registration: &mut jobs_worker::Registration<'_>,
) -> Result<(), jobs_worker::BuildError> {
    let subject = std::env::var("READING_EVENT_SUBJECT").unwrap_or_else(|_| SUBJECT.to_owned());
    publication(&mut registration.messages, &subject)?;
    let controls = Arc::new(Controls {
        pressure: std::env::var_os(POOL_PRESSURE_DIR).map(PathBuf::from),
        checkpoint: std::env::var_os(LOCAL_CHECKPOINT_DIR).map(PathBuf::from),
        fail_operation: std::env::var(FAIL_OPERATION).ok(),
        ..Controls::default()
    });
    if controls.pressure.is_some() {
        let options = service_config::LoadOptions::try_parse_from(std::env::args_os().skip(1))?;
        let config = service_config::load_jobs_operator(&options)?;
        let postgres = config.postgres();
        let dsn = Dsn::admit_with(
            postgres.dsn.expose_secret(),
            postgres.password_file.as_deref(),
        )?;
        let holder = Arc::clone(&controls);
        registration
            .jobs
            .register(Policy::default(), move |job: Job<HoldConnections>| {
                let holder = Arc::clone(&holder);
                async move {
                    holder
                        .hold_connections(job.pool())
                        .await
                        .map_err(|_| JobError::retryable("reading fixture pressure failed"))
                }
            });
        registration.spawn("reading_pressure_request", move |cancel| async move {
            if matches!(
                cancel
                    .run_until_cancelled(Box::pin(enqueue_pressure(&dsn)))
                    .await,
                Some(Ok(()))
            ) {
                cancel.cancelled().await;
            }
        });
    }
    registration.jobs.register_transactional(
        Policy {
            max_attempts: 25,
            timeout: Duration::from_secs(30),
            max_running: None,
        },
        controls,
    );
    Ok(())
}

async fn enqueue_pressure(dsn: &Dsn) -> Result<(), Error> {
    let pool = infra_postgres::connect(
        dsn,
        &infra_postgres::PoolOptions {
            max_connections: std::num::NonZeroU32::MIN,
            application_name: "reading-pressure-producer",
            default_isolation: infra_postgres::Isolation::ServerDefault,
            session_settings_source: infra_postgres::SessionSettingsSource::Startup,
            wait_order: infra_postgres::WaitOrder::Arrival,
        },
    )
    .await
    .map_err(|_| Error::Control)?;
    let result = in_tx(&pool, async |tx| -> Result<(), Error> {
        let _ = infra_jobs::enqueue(tx, &HoldConnections, EnqueueOptions::default())
            .await
            .map_err(Error::Enqueue)?;
        Ok(())
    })
    .await;
    pool.close().await;
    result
}
