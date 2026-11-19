use std::{num::NonZeroU32, path::PathBuf, process::ExitCode, time::Duration};

use clap::{Parser, Subcommand};
use infra_postgres::{Dsn, Isolation, Pool, PoolOptions, SessionSettingsSource, SslPolicy};
use infra_webhooks::outbound::Producer;
use integration_tests::reading_counter::{self, Error, Operation, PreparedRequest};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

const ENDPOINT: &str = "reading-counter";
type FixtureError = Box<dyn std::error::Error + Send + Sync>;

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
    Accept(AcceptArgs),
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

fn operation(raw: &str) -> Result<Operation, FixtureError> {
    if raw.len() > 1024 {
        return Err("operation exceeds one KiB".into());
    }
    let value: Operation = serde_json::from_str(raw)?;
    value.validate()?;
    Ok(value)
}

async fn open_pool() -> Result<Pool, FixtureError> {
    let dsn = Dsn::admit_with(
        &std::env::var("DATABASE_URL")?,
        None,
        SslPolicy::AllowLocalDevelopment,
    )?;
    Ok(infra_postgres::connect(
        &dsn,
        &PoolOptions {
            max_connections: const { NonZeroU32::new(2).expect("2 is nonzero") },
            application_name: "reading-counter-fixture",
            default_isolation: Isolation::ServerDefault,
            session_settings_source: SessionSettingsSource::Startup,
            wait_order: infra_postgres::WaitOrder::Arrival,
            notifications: true,
        },
    )
    .await?)
}

#[expect(
    clippy::print_stdout,
    reason = "JSON lines are the fixture's output protocol"
)]
fn emit(value: &Value) {
    println!("{value}");
}

fn error_receipt(error: &Error) -> Value {
    json!({"status": if matches!(error, Error::Conflict) { "conflict" } else { "unknown" },
        "error": error.to_string()})
}

async fn accept_one(
    pool: &Pool,
    operation: Operation,
    publication: &infra_messaging::PublicationRoute<Operation>,
    endpoint: &str,
) -> Result<bool, FixtureError> {
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

async fn accept(pool: &Pool, args: AcceptArgs) -> Result<bool, FixtureError> {
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

async fn command(Command::Accept(args): Command) -> Result<bool, FixtureError> {
    let Ok(pool) = open_pool().await else {
        emit(&json!({"status":"unknown","error":"database unavailable"}));
        return Ok(false);
    };
    let result = accept(&pool, args).await;
    let closed = infra_postgres::close(&pool, Duration::from_secs(5)).await;
    if closed == infra_postgres::CloseOutcome::TimedOut {
        return Err("fixture pool close timed out".into());
    }
    result
}

fn main() -> ExitCode {
    let result = (|| -> Result<bool, FixtureError> {
        let cli = Cli::try_parse()?;
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
