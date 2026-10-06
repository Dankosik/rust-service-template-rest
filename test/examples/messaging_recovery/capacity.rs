//! Bounded open-loop fixture ingress. This never publishes or consumes directly.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::num::NonZeroU32;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use domain_events::{Event, EventPayload};
use infra_messaging::{PreparedEvent, Registry, Route};
use infra_postgres::{Dsn, Isolation, PgPool, PoolOptions, SessionBudgets, TxError, in_tx};
use integration_tests::jobs::{Probe, ProbeAction};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, value::RawValue};
use sha2::{Digest as _, Sha256};
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::{Error, effect};

const PRODUCER_SLOTS: usize = 8;
const MAX_OFFERS: u32 = 7_500;
const MAX_SECONDS: u32 = 300;
const MAX_BODY_BYTES: u64 = 12 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    start_unix_ms: u64,
    stages: Vec<Stage>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stage {
    name: String,
    seconds: u32,
    rate: u32,
    payload_bytes: usize,
}

#[derive(Clone)]
struct Offer {
    id: String,
    phase: String,
    offset: Duration,
    payload_bytes: usize,
    index: u32,
    probe: bool,
}

#[derive(Serialize)]
#[serde(transparent)]
struct SizedIncrement(Box<RawValue>);

impl EventPayload for SizedIncrement {
    const EVENT_TYPE: &'static str = effect::Increment::EVENT_TYPE;
    const SCHEMA_VERSION: u16 = effect::Increment::SCHEMA_VERSION;
}

fn now_ms() -> Result<u64, Error> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

fn offers(plan: &Plan) -> Result<Vec<Offer>, Error> {
    if plan.stages.is_empty() || plan.stages.len() > 16 {
        return Err("invalid stage count".into());
    }
    let mut schedule = Vec::new();
    let mut elapsed = 0_u32;
    let mut count = 0_u32;
    let mut total_bytes = 0_u64;
    for stage in &plan.stages {
        if stage.name.is_empty()
            || stage.name.len() > 40
            || !stage
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || stage.seconds == 0
            || stage.seconds > MAX_SECONDS
            || stage.rate > 1_000
            || !matches!(stage.payload_bytes, 1_024 | 65_536)
        {
            return Err("stage is outside the bounded workload".into());
        }
        let stage_count = stage.seconds * stage.rate;
        count = count
            .checked_add(stage_count)
            .ok_or("offer count overflow")?;
        total_bytes += u64::from(stage_count) * u64::try_from(stage.payload_bytes)?;
        if count > MAX_OFFERS
            || total_bytes > MAX_BODY_BYTES
            || elapsed + stage.seconds > MAX_SECONDS
        {
            return Err("workload exceeds the fixture budget".into());
        }
        for index in 0..stage_count {
            let ordinal = count - stage_count + index;
            schedule.push(Offer {
                id: format!("capacity-{ordinal:06}"),
                phase: stage.name.clone(),
                offset: Duration::from_secs(u64::from(elapsed))
                    + Duration::from_nanos(
                        u64::from(index) * 1_000_000_000 / u64::from(stage.rate),
                    ),
                payload_bytes: stage.payload_bytes,
                index: ordinal,
                probe: false,
            });
        }
        for second in 0..stage.seconds {
            let ordinal = elapsed + second;
            schedule.push(Offer {
                id: format!("probe-{ordinal:06}"),
                phase: stage.name.clone(),
                offset: Duration::from_secs(u64::from(ordinal)),
                payload_bytes: 0,
                index: ordinal,
                probe: true,
            });
        }
        elapsed += stage.seconds;
    }
    schedule.sort_by_key(|offer| offer.offset);
    Ok(schedule)
}

fn prepared(
    offer: &Offer,
    occurred_at: time::UtcDateTime,
) -> Result<(Event<effect::Increment>, PreparedEvent), Error> {
    let event = Event {
        id: offer.id.clone(),
        occurred_at,
        payload: effect::Increment {
            counter_id: format!("counter-{:06}", offer.index),
            delta: 1,
        },
    };
    let compact = serde_json::to_string(&event.payload)?;
    let padding = offer
        .payload_bytes
        .checked_sub(compact.len())
        .ok_or("payload too small")?;
    // Whitespace inside the object is legal JSON, survives RawValue serialization,
    // and does not enlarge the indexed business key or add ignored schema fields.
    let body = format!("{{{}{rest}", " ".repeat(padding), rest = &compact[1..]);
    let meaning: effect::Increment = serde_json::from_str(&body)?;
    if meaning != event.payload {
        return Err("sized payload changed event meaning".into());
    }
    let wire_event = Event {
        id: event.id.clone(),
        occurred_at,
        payload: SizedIncrement(RawValue::from_string(body)?),
    };
    let publication = Registry::new([Route::new::<SizedIncrement>(effect::DEFAULT_SUBJECT)])?
        .prepare(&wire_event, 65_536)?;
    if publication.payload().len() != offer.payload_bytes {
        return Err("serializer changed the declared payload size".into());
    }
    Ok((event, publication))
}

