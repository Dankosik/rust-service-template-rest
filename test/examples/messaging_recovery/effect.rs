//! Example-owned receipt policy. No external effect can join this atomicity claim.

use domain_events::{Event, EventPayload};
use infra_messaging::{HandlerError, Registry, RegistryError};
use infra_postgres::{Isolation, PgPool, Tx, TxError, TxOptions, in_tx_with};
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use tokio_util::sync::CancellationToken;

pub(super) const CONSUMER_SCOPE: &str = "messaging-recovery-counter-v1";
pub(super) const DEFAULT_SUBJECT: &str = "recovery.counter.incremented";

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

#[derive(Debug)]
pub(super) enum EffectError {
    Conflict,
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
            Self::Transaction(TxError::CommitUnknown(_)) => "durable effect commit is unresolved",
            Self::Transaction(_) => "durable effect transaction failed",
        })
    }
}

impl std::error::Error for EffectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Query(error) => Some(error),
            Self::Transaction(error) => Some(error),
            Self::Conflict | Self::Unresolved => None,
        }
    }
}

impl From<sqlx::Error> for EffectError {
    fn from(error: sqlx::Error) -> Self {
        Self::Query(error)
    }
}

impl From<TxError> for EffectError {
    fn from(error: TxError) -> Self {
        Self::Transaction(error)
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

/// One attempt only. In particular, an unknown COMMIT never replays this closure.
pub(super) async fn attempt(
    pool: &PgPool,
    event: &Event<Increment>,
) -> Result<Applied, EffectError> {
    in_tx_with(
        pool,
        TxOptions {
            isolation: Isolation::ReadCommitted,
            read_only: false,
        },
        async |tx| apply(tx, event).await,
    )
    .await
}

/// Join the caller's READ COMMITTED transaction; the caller alone commits it.
///
/// Fixed bound SQL belongs to the optional example schema, not production migrations.
/// Arbitration must be an INSERT first: a plain read cannot settle a racing COMMIT.
pub(super) async fn apply(
    tx: &mut Tx<'_>,
    event: &Event<Increment>,
) -> Result<Applied, EffectError> {
    // Text keeps every nanosecond; PostgreSQL timestamptz would truncate precision.
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
    // A fresh statement sees the winner after unique-index arbitration waited for it.
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

/// A cancellation or database failure never acknowledges an unresolved effect.
pub(super) async fn handle(
    pool: &PgPool,
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

pub(super) fn register(pool: PgPool, registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register::<Increment, _, _>(move |event, cancel| {
        let pool = pool.clone();
        async move { handle(&pool, &event, &cancel).await }
    })
}
