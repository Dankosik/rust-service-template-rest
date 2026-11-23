#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test helpers panic directly on broken fixtures"
)]

use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use infra_temporal::{
    CloseOutcome, NonBlankName, StartIdPolicy, StartRequest, TemporalAuth, TemporalOptions,
    TemporalStartup, WorkflowId,
};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use operation_context::OperationContext;
use temporalio_macros::{workflow, workflow_methods};
use temporalio_workflow::{WorkflowContext, WorkflowResult};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use url::Url;

const RECORD_BOUND: Duration = Duration::from_secs(20);
const EXIT_BOUND: Duration = Duration::from_secs(20);
const OP_TIMEOUT: Duration = Duration::from_secs(20);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[workflow]
#[derive(Default)]
struct FixtureDrainWorkflow;

#[workflow_methods]
impl FixtureDrainWorkflow {
    #[run]
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "client workflow stub matches FixtureDrainWorkflow signature"
    )]
    async fn run(_ctx: &mut WorkflowContext<Self>, mode: String) -> WorkflowResult<String> {
        Ok(mode)
    }
}

fn unique_name(prefix: &str) -> String {
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{seq}", std::process::id())
}

fn temporal_endpoint() -> Url {
    let raw = std::env::var("TEMPORAL_ADDRESS")
        .expect("TEMPORAL_ADDRESS is required; run just test-integration-temporal");
    let normalized = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw
    } else {
        format!("http://{raw}")
    };
    Url::parse(&normalized).expect("TEMPORAL_ADDRESS must form a valid URL")
}

async fn connect_client() -> (TemporalStartup, infra_temporal::TemporalClient) {
    let options = TemporalOptions {
        endpoint: temporal_endpoint(),
        namespace: "default".to_owned(),
        identity: format!("integration-tests-temporal-{}@0.1.0", std::process::id()),
        auth: TemporalAuth::None,
        tls: None,
        allow_plaintext: true,
        allow_unauthenticated: true,
        trusted_network: false,
        credential_refresh_interval: Duration::from_secs(30),
        connect_timeout: Duration::from_secs(10),
        rpc_timeout: Duration::from_secs(10),
        keep_alive_interval: Duration::from_secs(30),
        keep_alive_timeout: Duration::from_secs(15),
        dns_load_balancing: false,
        http_proxy_url: None,
        max_payload_bytes: 512 * 1024,
        probe_budget: Duration::from_secs(5),
        payload_codec: None,
    };
    let ctx = OperationContext::with_timeout(OP_TIMEOUT);
    let mut startup = TemporalStartup::prepare(options, &ctx);
    let conn = startup
        .admit(CancellationToken::new())
        .await
        .expect("client connects to live Temporal Server");
    let client = conn.client();
    (startup, client)
}

fn extract_json_string_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":\"");
    let rest = line.split_once(&needle)?.1;
    let (value, _) = rest.split_once('"')?;
    Some(value)
}

async fn http_get(addr: SocketAddr, path: &str) -> (String, String) {
    timeout(Duration::from_secs(3), async {
        let mut stream = TcpStream::connect(addr)
            .await
            .expect("connect loopback HTTP listener");
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write HTTP request");
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader
            .read_line(&mut status)
            .await
            .expect("read HTTP status line");
        let mut rest = String::new();
        reader
            .read_to_string(&mut rest)
            .await
            .expect("read HTTP response");
        (status, rest)
    })
    .await
    .expect("HTTP request completes within bound")
}

struct WorkerProcess {
    child: Child,
    pid: u32,
    lines: mpsc::UnboundedReceiver<String>,
    stdout: AbortOnDropHandle<std::io::Result<String>>,
    stderr: AbortOnDropHandle<std::io::Result<String>>,
}

