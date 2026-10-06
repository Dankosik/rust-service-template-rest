//! Test-only reading reference CLI; worker mode enters the actual worker runner.

use std::{
    collections::BTreeMap, ffi::OsString, num::NonZeroU32, process::ExitCode, time::Duration,
};

use infra_postgres::{Dsn, Isolation, PoolOptions, SessionBudgets};
use infra_webhooks::{
    outbound::{Dispatcher, Endpoint, Outbound},
    protocol::KeyRing,
};
use integration_tests::{
    reading_counter::{self, Channel, Error, Operation, PreparedRequest},
    reading_counter_receiver::{self, CompletionHold, ReceiverError, ReceiverOptions},
};
use serde_json::{Value, json};
use sqlx::PgPool;

const ENDPOINT: &str = "reading-counter";
const FIXTURE_KEY: &str = "whsec_QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE=";

fn register(
    registration: &mut jobs_worker::Registration<'_>,
) -> Result<(), jobs_worker::BuildError> {
    reading_counter::register(registration)?;
    let destination = url::Url::parse(&std::env::var("READING_WEBHOOK_URL")?)?;
    if destination.scheme() != "http"
        || destination
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_none_or(|address| !address.is_loopback())
        || destination.path() != "/reading"
    {
        return Err("READING_WEBHOOK_URL requires loopback HTTP /reading".into());
    }
    let client = infra_outbound_http::Client::new_for_test_http(
        &destination,
        infra_outbound_http::Limits {
            operation_timeout: infra_webhooks::outbound::DELIVERY_POLICY.timeout,
            response_header_count: 64,
            response_body_bytes: 1024,
        },
    )?;
    let endpoint = Endpoint::with_client(
        destination,
        client,
        KeyRing::from_encoded(FIXTURE_KEY, None)?,
    )?;
    Dispatcher::new(BTreeMap::from([(ENDPOINT.to_owned(), endpoint)]))
        .register(&mut registration.jobs, None);
    Ok(())
}

struct Arguments {
    mode: String,
    flags: BTreeMap<String, String>,
}

impl Arguments {
    fn parse(args: impl Iterator<Item = OsString>) -> Result<Self, ReceiverError> {
        let mut args = args.map(|value| value.into_string().map_err(|_| "arguments must be UTF-8"));
        let mode = args.next().ok_or("a fixture mode is required")??;
        let mut flags = BTreeMap::new();
        while let Some(flag) = args.next() {
            let flag = flag?;
            if !flag.starts_with("--") || flags.contains_key(&flag) {
                return Err("flags require unique --name value pairs".into());
            }
            let value = args.next().ok_or("flag requires a value")??;
            flags.insert(flag, value);
        }
        Ok(Self { mode, flags })
    }

    fn take(&mut self, flag: &str) -> Result<String, ReceiverError> {
        self.flags
            .remove(flag)
            .ok_or_else(|| format!("missing {flag}").into())
    }

    fn optional(&mut self, flag: &str, default: &str) -> String {
        self.flags
            .remove(flag)
            .unwrap_or_else(|| default.to_owned())
    }

    fn complete(&self) -> Result<(), ReceiverError> {
        if self.flags.is_empty() {
            Ok(())
        } else {
            Err("unknown fixture flag".into())
        }
    }
}

fn channel(value: &str) -> Result<Channel, ReceiverError> {
    match value {
        "local" => Ok(Channel::Local),
        "outbox" => Ok(Channel::Outbox),
        "webhook" => Ok(Channel::Webhook),
        _ => Err("channel must be local, outbox, or webhook".into()),
    }
}

fn operation(raw: &str) -> Result<Operation, ReceiverError> {
    if raw.len() > 1024 {
        return Err("operation exceeds one KiB".into());
    }
    let value: Operation = serde_json::from_str(raw)?;
    value.validate()?;
    Ok(value)
}

async fn open_pool() -> Result<PgPool, ReceiverError> {
    let dsn = Dsn::admit(&std::env::var("DATABASE_URL")?)?;
    Ok(infra_postgres::connect(
        &dsn,
        &PoolOptions {
            max_connections: NonZeroU32::new(2).ok_or("invalid fixture capacity")?,
            application_name: "reading-counter-fixture",
            default_isolation: Isolation::ServerDefault,
            session_budgets: SessionBudgets::Startup,
        },
    )
    .await?)
}

