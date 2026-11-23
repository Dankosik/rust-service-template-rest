use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use infra_temporal::worker::activity_operation_context;
use temporalio_common::RetryPolicy;
use temporalio_macros::{activities, workflow, workflow_methods};
use temporalio_sdk::ActivityOptions;
use temporalio_sdk::activities::{ActivityContext, ActivityError};
use temporalio_workflow::{WorkflowContext, WorkflowResult};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

#[workflow]
#[derive(Default)]
struct FixtureDrainWorkflow;

#[workflow_methods]
impl FixtureDrainWorkflow {
    #[run]
    async fn run(ctx: &mut WorkflowContext<Self>, mode: String) -> WorkflowResult<String> {
        let mut options = if mode == "preempt" {
            ActivityOptions::start_to_close_timeout(Duration::from_secs(2))
        } else {
            ActivityOptions::start_to_close_timeout(Duration::from_secs(30))
        };
        if mode == "preempt" {
            let mut retry = RetryPolicy::default();
            retry
                .set_initial_interval(Duration::from_millis(100))
                .set_backoff_coefficient(1.0)
                .set_maximum_interval(Some(Duration::from_millis(200)))
                .set_maximum_attempts(5);
            options.retry_policy = Some(retry);
        }
        let result = ctx
            .execute_activity(FixtureActivities::execute, mode, options)
            .await?;
        Ok(result)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FixtureMode {
    Fast,
    Cooperative,
    Stubborn,
}

struct FixtureActivities {
    mode: FixtureMode,
    notify_addr: Option<SocketAddr>,
}

#[activities]
impl FixtureActivities {
    #[activity]
    async fn execute(
        self: Arc<Self>,
        ctx: ActivityContext,
        _workflow_mode: String,
    ) -> Result<String, ActivityError> {
        let attempt = ctx.info().attempt;
        let _op = activity_operation_context(&ctx);
        match self.mode {
            FixtureMode::Fast => Ok(format!("recovered-attempt-{attempt}")),
            FixtureMode::Cooperative => {
                if let Some(addr) = self.notify_addr {
                    let mut stream = TcpStream::connect(addr).await?;
                    stream.write_all(&[1_u8]).await?;
                    let mut release = [0_u8; 1];
                    stream.read_exact(&mut release).await?;
                }
                Ok(format!("cooperative-attempt-{attempt}"))
            }
            FixtureMode::Stubborn => {
                if let Some(addr) = self.notify_addr
                    && let Ok(mut stream) = TcpStream::connect(addr).await
                {
                    let _ = stream.write_all(&[1_u8]).await;
                }
                futures::future::pending::<()>().await;
                Ok(format!("unreachable-{attempt}"))
            }
        }
    }
}

fn register(
    registration: &mut temporal_worker::Registration<'_>,
) -> Result<(), temporal_worker::BuildError> {
    let mode = match std::env::var("TEMPORAL_FIXTURE_MODE")
        .unwrap_or_else(|_| "fast".to_owned())
        .as_str()
    {
        "cooperative" => FixtureMode::Cooperative,
        "stubborn" => FixtureMode::Stubborn,
        _ => FixtureMode::Fast,
    };
    let notify_addr = match std::env::var("TEMPORAL_FIXTURE_NOTIFY_ADDR") {
        Ok(raw) if !raw.is_empty() => Some(
            raw.parse::<SocketAddr>()
                .map_err(|err| temporal_worker::BuildError::new(Box::new(err)))?,
        ),
        _ => None,
    };
    registration
        .temporal
        .default_queue()?
        .register_workflow::<FixtureDrainWorkflow>()?
        .register_activities(FixtureActivities { mode, notify_addr });
    Ok(())
}

fn main() -> ExitCode {
    temporal_worker::run(std::env::args_os(), register)
}