async fn pool() -> Result<PgPool, Error> {
    let dsn = Dsn::admit(&std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is required")?)?;
    Ok(infra_postgres::connect(
        &dsn,
        &PoolOptions {
            max_connections: NonZeroU32::new(8).ok_or("invalid fixture pool limit")?,
            application_name: "messaging-recovery-load",
            default_isolation: Isolation::ReadCommitted,
            session_budgets: SessionBudgets::Startup,
        },
    )
    .await?)
}

async fn probe(pool: &PgPool) -> Result<String, Error> {
    in_tx(pool, async |tx| {
        match infra_jobs::enqueue(
            tx,
            &Probe {
                action: ProbeAction::Succeed,
            },
            infra_jobs::EnqueueOptions::default(),
        )
        .await?
        {
            infra_jobs::Enqueued::Created(id) => Ok(id.to_string()),
            infra_jobs::Enqueued::Duplicate => Err("unexpected ordinary probe duplicate".into()),
        }
    })
    .await
}

fn record(output: &mut File, value: &Value) -> Result<(), Error> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn observation(offer: &Offer, start: u64, outcome: &str) -> Value {
    json!({"id": offer.id, "phase": offer.phase, "kind": if offer.probe { "probe" } else { "event" },
           "scheduled_unix_ms": start + u64::try_from(offer.offset.as_millis()).unwrap_or(u64::MAX),
           "payload_bytes": offer.payload_bytes, "outcome": outcome})
}

async fn submit(pool: PgPool, offer: Offer, start: u64, occurred_at: time::UtcDateTime) -> Value {
    let mut row = observation(&offer, start, "not_admitted");
    let started = Instant::now();
    row["started_unix_ms"] = json!(now_ms().ok());
    let result = if offer.probe {
        probe(&pool).await.map(|id| {
            row["job_id"] = json!(id);
            "intent_committed"
        })
    } else {
        match prepared(&offer, occurred_at) {
            Ok((event, prepared)) => {
                row["payload_sha256"] = json!(format!("{:x}", Sha256::digest(prepared.payload())));
                super::commit_event(&pool, &event, &prepared).await
            }
            Err(error) => Err(error),
        }
    };
    row["transaction_ms"] = json!(started.elapsed().as_secs_f64() * 1_000.0);
    row["finished_unix_ms"] = json!(now_ms().ok());
    row["outcome"] = json!(match result {
        Ok(value) => value,
        Err(error)
            if matches!(
                error.downcast_ref::<TxError>(),
                Some(TxError::CommitUnknown(_))
            ) =>
            "commit_unknown",
        Err(_) => "not_admitted",
    });
    row
}

async fn load(plan_path: &Path, output_path: &Path) -> Result<(), Error> {
    if plan_path.metadata()?.len() > 16_384 {
        return Err("plan too large".into());
    }
    let plan: Plan = serde_json::from_slice(&std::fs::read(plan_path)?)?;
    let schedule = offers(&plan)?;
    let now = now_ms()?;
    if plan.start_unix_ms < now || plan.start_unix_ms - now > 30_000 {
        return Err("start must be within the next thirty seconds".into());
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_path)?;
    let pool = pool().await?;
    let start =
        Instant::now() + Duration::from_millis(plan.start_unix_ms.saturating_sub(now_ms()?));
    let occurred_at = time::OffsetDateTime::from_unix_timestamp_nanos(
        i128::from(plan.start_unix_ms) * 1_000_000,
    )?
    .to_utc();
    let mut running = JoinSet::new();
    for offer in schedule {
        let scheduled = start + offer.offset;
        loop {
            tokio::select! {
                biased;
                result = running.join_next(), if !running.is_empty() => {
                    record(&mut output, &result.ok_or("missing producer task")??)?;
                }
                () = tokio::time::sleep_until(scheduled) => break,
            }
        }
        let rejection =
            if Instant::now().saturating_duration_since(scheduled) > Duration::from_millis(100) {
                Some("missed_schedule")
            } else if running.len() >= PRODUCER_SLOTS {
                Some("generator_busy")
            } else {
                None
            };
        if let Some(reason) = rejection {
            let mut row = observation(&offer, plan.start_unix_ms, reason);
            row["finished_unix_ms"] = json!(now_ms()?);
            record(&mut output, &row)?;
        } else {
            running.spawn(submit(pool.clone(), offer, plan.start_unix_ms, occurred_at));
        }
    }
    while let Some(result) = running.join_next().await {
        record(&mut output, &result?)?;
    }
    tokio::time::timeout(Duration::from_secs(5), pool.close()).await?;
    record(
        &mut output,
        &json!({"kind": "finished", "producer_slots": PRODUCER_SLOTS,
                              "finished_unix_ms": now_ms()?}),
    )?;
    Ok(())
}

// Native read-only requests through one finite client; no delivery consumer or replay.
async fn audit(output_path: &Path) -> Result<(), Error> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_path)?;
    let servers = std::env::var("APP__MESSAGING__URLS")?;
    let client = async_nats::ConnectOptions::with_credentials_file("/session/admin.creds")
        .await?
        .add_root_certificates("/session/tls/ca.crt".into())
        .require_tls(true)
        .request_timeout(Some(Duration::from_secs(3)))
        .connect(servers.split(',').collect::<Vec<_>>())
        .await?;
    let result = tokio::time::timeout(Duration::from_secs(120), async {
        let info = client.request("$JS.API.STREAM.INFO.RECOVERY", "{}".into()).await?;
        let info: Value = serde_json::from_slice(&info.payload)?;
        let first = info["state"]["first_seq"].as_u64().ok_or("missing stream sequence")?;
        let last = info["state"]["last_seq"].as_u64().ok_or("missing stream sequence")?;
        if last.saturating_sub(first) > u64::from(MAX_OFFERS) {
            return Err::<(), Error>("source scan exceeds the workload bound".into());
        }
        for sequence in first..=last {
            let request = serde_json::to_vec(&json!({"seq": sequence}))?;
            let response = client.request("$JS.API.STREAM.MSG.GET.RECOVERY", request.into()).await?;
            let response: Value = serde_json::from_slice(&response.payload)?;
            let message = &response["message"];
            let payload = STANDARD.decode(message["data"].as_str().ok_or("missing raw body")?)?;
            let headers = STANDARD.decode(message["hdrs"].as_str().ok_or("missing raw headers")?)?;
            let meaning: effect::Increment = serde_json::from_slice(&payload)?;
            record(&mut output, &json!({"seq": sequence, "subject": message["subject"],
                "hdrs": message["hdrs"], "header_bytes": headers.len(), "payload_bytes": payload.len(),
                "payload_sha256": format!("{:x}", Sha256::digest(&payload)), "meaning": meaning}))?;
        }
        Ok(())
    }).await;
    client.drain().await?;
    result??;
    Ok(())
}