#[allow(
    clippy::print_stdout,
    reason = "JSON lines are the fixture protocol consumed by the process driver"
)]
fn emit(value: Value) {
    println!("{value}");
}

fn error_receipt(error: &Error) -> Value {
    json!({"status": if matches!(error, Error::Conflict) { "conflict" } else { "unknown" },
        "error": error.to_string()})
}

async fn accept_one(
    pool: &PgPool,
    operation: Operation,
    subject: &str,
    endpoint: &str,
) -> Result<bool, ReceiverError> {
    let prepared = PreparedRequest::new(operation, subject)?;
    let outbound = Outbound::new([endpoint.to_owned()]);
    match reading_counter::accept(pool, &prepared, &outbound, endpoint).await {
        Ok(accepted) => {
            emit(json!({"status":"accepted", "accepted":accepted}));
            Ok(true)
        }
        Err(error) if error.commit_unknown() => {
            match reading_counter::read_request(pool, prepared.operation()).await {
                Ok(Some(accepted)) => {
                    emit(json!({"status":"accepted","accepted":accepted,"commit_reconciled":true}));
                    Ok(true)
                }
                _ => {
                    emit(json!({"status":"unknown","operation":prepared.operation()}));
                    Ok(false)
                }
            }
        }
        Err(error) => {
            emit(error_receipt(&error));
            Ok(false)
        }
    }
}