impl WorkerProcess {
    fn spawn(
        task_queue: &str,
        mode: &str,
        notify_addr: Option<SocketAddr>,
        env: &[(&str, &str)],
    ) -> Self {
        let endpoint = temporal_endpoint();
        let mut command = Command::new(env!("CARGO_BIN_EXE_temporal-worker-fixture"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("APP__APP__ENV", "local")
            .env("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true")
            .env("APP__LOG__FORMAT", "json")
            .env("APP__HTTP__ADDR", "127.0.0.1:0")
            .env("APP__OBSERVABILITY__METRICS__ADDR", "127.0.0.1:0")
            .env("APP__TEMPORAL__ENABLED", "true")
            .env("APP__TEMPORAL__ENDPOINT", endpoint.as_str())
            .env("APP__TEMPORAL__ALLOW_PLAINTEXT", "true")
            .env("APP__TEMPORAL__ALLOW_UNAUTHENTICATED", "true")
            .env("APP__TEMPORAL__DNS_LOAD_BALANCING", "false")
            .env("APP__TEMPORAL_WORKER__TASK_QUEUE", task_queue)
            .env("TEMPORAL_FIXTURE_MODE", mode)
            .envs(env.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(addr) = notify_addr {
            command.env("TEMPORAL_FIXTURE_NOTIFY_ADDR", addr.to_string());
        }
        let mut child = command.spawn().expect("spawn temporal-worker-fixture");
        let pid = child.id().expect("spawned worker has PID");
        let stdout_pipe = child.stdout.take().expect("piped stdout");
        let stderr_pipe = child.stderr.take().expect("piped stderr");
        let (tx, lines) = mpsc::unbounded_channel();
        let stdout = tokio::spawn(async move {
            let mut captured = String::new();
            let mut reader = BufReader::new(stdout_pipe).lines();
            while let Some(line) = reader.next_line().await? {
                captured.push_str(&line);
                captured.push('\n');
                let _ = tx.send(line);
            }
            Ok(captured)
        });
        let stderr = tokio::spawn(async move {
            let mut captured = String::new();
            let mut reader = stderr_pipe;
            reader.read_to_string(&mut captured).await?;
            Ok(captured)
        });
        Self {
            child,
            pid,
            lines,
            stdout: AbortOnDropHandle::new(stdout),
            stderr: AbortOnDropHandle::new(stderr),
        }
    }

    async fn await_message(&mut self, message: &str) -> String {
        let needle = format!("\"message\":\"{message}\"");
        timeout(RECORD_BOUND, async {
            loop {
                let Some(line) = self.lines.recv().await else {
                    panic!("worker stdout closed before {message:?} record");
                };
                if line.contains(&needle) {
                    return line;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no {message:?} record before deadline"))
    }

    async fn await_startup_addrs(&mut self) -> (SocketAddr, SocketAddr) {
        let mut health_addr = None;
        let mut metrics_addr = None;
        timeout(RECORD_BOUND, async {
            loop {
                let Some(line) = self.lines.recv().await else {
                    panic!("worker stdout closed before temporal_worker_ready");
                };
                if line.contains("\"message\":\"http listener bound\"") {
                    health_addr = extract_json_string_field(&line, "addr")
                        .map(|raw| raw.parse().expect("valid health SocketAddr"));
                } else if line.contains("\"message\":\"diagnostics listener bound\"") {
                    metrics_addr = extract_json_string_field(&line, "addr")
                        .map(|raw| raw.parse().expect("valid diagnostics SocketAddr"));
                } else if line.contains("\"message\":\"temporal_worker_ready\"") {
                    break;
                }
            }
        })
        .await
        .expect("worker became ready before deadline");
        (
            health_addr.expect("health_listening logged addr"),
            metrics_addr.expect("diagnostics_listening logged addr"),
        )
    }

    fn send_sigterm(&self) {
        kill(Pid::from_raw(self.pid.cast_signed()), Signal::SIGTERM).expect("send SIGTERM");
    }

    async fn wait(mut self) -> (Option<i32>, String, String) {
        let deadline = Instant::now() + EXIT_BOUND;
        let status = timeout(EXIT_BOUND, self.child.wait())
            .await
            .expect("worker exits within bound")
            .expect("wait on worker child");
        let stderr = timeout_at(deadline, &mut self.stderr)
            .await
            .expect("stderr EOF")
            .expect("stderr join")
            .expect("stderr read");
        let stdout = timeout_at(deadline, &mut self.stdout)
            .await
            .expect("stdout EOF")
            .expect("stdout join")
            .expect("stdout read");
        (status.code(), stdout, stderr)
    }
}

#[tokio::test]
async fn sigterm_flips_readiness_503_drains_cooperatively_and_exits_zero() {
    let task_queue = unique_name("fixture-coop-q");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let notify_addr = listener.local_addr().unwrap();

    let (startup, client) = connect_client().await;
    let mut worker = WorkerProcess::spawn(
        &task_queue,
        "cooperative",
        Some(notify_addr),
        &[
            ("APP__TEMPORAL_WORKER__GRACEFUL_SHUTDOWN_PERIOD", "5s"),
            ("APP__TEMPORAL_WORKER__ACTIVITY_CANCEL_CLEANUP", "5s"),
            ("APP__TEMPORAL_WORKER__FINALIZE_BUDGET", "5s"),
            ("APP__HTTP__DRAIN_TIMEOUT", "15s"),
            ("APP__HTTP__GRACE_PERIOD", "35s"),
        ],
    );
    let (health_addr, metrics_addr) = worker.await_startup_addrs().await;

    let (ready_status, _) = http_get(health_addr, "/health/ready").await;
    assert!(
        ready_status.starts_with("HTTP/1.1 200"),
        "expected ready 200, got {ready_status:?}"
    );

    let op = OperationContext::with_timeout(OP_TIMEOUT);
    let handle = client
        .start_workflow(
            &op,
            StartRequest::new(
                FixtureDrainWorkflow,
                NonBlankName::try_from(task_queue.as_str()).unwrap(),
                WorkflowId::try_from(unique_name("fixture-coop-wf")).unwrap(),
                StartIdPolicy::RejectDuplicate,
                "cooperative".to_owned(),
            ),
        )
        .await
        .unwrap();

    let (mut activity_conn, _) = timeout(OP_TIMEOUT, listener.accept())
        .await
        .expect("activity connected to notify listener")
        .unwrap();
    let mut started = [0_u8; 1];
    timeout(
        Duration::from_secs(5),
        activity_conn.read_exact(&mut started),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(started, [1_u8]);

    let (metrics_status, metrics_body) = http_get(metrics_addr, "/metrics").await;
    assert!(
        metrics_status.starts_with("HTTP/1.1 200"),
        "{metrics_status:?}"
    );
    assert!(
        metrics_body.contains("temporal_workflow_task_schedule_to_start_latency_bucket")
            && metrics_body.contains(&format!("task_queue=\"{task_queue}\"")),
        "{metrics_body}"
    );

    worker.send_sigterm();
    let _ = worker.await_message("readiness_disabled").await;

    let (draining_status, _) = http_get(health_addr, "/health/ready").await;
    assert!(
        draining_status.starts_with("HTTP/1.1 503"),
        "expected draining 503, got {draining_status:?}"
    );

    activity_conn.write_all(&[1_u8]).await.unwrap();
    drop(activity_conn);

    let (code, stdout, stderr) = worker.wait().await;
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("\"message\":\"temporal_drain_completed\""),
        "{stdout}"
    );

    let mut worker_two = WorkerProcess::spawn(&task_queue, "fast", None, &[]);
    let _ = worker_two.await_startup_addrs().await;

    let result = handle.get_result(&op).await.unwrap();
    assert_eq!(result, "cooperative-attempt-1");

    worker_two.send_sigterm();
    let (code_two, stdout_two, stderr_two) = worker_two.wait().await;
    assert_eq!(
        code_two,
        Some(0),
        "stdout:\n{stdout_two}\nstderr:\n{stderr_two}"
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(5)).await,
        CloseOutcome::Complete
    );
}

#[tokio::test]
async fn second_sigterm_forces_preemption_exits_three_and_fresh_replica_completes_retry() {
    let task_queue = unique_name("fixture-preempt-q");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let notify_addr = listener.local_addr().unwrap();

    let (startup, client) = connect_client().await;
    let mut worker_one = WorkerProcess::spawn(
        &task_queue,
        "stubborn",
        Some(notify_addr),
        &[
            ("APP__TEMPORAL_WORKER__GRACEFUL_SHUTDOWN_PERIOD", "45s"),
            ("APP__TEMPORAL_WORKER__ACTIVITY_CANCEL_CLEANUP", "10s"),
            ("APP__TEMPORAL_WORKER__FINALIZE_BUDGET", "5s"),
            ("APP__HTTP__DRAIN_TIMEOUT", "60s"),
            ("APP__HTTP__GRACE_PERIOD", "90s"),
        ],
    );
    let (health_addr, _) = worker_one.await_startup_addrs().await;
    let (ready_status, _) = http_get(health_addr, "/health/ready").await;
    assert!(ready_status.starts_with("HTTP/1.1 200"), "{ready_status:?}");

    let op = OperationContext::with_timeout(OP_TIMEOUT);
    let handle = client
        .start_workflow(
            &op,
            StartRequest::new(
                FixtureDrainWorkflow,
                NonBlankName::try_from(task_queue.as_str()).unwrap(),
                WorkflowId::try_from(unique_name("fixture-preempt-wf")).unwrap(),
                StartIdPolicy::RejectDuplicate,
                "preempt".to_owned(),
            ),
        )
        .await
        .unwrap();

    let (mut activity_conn, _) = timeout(OP_TIMEOUT, listener.accept())
        .await
        .expect("stubborn activity started on worker_one")
        .unwrap();
    let mut started = [0_u8; 1];
    timeout(
        Duration::from_secs(5),
        activity_conn.read_exact(&mut started),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(started, [1_u8]);

    worker_one.send_sigterm();
    let _ = worker_one.await_message("temporal_drain_started").await;

    let (draining_status, _) = http_get(health_addr, "/health/ready").await;
    assert!(
        draining_status.starts_with("HTTP/1.1 503"),
        "{draining_status:?}"
    );

    let second_stop_at = Instant::now();
    worker_one.send_sigterm();

    let (code, stdout, stderr) = worker_one.wait().await;
    assert!(
        second_stop_at.elapsed() < Duration::from_secs(12),
        "2nd SIGTERM must preempt without waiting 45s graceful_shutdown_period"
    );
    assert_eq!(code, Some(3), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("\"message\":\"temporal_drain_forced\"")
            && stdout.contains("\"reason\":\"second_signal\""),
        "{stdout}"
    );

    let mut worker_two = WorkerProcess::spawn(&task_queue, "fast", None, &[]);
    let (health_two, _) = worker_two.await_startup_addrs().await;
    let (ready_two, _) = http_get(health_two, "/health/ready").await;
    assert!(ready_two.starts_with("HTTP/1.1 200"), "{ready_two:?}");

    let result = handle.get_result(&op).await.unwrap();
    assert_eq!(result, "recovered-attempt-2");

    worker_two.send_sigterm();
    let (code_two, stdout_two, stderr_two) = worker_two.wait().await;
    assert_eq!(
        code_two,
        Some(0),
        "stdout:\n{stdout_two}\nstderr:\n{stderr_two}"
    );
    assert_eq!(
        startup.close(Instant::now() + Duration::from_secs(5)).await,
        CloseOutcome::Complete
    );
}