pub(super) fn run(mode: &str, args: Vec<OsString>) -> Result<(), Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        match mode {
            "audit" if args.len() == 1 => audit(Path::new(&args[0])).await,
            "load" if args.len() == 2 => load(Path::new(&args[0]), Path::new(&args[1])).await,
            "metrics" if args.is_empty() => {
                let response = reqwest::Client::builder()
                    .timeout(Duration::from_secs(3))
                    .build()?
                    .get("http://127.0.0.1:9080/metrics")
                    .send()
                    .await?
                    .error_for_status()?
                    .bytes()
                    .await?;
                if response.len() > 1024 * 1024 {
                    return Err("metrics output bound".into());
                }
                std::io::stdout().write_all(&response)?;
                Ok(())
            }
            "probe" if args.is_empty() => {
                let pool = pool().await?;
                let result = probe(&pool).await;
                tokio::time::timeout(Duration::from_secs(5), pool.close()).await?;
                println!("{}", result?);
                Ok(())
            }
            _ => Err("invalid capacity command arguments".into()),
        }
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "fixture assertions have concrete expected values"
)]
mod tests {
    use super::*;

    #[test]
    fn sized_publication_retains_the_strict_effect_meaning_and_exact_bytes() {
        for size in [1_024, 65_536] {
            let offer = Offer {
                id: "same-event".into(),
                phase: "slice".into(),
                offset: Duration::ZERO,
                payload_bytes: size,
                index: 17,
                probe: false,
            };
            let time = time::OffsetDateTime::from_unix_timestamp(1_700_000_000)
                .unwrap()
                .to_utc();
            let (event, first) = prepared(&offer, time).unwrap();
            let (_, retry) = prepared(&offer, time).unwrap();
            let decoded: effect::Increment = serde_json::from_slice(first.payload()).unwrap();
            assert_eq!(first.payload().len(), size);
            assert_eq!(
                decoded,
                effect::Increment {
                    counter_id: "counter-000017".into(),
                    delta: 1
                }
            );
            assert_eq!(decoded, event.payload);
            assert_eq!(first.payload(), retry.payload());
            assert_eq!(first.publication_id(), "same-event");
        }
    }

    #[test]
    fn schedule_has_fixed_offers_and_refuses_more_than_its_resource_ceiling() {
        let mut plan = Plan {
            start_unix_ms: 0,
            stages: vec![Stage {
                name: "fixed".into(),
                seconds: 2,
                rate: 3,
                payload_bytes: 1_024,
            }],
        };
        let schedule = offers(&plan).unwrap();
        assert_eq!(schedule.iter().filter(|offer| !offer.probe).count(), 6);
        assert_eq!(schedule.iter().filter(|offer| offer.probe).count(), 2);
        assert_eq!(
            schedule
                .iter()
                .filter(|offer| !offer.probe)
                .last()
                .unwrap()
                .offset,
            Duration::from_nanos(1_666_666_666)
        );
        plan.stages[0].rate = 1_000;
        plan.stages[0].seconds = 8;
        assert!(offers(&plan).is_err());
        plan.stages[0].seconds = 1;
        plan.stages[0].payload_bytes = 65_536;
        assert!(offers(&plan).is_err());
    }
}