async fn accept(pool: &PgPool, args: &mut Arguments) -> Result<bool, ReceiverError> {
    let subject = args.optional("--subject", "reading.accepted");
    let endpoint = args.optional("--endpoint", ENDPOINT);
    let operations = if let Some(path) = args.flags.remove("--operations-file") {
        // The fixed batch bound applies before deserializing or opening a Tx.
        let file = tokio::fs::File::open(path).await?;
        if file.metadata().await?.len() > 128 * 1024 + 256 {
            return Err("batch exceeds size bound".into());
        }
        let mut bytes = Vec::new();
        use tokio::io::AsyncReadExt;
        file.take(128 * 1024 + 257).read_to_end(&mut bytes).await?;
        let values: Vec<Operation> = serde_json::from_slice(&bytes)?;
        if values.is_empty() || values.len() > 128 {
            return Err("batch requires 1..=128 operations".into());
        }
        for value in &values {
            value.validate()?;
        }
        values
    } else {
        vec![operation(&args.take("--operation-json")?)?]
    };
    args.complete()?;
    for operation in operations {
        if !accept_one(pool, operation, &subject, &endpoint).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn read(pool: &PgPool, args: &mut Arguments) -> Result<bool, ReceiverError> {
    let operation = operation(&args.take("--operation-json")?)?;
    let selected = args.optional("--channel", "local");
    args.complete()?;
    if selected == "request" {
        match reading_counter::read_request(pool, &operation).await {
            Ok(value) => emit(
                json!({"status":if value.is_some() {"present"} else {"absent"},"accepted":value}),
            ),
            Err(error) => {
                emit(error_receipt(&error));
                return Ok(false);
            }
        }
    } else {
        match reading_counter::read_effect(pool, channel(&selected)?, &operation).await {
            Ok(value) => emit(
                json!({"status":if value.is_some() {"present"} else {"absent"},"effect":value}),
            ),
            Err(error) => {
                emit(error_receipt(&error));
                return Ok(false);
            }
        }
    }
    Ok(true)
}

async fn receiver(pool: &PgPool, args: &mut Arguments) -> Result<(), ReceiverError> {
    let hold = if let Some(selected) = args.flags.remove("--hold-channel") {
        let channel = channel(&selected)?;
        if channel == Channel::Local {
            return Err("receiver hold channel must be outbox or webhook".into());
        }
        Some(CompletionHold {
            channel,
            operation_id: args.take("--hold-operation")?,
            ready: args.take("--fault-ready")?.into(),
            release: args.take("--fault-release")?.into(),
        })
    } else {
        None
    };
    let options = ReceiverOptions {
        nats_url: args.take("--nats-url")?,
        stream: args.take("--stream")?,
        subject: args.take("--subject")?,
        consumer: args.take("--consumer")?,
        http_bind: args.take("--http-bind")?.parse()?,
        run_for: Duration::from_secs(args.optional("--run-seconds", "250").parse()?),
        hold,
    };
    args.complete()?;
    reading_counter_receiver::run(pool, options).await
}

async fn broker(args: &mut Arguments) -> Result<bool, ReceiverError> {
    let url = args.take("--nats-url")?;
    let name = args.take("--stream")?;
    let subject = if args.mode == "broker-init" {
        Some(args.take("--subject")?)
    } else {
        None
    };
    args.complete()?;
    let client = async_nats::connect(url).await?;
    let jetstream = async_nats::jetstream::new(client.clone());
    if let Some(subject) = subject {
        jetstream
            .create_stream(async_nats::jetstream::stream::Config {
                name: name.clone(),
                subjects: vec![subject],
                storage: async_nats::jetstream::stream::StorageType::File,
                duplicate_window: Duration::from_secs(1),
                max_message_size: 1024 + 8 * 1024,
                discard: async_nats::jetstream::stream::DiscardPolicy::New,
                max_messages: 4096,
                max_bytes: 4 * 1024 * 1024,
                ..Default::default()
            })
            .await?;
    } else {
        jetstream.delete_stream(&name).await?;
    }
    client.drain().await?;
    emit(json!({"status":"ok","stream":name}));
    Ok(true)
}

async fn command(mut args: Arguments) -> Result<bool, ReceiverError> {
    if matches!(args.mode.as_str(), "broker-init" | "broker-cleanup") {
        return tokio::time::timeout(Duration::from_secs(10), broker(&mut args)).await?;
    }
    let pool = match open_pool().await {
        Ok(pool) => pool,
        Err(_) => {
            emit(json!({"status":"unknown","error":"database unavailable"}));
            return Ok(false);
        }
    };
    let result = async {
        match args.mode.as_str() {
            "migrate" => {
                let target = args.optional("--target", "producer");
                args.complete()?;
                match target.as_str() {
                    "producer" => migrate::MIGRATOR.run(&pool).await?,
                    "receiver" => {}
                    _ => return Err("migration target must be producer or receiver".into()),
                }
                reading_counter::migrate(&pool).await?;
                emit(json!({"status":"migrated","target":target}));
                Ok(true)
            }
            "accept" => accept(&pool, &mut args).await,
            "read" => read(&pool, &mut args).await,
            "receiver" => {
                receiver(&pool, &mut args).await?;
                Ok(true)
            }
            "replay" => {
                let operation = operation(&args.take("--operation-json")?)?;
                let endpoint = args.optional("--endpoint", ENDPOINT);
                args.complete()?;
                let outbound = Outbound::new([endpoint.clone()]);
                match reading_counter::replay(&pool, &operation, &outbound, &endpoint).await {
                    Ok(accepted) => {
                        emit(json!({"status":"accepted","accepted":accepted}));
                        Ok(true)
                    }
                    Err(error) => {
                        emit(error_receipt(&error));
                        Ok(false)
                    }
                }
            }
            _ => Err("unknown fixture mode".into()),
        }
    }
    .await;
    let closed = infra_postgres::close(&pool, Duration::from_secs(5)).await;
    if closed == infra_postgres::Closed::TimedOut {
        return Err("fixture pool close timed out".into());
    }
    result
}

fn main() -> ExitCode {
    let mut args = std::env::args_os();
    let binary = args.next().unwrap_or_default();
    let remaining: Vec<_> = args.collect();
    if remaining.first().is_some_and(|mode| mode == "worker") {
        return jobs_worker::run(
            std::iter::once(binary).chain(remaining.into_iter().skip(1)),
            register,
        );
    }
    let result = (|| {
        let arguments = Arguments::parse(remaining.into_iter())?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let result = runtime.block_on(command(arguments));
        runtime.shutdown_timeout(Duration::from_secs(1));
        result
    })();
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(error) => {
            emit(json!({"status":"error","error":error.to_string()}));
            ExitCode::FAILURE
        }
    }
}
