use std::{
    collections::BTreeMap, ffi::OsString, net::SocketAddr, num::NonZeroU32, path::PathBuf,
    process::ExitCode, time::Duration,
};

use clap::{Parser, Subcommand};
use infra_postgres::{Dsn, Isolation, PoolOptions, SessionSettingsSource};
use infra_webhooks::{
    outbound::{Dispatcher, Endpoint, Producer},
    protocol::KeyRing,
};
use integration_tests::{
    reading_counter::{self, Channel, Error, Operation, PreparedRequest},
    reading_counter_receiver::{self, CompletionHold, ReceiverError, ReceiverOptions},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::io::AsyncReadExt;

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
    let client = infra_outbound_http::Client::try_new_for_test_http(
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

#[derive(Debug, Parser)]
#[command(
    name = "reading-counter-fixture",
    disable_version_flag = true,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Migrate {
        #[arg(long, default_value = "producer")]
        target: String,
    },
    Accept(AcceptArgs),
    Read {
        #[arg(long)]
        operation_json: String,
        #[arg(long, default_value = "local")]
        channel: String,
    },
    Receiver(ReceiverArgs),
    Replay {
        #[arg(long)]
        operation_json: String,
        #[arg(long, default_value = ENDPOINT)]
        endpoint: String,
    },
    BrokerInit {
        #[arg(long)]
        nats_url: String,
        #[arg(long)]
        stream: String,
        #[arg(long)]
        subject: String,
    },
    BrokerCleanup {
        #[arg(long)]
        nats_url: String,
        #[arg(long)]
        stream: String,
    },
}

#[derive(Debug, clap::Args)]
struct AcceptArgs {
    #[arg(long)]
    subject: Option<String>,
    #[arg(long, default_value = ENDPOINT)]
    endpoint: String,
    #[arg(
        long,
        conflicts_with = "operation_json",
        required_unless_present = "operation_json"
    )]
    operations_file: Option<PathBuf>,
    #[arg(long, required_unless_present = "operations_file")]
    operation_json: Option<String>,
}

#[derive(Debug, clap::Args)]
struct ReceiverArgs {
    #[arg(long)]
    nats_url: String,
    #[arg(long)]
    stream: String,
    #[arg(long)]
    subject: String,
    #[arg(long)]
    consumer: String,
    #[arg(long)]
    http_bind: SocketAddr,
    #[arg(long, default_value_t = 250)]
    run_seconds: u64,
    #[arg(
        long,
        requires_all = ["hold_operation", "fault_ready", "fault_release"]
    )]
    hold_channel: Option<String>,
    #[arg(long, requires = "hold_channel")]
    hold_operation: Option<String>,
    #[arg(long, requires = "hold_channel")]
    fault_ready: Option<PathBuf>,
    #[arg(long, requires = "hold_channel")]
    fault_release: Option<PathBuf>,
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
            max_connections: const { NonZeroU32::new(2).expect("2 is nonzero") },
            application_name: "reading-counter-fixture",
            default_isolation: Isolation::ServerDefault,
            session_settings_source: SessionSettingsSource::Startup,
            wait_order: infra_postgres::WaitOrder::Arrival,
        },
    )
    .await?)
}

#[expect(
    clippy::print_stdout,
    reason = "JSON lines are the fixture protocol consumed by the process driver"
)]
fn emit(value: &Value) {
    println!("{value}");
}

fn error_receipt(error: &Error) -> Value {
    json!({"status": if matches!(error, Error::Conflict) { "conflict" } else { "unknown" },
        "error": error.to_string()})
}

async fn accept_one(
    pool: &PgPool,
    operation: Operation,
    publication: &infra_messaging::PublicationRoute<Operation>,
    endpoint: &str,
) -> Result<bool, ReceiverError> {
    let prepared = PreparedRequest::try_new(operation, publication)?;
    let outbound = Producer::new([endpoint.to_owned()]);
    match reading_counter::accept(pool, &prepared, &outbound, endpoint).await {
        Ok(accepted) => {
            emit(&json!({"status":"accepted", "accepted":accepted}));
            Ok(true)
        }
        Err(error) if error.commit_unknown() => {
            if let Ok(Some(accepted)) =
                reading_counter::read_request(pool, prepared.operation()).await
            {
                emit(&json!({"status":"accepted","accepted":accepted,"commit_reconciled":true}));
                Ok(true)
            } else {
                emit(&json!({"status":"unknown","operation":prepared.operation()}));
                Ok(false)
            }
        }
        Err(error) => {
            emit(&error_receipt(&error));
            Ok(false)
        }
    }
}

