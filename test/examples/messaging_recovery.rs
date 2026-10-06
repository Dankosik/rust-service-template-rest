//! Opt-in durable counter recipe. See docs/postgres-transactional-outbox.md.

#[path = "messaging_recovery/effect.rs"]
mod effect;

#[path = "messaging_recovery/capacity.rs"]
mod capacity;

use std::ffi::OsString;
use std::num::NonZeroU32;
use std::process::ExitCode;
use std::time::Duration;

use domain_events::Event;
use infra_messaging::{Registry, Route};
use infra_postgres::{Dsn, Isolation, PoolOptions, SessionBudgets, TxError, in_tx};
use sha2::{Digest as _, Sha256};
use time::format_description::well_known::Rfc3339;

type Error = jobs_worker::BuildError;

fn main() -> ExitCode {
    let mut args = std::env::args_os();
    let binary = args
        .next()
        .unwrap_or_else(|| OsString::from("messaging_recovery"));
    match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
        Some(role @ ("worker" | "publisher" | "consumer")) => {
            jobs_worker::run(std::iter::once(binary).chain(args), |registration| {
                if role != "consumer" {
                    integration_tests::jobs::register(registration)?;
                }
                if role == "publisher" {
                    return Ok(());
                }
                let subject = std::env::var("RECOVERY_SUBJECT")
                    .unwrap_or_else(|_| effect::DEFAULT_SUBJECT.to_owned());
                registration.messages = Registry::new([Route::new::<effect::Increment>(subject)])?;
                registration.with_postgres_messages(|pool, registry| {
                    effect::register(pool, registry)?;
                    Ok(())
                })
            })
        }
        Some(mode @ ("load" | "metrics" | "probe" | "audit")) => {
            match capacity::run(mode, args.collect()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => {
                    eprintln!("capacity command failed; retain its observation files");
                    ExitCode::FAILURE
                }
            }
        }
        Some("produce") => match produce(args.collect()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!(
                "usage: messaging_recovery worker|publisher|consumer [worker flags] | load <plan.json> <observations.jsonl> | metrics | probe | audit <output.jsonl> | produce <logical-id> <RFC3339 time> <counter-id> <delta> [subject]"
            );
            ExitCode::from(2)
        }
    }
}

fn produce(args: Vec<OsString>) -> Result<(), Error> {
    let args = args
        .into_iter()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "producer arguments must be UTF-8")?;
    if !(4..=5).contains(&args.len()) {
        return Err(
            "produce requires logical-id, RFC3339 time, counter-id, delta and optional subject"
                .into(),
        );
    }
    let event = Event {
        id: args[0].clone(),
        occurred_at: time::OffsetDateTime::parse(&args[1], &Rfc3339)
            .map_err(|_| "invalid event occurrence time")?
            .to_utc(),
        payload: effect::Increment {
            counter_id: args[2].clone(),
            delta: args[3]
                .parse()
                .map_err(|_| "delta must be a signed 64-bit integer")?,
        },
    };
    let subject = args.get(4).map_or(effect::DEFAULT_SUBJECT, String::as_str);
    let prepared =
        Registry::new([Route::new::<effect::Increment>(subject)])?.prepare(&event, 16_384)?;
    let dsn = Dsn::admit(&std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is required")?)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let pool = infra_postgres::connect(
            &dsn,
            &PoolOptions {
                max_connections: NonZeroU32::MIN,
                application_name: "messaging-recovery-producer",
                default_isolation: Isolation::ReadCommitted,
                session_budgets: SessionBudgets::Startup,
            },
        )
        .await?;
        let result = commit_event(&pool, &event, &prepared).await;
        let closed = tokio::time::timeout(Duration::from_secs(5), pool.close())
            .await
            .is_ok();
        match result {
            Err(error)
                if matches!(
                    error.downcast_ref::<TxError>(),
                    Some(TxError::CommitUnknown(_))
                ) =>
            {
                Err(
                    "producer COMMIT unresolved; retain the same event identity for reconciliation"
                        .into(),
                )
            }
            Err(_) => {
                Err("producer transaction failed; publication intent is not confirmed".into())
            }
            Ok(_) if !closed => Err("intent committed; producer pool close is incomplete".into()),
            Ok(outcome) => {
                println!("{outcome}");
                Ok(())
            }
        }
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

// Both producer entry points share the same business/receipt/outbox transaction.
async fn commit_event(
    pool: &infra_postgres::PgPool,
    event: &Event<effect::Increment>,
    prepared: &infra_messaging::PreparedEvent,
) -> Result<&'static str, Error> {
    in_tx(pool, async |tx| {
            let occurred_at = event.occurred_at.format(&Rfc3339)?;
            let digest = format!("{:x}", Sha256::digest(prepared.payload()));
            let inserted: Option<i32> = sqlx::query_scalar(
                "INSERT INTO recovery_producer_events \
                 (logical_id, event_type, schema_version, occurred_at, subject, payload_sha256) \
                 VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING RETURNING 1",
            )
            .bind(&event.id)
            .bind(prepared.event_type())
            .bind(i32::from(prepared.schema_version()))
            .bind(&occurred_at)
            .bind(prepared.subject())
            .bind(&digest)
            .fetch_optional(&mut *tx)
            .await?;
            if inserted.is_none() {
                let same: Option<bool> = sqlx::query_scalar(
                    "SELECT event_type = $2 AND schema_version = $3 AND occurred_at = $4 \
                     AND subject = $5 AND payload_sha256 = $6 FROM recovery_producer_events WHERE logical_id = $1",
                )
                .bind(&event.id)
                .bind(prepared.event_type())
                .bind(i32::from(prepared.schema_version()))
                .bind(&occurred_at)
                .bind(prepared.subject())
                .bind(&digest)
                .fetch_optional(&mut *tx)
                .await?;
                return if same == Some(true) {
                    // The original transaction established both business state
                    // and intent. Reconciliation never blindly enqueues again.
                    Ok::<_, Error>("producer_receipt_reconciled")
                } else {
                    Err("producer identity conflicts or is unresolved".into())
                };
            }
            sqlx::query(
                "INSERT INTO recovery_producer_counters (counter_id, value) VALUES ($1, $2) \
                 ON CONFLICT (counter_id) DO UPDATE SET value = recovery_producer_counters.value + EXCLUDED.value",
            )
            .bind(&event.payload.counter_id)
            .bind(event.payload.delta)
            .execute(&mut *tx)
            .await?;
            prepared
                .enqueue(tx)
                .await
                .map_err(|_| -> Error { "outbox intent was not established".into() })?;
            Ok::<_, Error>("intent_committed")
        })
        .await
}