async fn accept(pool: &PgPool, args: AcceptArgs) -> Result<bool, ReceiverError> {
    let operations = if let Some(path) = args.operations_file {
        const MAX_BATCH_BYTES: usize = 128 * 1024 + 256;
        let file = tokio::fs::File::open(path).await?;
        if file.metadata().await?.len() > MAX_BATCH_BYTES as u64 {
            return Err("batch exceeds size bound".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BATCH_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > MAX_BATCH_BYTES {
            return Err("batch exceeds size bound".into());
        }
        let values: Vec<Operation> = serde_json::from_slice(&bytes)?;
        if values.is_empty() || values.len() > 128 {
            return Err("batch requires 1..=128 operations".into());
        }
        for value in &values {
            value.validate()?;
        }
        values
    } else {
        let raw = args.operation_json.ok_or("missing --operation-json")?;
        vec![operation(&raw)?]
    };
    let subject = args
        .subject
        .or_else(|| std::env::var("READING_EVENT_SUBJECT").ok())
        .unwrap_or_else(|| reading_counter::SUBJECT.to_owned());
    let publication =
        reading_counter::publication(&mut infra_messaging::Registry::default(), &subject)?;
    for operation in operations {
        if !accept_one(pool, operation, &publication, &args.endpoint).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn read(pool: &PgPool, operation_json: &str, selected: &str) -> Result<bool, ReceiverError> {
    let operation = operation(operation_json)?;
    if selected == "request" {
        match reading_counter::read_request(pool, &operation).await {
            Ok(value) => emit(
                &json!({"status":if value.is_some() {"present"} else {"absent"},"accepted":value}),
            ),
            Err(error) => {
                emit(&error_receipt(&error));
                return Ok(false);
            }
        }
    } else {
        match reading_counter::read_effect(pool, channel(selected)?, &operation).await {
            Ok(value) => emit(
                &json!({"status":if value.is_some() {"present"} else {"absent"},"effect":value}),
            ),
            Err(error) => {
                emit(&error_receipt(&error));
                return Ok(false);
            }
        }
    }
    Ok(true)
}

async fn receiver(pool: &PgPool, args: ReceiverArgs) -> Result<(), ReceiverError> {
    let hold = if let Some(selected) = args.hold_channel {
        let channel = channel(&selected)?;
        if channel == Channel::Local {
            return Err("receiver hold channel must be outbox or webhook".into());
        }
        Some(CompletionHold {
            channel,
            operation_id: args.hold_operation.ok_or("missing --hold-operation")?,
            ready: args.fault_ready.ok_or("missing --fault-ready")?,
            release: args.fault_release.ok_or("missing --fault-release")?,
        })
    } else {
        None
    };
    let options = ReceiverOptions {
        nats_url: args.nats_url,
        stream: args.stream,
        subject: args.subject,
        consumer: args.consumer,
        http_bind: args.http_bind,
        run_for: Duration::from_secs(args.run_seconds),
        hold,
    };
    Box::pin(reading_counter_receiver::run(pool, options)).await
}

async fn broker(url: String, name: String, subject: Option<String>) -> Result<bool, ReceiverError> {
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
    emit(&json!({"status":"ok","stream":name}));
    Ok(true)
}

async fn command(cmd: Command) -> Result<bool, ReceiverError> {
    match cmd {
        Command::BrokerInit {
            nats_url,
            stream,
            subject,
        } => {
            return tokio::time::timeout(
                Duration::from_secs(10),
                broker(nats_url, stream, Some(subject)),
            )
            .await?;
        }
        Command::BrokerCleanup { nats_url, stream } => {
            return tokio::time::timeout(Duration::from_secs(10), broker(nats_url, stream, None))
                .await?;
        }
        _ => {}
    }
    let Ok(pool) = open_pool().await else {
        emit(&json!({"status":"unknown","error":"database unavailable"}));
        return Ok(false);
    };
    let result = async {
        match cmd {
            Command::Migrate { target } => {
                match target.as_str() {
                    "producer" => migrate::MIGRATOR.run(&pool).await?,
                    "receiver" => {}
                    _ => return Err("migration target must be producer or receiver".into()),
                }
                reading_counter::migrate(&pool).await?;
                emit(&json!({"status":"migrated","target":target}));
                Ok(true)
            }
            Command::Accept(args) => accept(&pool, args).await,
            Command::Read {
                operation_json,
                channel,
            } => read(&pool, &operation_json, &channel).await,
            Command::Receiver(args) => {
                receiver(&pool, args).await?;
                Ok(true)
            }
            Command::Replay {
                operation_json,
                endpoint,
            } => {
                let operation = operation(&operation_json)?;
                let outbound = Producer::new([endpoint.clone()]);
                match reading_counter::replay(&pool, &operation, &outbound, &endpoint).await {
                    Ok(accepted) => {
                        emit(&json!({"status":"accepted","accepted":accepted}));
                        Ok(true)
                    }
                    Err(error) => {
                        emit(&error_receipt(&error));
                        Ok(false)
                    }
                }
            }
            Command::BrokerInit { .. } | Command::BrokerCleanup { .. } => {
                Err("broker subcommands execute before pool admission".into())
            }
        }
    }
    .await;
    let closed = infra_postgres::close(&pool, Duration::from_secs(5)).await;
    if closed == infra_postgres::CloseOutcome::TimedOut {
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
    let result = (|| -> Result<bool, ReceiverError> {
        let cli = Cli::try_parse_from(
            std::iter::once(OsString::from("reading-counter-fixture")).chain(remaining),
        )?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let result = runtime.block_on(command(cli.command));
        runtime.shutdown_timeout(Duration::from_secs(1));
        result
    })();
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(error) => {
            emit(&json!({"status":"error","error":error.to_string()}));
            ExitCode::FAILURE
        }
    }
}
