#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a broken fixture or setup fails the test"
)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead as _, BufReader};
use std::net::SocketAddr;
use std::pin::Pin;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Response};
use bytes::{BufMut as _, Bytes, BytesMut};
use http_body::{Body as HttpBody, Frame};
use infra_temporal::CloseOutcome;
use prost::Message as _;
use temporalio_common::protos::temporal::api::common::v1::{
    ActivityType, Payload, Payloads, WorkflowExecution, WorkflowType,
};
use temporalio_common::protos::temporal::api::enums::v1::{EventType, TaskQueueKind};
use temporalio_common::protos::temporal::api::history::v1::{
    History, HistoryEvent, WorkflowExecutionStartedEventAttributes,
    WorkflowTaskScheduledEventAttributes, WorkflowTaskStartedEventAttributes, history_event,
};
use temporalio_common::protos::temporal::api::taskqueue::v1::TaskQueue;
use temporalio_common::protos::temporal::api::workflowservice::v1::{
    DescribeNamespaceResponse, GetSystemInfoResponse, PollActivityTaskQueueResponse,
    PollWorkflowTaskQueueResponse, RecordActivityTaskHeartbeatResponse,
    RespondActivityTaskCompletedResponse, RespondWorkflowTaskCompletedResponse,
};
use temporalio_common::protos::{ENCODING_PAYLOAD_KEY, JSON_ENCODING_VAL};
use temporalio_macros::{activities, workflow, workflow_methods};
use temporalio_sdk::activities::{ActivityContext, ActivityError};
use temporalio_workflow::{WorkflowContext, WorkflowResult};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

struct GrpcFrameBody {
    data: Option<Bytes>,
    trailers: Option<HeaderMap>,
}

impl GrpcFrameBody {
    fn ok(proto: &[u8]) -> Self {
        let len = u32::try_from(proto.len()).expect("test proto fits in u32");
        let mut frame = BytesMut::with_capacity(5 + proto.len());
        frame.put_u8(0);
        frame.put_u32(len);
        frame.extend_from_slice(proto);
        let mut trailers = HeaderMap::new();
        trailers.insert(
            HeaderName::from_static("grpc-status"),
            HeaderValue::from_static("0"),
        );
        Self {
            data: Some(frame.freeze()),
            trailers: Some(trailers),
        }
    }
}

impl HttpBody for GrpcFrameBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if let Some(data) = self.data.take() {
            return Poll::Ready(Some(Ok(Frame::data(data))));
        }
        if let Some(trailers) = self.trailers.take() {
            return Poll::Ready(Some(Ok(Frame::trailers(trailers))));
        }
        Poll::Ready(None)
    }
}

fn grpc_ok(proto: &[u8]) -> Response<Body> {
    Response::builder()
        .status(200)
        .header("content-type", "application/grpc")
        .body(Body::new(GrpcFrameBody::ok(proto)))
        .unwrap()
}

fn json_string_payload(value: &str) -> Payload {
    Payload {
        metadata: HashMap::from([(
            ENCODING_PAYLOAD_KEY.to_owned(),
            JSON_ENCODING_VAL.as_bytes().to_vec(),
        )]),
        data: serde_json::to_vec(value).unwrap(),
        external_payloads: Vec::new(),
    }
}

#[workflow]
#[derive(Default)]
struct CpuSpinWorkflow;

#[workflow_methods]
impl CpuSpinWorkflow {
    #[run]
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "temporal #[workflow_methods] #[run] requires async fn"
    )]
    async fn run(_ctx: &mut WorkflowContext<Self>, millis: u64) -> WorkflowResult<String> {
        let spin_until = Instant::now() + Duration::from_millis(millis);
        tracing::info!(millis, "cpu_spin_workflow_started");
        while Instant::now() < spin_until {
            std::hint::spin_loop();
        }
        tracing::info!("cpu_spin_workflow_finished");
        Ok("spun".to_owned())
    }
}

struct StubbornBlockingActivities;

#[activities]
impl StubbornBlockingActivities {
    #[activity]
    async fn block(_ctx: ActivityContext, _input: String) -> Result<String, ActivityError> {
        tracing::info!("stubborn_activity_started");
        futures::future::pending::<()>().await;
        Ok("never".to_owned())
    }
}

struct CooperativeShutdownActivities;

#[activities]
impl CooperativeShutdownActivities {
    #[activity]
    async fn work(ctx: ActivityContext, _input: String) -> Result<String, ActivityError> {
        let op = infra_temporal::worker::activity_operation_context(&ctx);
        tracing::info!("cooperative_activity_started");
        let stopped = op.wait_stopped().await;
        if stopped == operation_context::Stopped::Cancelled {
            tracing::info!("cooperative_activity_cancelled");
            return Err(ActivityError::cancelled());
        }
        Ok("unexpected".to_owned())
    }
}

struct LeakingActivities {
    leaked: Arc<Mutex<Option<ActivityContext>>>,
}

#[activities]
impl LeakingActivities {
    #[activity]
    async fn leak(
        self: Arc<Self>,
        ctx: ActivityContext,
        _input: String,
    ) -> Result<String, ActivityError> {
        *self.leaked.lock().unwrap() = Some(ctx);
        tokio::task::yield_now().await;
        tracing::info!("leaking_activity_completed");
        Ok("leaked".to_owned())
    }
}

struct ReadyCustomProbe;

impl health::Probe for ReadyCustomProbe {
    fn name(&self) -> &'static str {
        "custom_dep"
    }

    fn check(&self) -> impl Future<Output = Result<(), health::ProbeError>> + Send {
        std::future::ready(Ok(()))
    }
}

#[derive(Clone, Copy)]
enum MockMode {
    Idle,
    BlockingActivity,
    CooperativeActivity,
    LeakingActivity,
    CpuSpinWorkflow { spin_ms: u64 },
}

struct LoopbackTemporalState {
    mode: MockMode,
    delivered: AtomicBool,
    cancel_rpcs: AtomicUsize,
    fail_rpcs: AtomicUsize,
    complete_wf_rpcs: AtomicUsize,
    poll_release: CancellationToken,
    shutdown_release: CancellationToken,
    hold_shutdown_worker: bool,
    activity_delivered: Notify,
    workflow_delivered: Notify,
}

struct LoopbackTemporalServer {
    addr: SocketAddr,
    state: Arc<LoopbackTemporalState>,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl LoopbackTemporalServer {
    async fn start(mode: MockMode) -> Self {
        Self::start_with_shutdown_gate(mode, false).await
    }

    async fn start_with_shutdown_gate(mode: MockMode, hold_shutdown_worker: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        let state = Arc::new(LoopbackTemporalState {
            mode,
            delivered: AtomicBool::new(false),
            cancel_rpcs: AtomicUsize::new(0),
            fail_rpcs: AtomicUsize::new(0),
            complete_wf_rpcs: AtomicUsize::new(0),
            poll_release: stop.child_token(),
            shutdown_release: stop.child_token(),
            hold_shutdown_worker,
            activity_delivered: Notify::new(),
            workflow_delivered: Notify::new(),
        });

        let router = build_temporal_router(&state);
        let server_stop = stop.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(server_stop.cancelled_owned())
                .await
                .unwrap();
        });

        Self {
            addr,
            state,
            stop,
            task,
        }
    }

    fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn release_polls(&self) {
        self.state.poll_release.cancel();
    }

    fn release_shutdown(&self) {
        self.state.shutdown_release.cancel();
    }

    fn cancel_rpcs(&self) -> usize {
        self.state.cancel_rpcs.load(Ordering::Acquire)
    }

    fn fail_rpcs(&self) -> usize {
        self.state.fail_rpcs.load(Ordering::Acquire)
    }

    async fn stop(self) {
        self.stop.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(3), self.task).await;
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "loopback gRPC test fixture mounts Temporal WorkflowService RPC routes directly on axum"
)]
fn build_temporal_router(state: &Arc<LoopbackTemporalState>) -> axum::Router {
    let s_poll_wf = Arc::clone(state);
    let s_poll_act = Arc::clone(state);
    let s_cancel = Arc::clone(state);
    let s_fail = Arc::clone(state);
    let s_wf_complete = Arc::clone(state);
    let s_shutdown = Arc::clone(state);

    axum::Router::new()
        .route(
            "/temporal.api.workflowservice.v1.WorkflowService/GetSystemInfo",
            axum::routing::post(async || {
                grpc_ok(&GetSystemInfoResponse::default().encode_to_vec())
            }),
        )
        .route(
            "/temporal.api.workflowservice.v1.WorkflowService/DescribeNamespace",
            axum::routing::post(async || {
                grpc_ok(&DescribeNamespaceResponse::default().encode_to_vec())
            }),
        )
        .route(
            "/temporal.api.workflowservice.v1.WorkflowService/PollWorkflowTaskQueue",
            axum::routing::post(move || {
                let state = Arc::clone(&s_poll_wf);
                async move { handle_poll_workflow(&state).await }
            }),
        )
        .route(
            "/temporal.api.workflowservice.v1.WorkflowService/PollActivityTaskQueue",
            axum::routing::post(move || {
                let state = Arc::clone(&s_poll_act);
                async move { handle_poll_activity(&state).await }
            }),
        )
        .route(
            "/temporal.api.workflowservice.v1.WorkflowService/RespondWorkflowTaskCompleted",
            axum::routing::post(move || {
                let state = Arc::clone(&s_wf_complete);
                async move {
                    state.complete_wf_rpcs.fetch_add(1, Ordering::AcqRel);
                    grpc_ok(&RespondWorkflowTaskCompletedResponse::default().encode_to_vec())
                }
            }),
        )
        .route(
            "/temporal.api.workflowservice.v1.WorkflowService/RespondActivityTaskCanceled",
            axum::routing::post(move || {
                let state = Arc::clone(&s_cancel);
                async move {
                    state.cancel_rpcs.fetch_add(1, Ordering::AcqRel);
                    grpc_ok(&[])
                }
            }),
        )
        .route(
            "/temporal.api.workflowservice.v1.WorkflowService/RespondActivityTaskFailed",
            axum::routing::post(move || {
                let state = Arc::clone(&s_fail);
                async move {
                    state.fail_rpcs.fetch_add(1, Ordering::AcqRel);
                    grpc_ok(&[])
                }
            }),
        )
        .route(
            "/temporal.api.workflowservice.v1.WorkflowService/RespondActivityTaskCompleted",
            axum::routing::post(async || {
                grpc_ok(&RespondActivityTaskCompletedResponse::default().encode_to_vec())
            }),
        )
        .route(
            "/temporal.api.workflowservice.v1.WorkflowService/RecordActivityTaskHeartbeat",
            axum::routing::post(async || {
                grpc_ok(&RecordActivityTaskHeartbeatResponse::default().encode_to_vec())
            }),
        )
        .route(
            "/temporal.api.workflowservice.v1.WorkflowService/ShutdownWorker",
            axum::routing::post(move || {
                let state = Arc::clone(&s_shutdown);
                async move {
                    if state.hold_shutdown_worker {
                        state.shutdown_release.cancelled().await;
                    }
                    grpc_ok(&[])
                }
            }),
        )
        .fallback(async || grpc_ok(&[]))
}

async fn handle_poll_workflow(state: &LoopbackTemporalState) -> Response<Body> {
    if let MockMode::CpuSpinWorkflow { spin_ms } = state.mode
        && !state.delivered.swap(true, Ordering::AcqRel)
    {
        state.workflow_delivered.notify_one();
        let now = SystemTime::now().into();
        let task_queue = TaskQueue {
            name: "spin-queue".to_owned(),
            kind: TaskQueueKind::Normal as i32,
            normal_name: String::new(),
        };
        let input_payload = Payload {
            metadata: HashMap::from([(
                ENCODING_PAYLOAD_KEY.to_owned(),
                JSON_ENCODING_VAL.as_bytes().to_vec(),
            )]),
            data: serde_json::to_vec(&spin_ms).unwrap(),
            external_payloads: Vec::new(),
        };
        let resp = PollWorkflowTaskQueueResponse {
            task_token: b"wf-spin-1".to_vec(),
            workflow_execution: Some(WorkflowExecution {
                workflow_id: "wf-spin-1".to_owned(),
                run_id: "run-spin-1".to_owned(),
            }),
            workflow_type: Some(WorkflowType {
                name: "CpuSpinWorkflow".to_owned(),
            }),
            workflow_execution_task_queue: Some(task_queue.clone()),
            started_event_id: 3,
            started_time: Some(now),
            attempt: 1,
            history: Some(History {
                events: vec![
                    HistoryEvent {
                        event_id: 1,
                        event_time: Some(now),
                        event_type: EventType::WorkflowExecutionStarted as i32,
                        attributes: Some(
                            history_event::Attributes::WorkflowExecutionStartedEventAttributes(
                                WorkflowExecutionStartedEventAttributes {
                                    workflow_type: Some(WorkflowType {
                                        name: "CpuSpinWorkflow".to_owned(),
                                    }),
                                    task_queue: Some(task_queue),
                                    original_execution_run_id: "run-spin-1".to_owned(),
                                    input: Some(Payloads {
                                        payloads: vec![input_payload],
                                    }),
                                    workflow_id: "wf-spin-1".to_owned(),
                                    ..Default::default()
                                },
                            ),
                        ),
                        ..Default::default()
                    },
                    HistoryEvent {
                        event_id: 2,
                        event_time: Some(now),
                        event_type: EventType::WorkflowTaskScheduled as i32,
                        attributes: Some(
                            history_event::Attributes::WorkflowTaskScheduledEventAttributes(
                                WorkflowTaskScheduledEventAttributes::default(),
                            ),
                        ),
                        ..Default::default()
                    },
                    HistoryEvent {
                        event_id: 3,
                        event_time: Some(now),
                        event_type: EventType::WorkflowTaskStarted as i32,
                        attributes: Some(
                            history_event::Attributes::WorkflowTaskStartedEventAttributes(
                                WorkflowTaskStartedEventAttributes {
                                    scheduled_event_id: 2,
                                    ..Default::default()
                                },
                            ),
                        ),
                        ..Default::default()
                    },
                ],
            }),
            ..Default::default()
        };
        return grpc_ok(&resp.encode_to_vec());
    }
    state.poll_release.cancelled().await;
    grpc_ok(&[])
}

async fn handle_poll_activity(state: &LoopbackTemporalState) -> Response<Body> {
    let activity_name = match state.mode {
        MockMode::BlockingActivity => Some(StubbornBlockingActivities::block.name()),
        MockMode::CooperativeActivity => Some(CooperativeShutdownActivities::work.name()),
        MockMode::LeakingActivity => Some(LeakingActivities::leak.name()),
        MockMode::Idle | MockMode::CpuSpinWorkflow { .. } => None,
    };
    if let Some(activity_name) = activity_name
        && !state.delivered.swap(true, Ordering::AcqRel)
    {
        state.activity_delivered.notify_one();
        let now = SystemTime::now().into();
        let resp = PollActivityTaskQueueResponse {
            task_token: b"act-block-1".to_vec(),
            workflow_execution: Some(WorkflowExecution {
                workflow_id: "wf-block-1".to_owned(),
                run_id: "run-block-1".to_owned(),
            }),
            activity_id: "1".to_owned(),
            activity_type: Some(ActivityType {
                name: activity_name.to_owned(),
            }),
            input: Some(Payloads {
                payloads: vec![json_string_payload("job")],
            }),
            scheduled_time: Some(now),
            started_time: Some(now),
            start_to_close_timeout: Duration::from_mins(1).try_into().ok(),
            schedule_to_close_timeout: Duration::from_mins(2).try_into().ok(),
            attempt: 1,
            ..Default::default()
        };
        return grpc_ok(&resp.encode_to_vec());
    }
    state.poll_release.cancelled().await;
    grpc_ok(&[])
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "process integration tests execute the compiled temporal-worker binary synchronously"
)]
fn shipped_binary_output(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_temporal-worker"))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(env.iter().copied())
        .output()
        .expect("spawn temporal-worker")
}

#[expect(
    clippy::disallowed_types,
    reason = "isolated child process exercises temporal_worker::run with real signals and global subscribers"
)]
struct ManagedChild {
    child: Option<Child>,
    lines: mpsc::Receiver<String>,
    all_lines: Arc<Mutex<Vec<String>>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "isolated child process exercises temporal_worker::run with real signals and global subscribers"
)]
impl ManagedChild {
    fn spawn(child_test: &str, env: &[(&str, &str)]) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", child_test, "--ignored", "--no-capture"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TEMPORAL_WORKER_CHILD_MODE", "1")
            .envs(env.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn child test");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let all_lines = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&all_lines);
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                captured.lock().unwrap().push(line.clone());
                let _ = tx.send(line);
            }
        });
        Self {
            child: Some(child),
            lines: rx,
            all_lines,
            reader: Some(reader),
        }
    }

    fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    fn wait_for_line(&self, needle: &str, timeout: Duration) -> String {
        let existing = self
            .all_lines
            .lock()
            .unwrap()
            .iter()
            .find(|line| line.contains(needle))
            .cloned();
        if let Some(existing) = existing {
            return existing;
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for log line containing {needle:?}; logs:\n{}",
                self.collected_logs()
            );
            match self.lines.recv_timeout(remaining) {
                Ok(line) if line.contains(needle) => return line,
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => panic!(
                    "timed out waiting for log line containing {needle:?}; logs:\n{}",
                    self.collected_logs()
                ),
                Err(mpsc::RecvTimeoutError::Disconnected) => panic!(
                    "child stdout closed before seeing {needle:?}; logs:\n{}",
                    self.collected_logs()
                ),
            }
        }
    }

    fn wait_exit(mut self, timeout: Duration) -> (Option<i32>, String, String) {
        let mut child = self.child.take().unwrap();
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "child did not exit within {timeout:?}; logs:\n{}",
                self.collected_logs()
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut stderr = String::new();
        if let Some(mut err_pipe) = child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut err_pipe, &mut stderr);
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        (status.code(), self.collected_logs(), stderr)
    }

    fn collected_logs(&self) -> String {
        self.all_lines.lock().unwrap().join("\n")
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "drop guard ensures no test child process leaks on panic"
)]
impl Drop for ManagedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "Unix signal fixture delivers real SIGTERM to its owned child process"
)]
fn send_sigterm(pid: u32) {
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
}

fn extract_bound_addr(json_line: &str) -> SocketAddr {
    let parsed: serde_json::Value = serde_json::from_str(json_line).unwrap();
    parsed["addr"].as_str().unwrap().parse().unwrap()
}

async fn http_get(addr: SocketAddr, path: &str) -> (String, String) {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reader = tokio::io::BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status).await.unwrap();
        let mut rest = String::new();
        reader.read_to_string(&mut rest).await.unwrap();
        (status, rest)
    })
    .await
    .unwrap()
}

fn base_local_temporal_env(endpoint: &str) -> Vec<(&'static str, &str)> {
    vec![
        ("APP__APP__ENV", "local"),
        ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
        ("APP__LOG__FORMAT", "json"),
        ("APP__HTTP__ADDR", "127.0.0.1:0"),
        ("APP__OBSERVABILITY__METRICS__ADDR", ""),
        ("APP__TEMPORAL__ENABLED", "true"),
        ("APP__TEMPORAL__ENDPOINT", endpoint),
        ("APP__TEMPORAL__ALLOW_PLAINTEXT", "true"),
        ("APP__TEMPORAL__ALLOW_UNAUTHENTICATED", "true"),
        ("APP__TEMPORAL__DNS_LOAD_BALANCING", "false"),
    ]
}

#[test]
fn help_exits_zero_and_prints_usage() {
    let output = shipped_binary_output(&["--help"], &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stdout.contains("Usage"), "stdout: {stdout}");
}

#[test]
fn invalid_cli_args_exit_two_without_echoing_input() {
    let output = shipped_binary_output(&["--sensitive-rejected-flag"], &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert_eq!(output.stdout, b"");
    assert!(!stderr.contains("sensitive-rejected-flag"), "{stderr}");
}

#[test]
fn shipped_binary_refuses_when_temporal_disabled() {
    let output = shipped_binary_output(&[], &[("APP__LOG__FORMAT", "json")]);
    assert!(
        output.stderr.is_empty(),
        "no duplicate stderr fallback: {:?}",
        output.stderr
    );
    let diagnostics = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "diagnostics: {diagnostics}");
    assert!(
        diagnostics.contains("temporal worker failed"),
        "{diagnostics}"
    );
    assert!(
        diagnostics.contains("temporal.enabled must be true"),
        "{diagnostics}"
    );
    assert!(
        diagnostics.contains("trace_shutdown_completed"),
        "{diagnostics}"
    );
    assert!(diagnostics.contains("shutdown_finishing"), "{diagnostics}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_handler_registration_exits_one_and_runs_close_hooks() {
    let server = LoopbackTemporalServer::start(MockMode::Idle).await;
    let endpoint = server.endpoint();
    let env = base_local_temporal_env(&endpoint);
    let child = ManagedChild::spawn("child_zero_handlers", &env);
    let (code, stdout, stderr) = child.wait_exit(Duration::from_secs(10));
    server.stop().await;
    assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("no workflows or activities are registered"),
        "{stdout}"
    );
    assert!(
        stdout.contains("close_hook_invoked"),
        "registered close hook must run even when prepare fails:\n{stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_queue_without_task_queue_config_exits_one() {
    let server = LoopbackTemporalServer::start(MockMode::Idle).await;
    let endpoint = server.endpoint();
    let env = base_local_temporal_env(&endpoint);
    let child = ManagedChild::spawn("child_default_queue_missing_config", &env);
    let (code, stdout, stderr) = child.wait_exit(Duration::from_secs(10));
    server.stop().await;
    assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("temporal_worker.task_queue"),
        "missing default task_queue must fail prepare:\n{stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unmatched_queue_override_exits_one() {
    let server = LoopbackTemporalServer::start(MockMode::Idle).await;
    let endpoint = server.endpoint();
    let mut env = base_local_temporal_env(&endpoint);
    env.push((
        "APP__TEMPORAL_WORKER__QUEUES__TYPO__MAX_CACHED_WORKFLOWS",
        "10",
    ));
    let child = ManagedChild::spawn("child_named_billing_queue", &env);
    let (code, stdout, stderr) = child.wait_exit(Duration::from_secs(10));
    server.stop().await;
    assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("typo"),
        "unmatched queue override must be named and rejected:\n{stdout}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_sigterm_forces_drain_exit_three_without_waiting_graceful_period() {
    let server =
        LoopbackTemporalServer::start_with_shutdown_gate(MockMode::BlockingActivity, true).await;
    let endpoint = server.endpoint();
    let mut env = base_local_temporal_env(&endpoint);
    env.push(("APP__OBSERVABILITY__METRICS__ADDR", "127.0.0.1:0"));
    env.push(("APP__TEMPORAL_WORKER__TASK_QUEUE", "block-queue"));
    env.push(("APP__TEMPORAL_WORKER__GRACEFUL_SHUTDOWN_PERIOD", "45s"));
    env.push(("APP__HTTP__DRAIN_TIMEOUT", "60s"));
    env.push(("APP__HTTP__GRACE_PERIOD", "90s"));

    let child = ManagedChild::spawn("child_blocking_activity", &env);
    let http_line = child.wait_for_line("http listener bound", Duration::from_secs(10));
    let metrics_line = child.wait_for_line("diagnostics listener bound", Duration::from_secs(10));
    let http_addr = extract_bound_addr(&http_line);
    let metrics_addr = extract_bound_addr(&metrics_line);

    let _ = child.wait_for_line("temporal_worker_ready", Duration::from_secs(10));
    let _ = child.wait_for_line("stubborn_activity_started", Duration::from_secs(10));

    let (ready_status, _) = http_get(http_addr, "/health/ready").await;
    assert!(ready_status.starts_with("HTTP/1.1 200 "), "{ready_status}");

    let (metrics_status, metrics_body) = http_get(metrics_addr, "/metrics").await;
    assert!(
        metrics_status.starts_with("HTTP/1.1 200 "),
        "{metrics_status}"
    );
    assert!(
        metrics_body.contains("temporal_"),
        "expected temporal_* series on single /metrics endpoint:\n{metrics_body}"
    );
    assert!(
        !metrics_body.contains("temporal_temporal_"),
        "must never double-prefix Core metrics:\n{metrics_body}"
    );

    let pid = child.pid();
    send_sigterm(pid);
    let _ = child.wait_for_line("temporal_drain_started", Duration::from_secs(5));

    let (draining_status, _) = http_get(http_addr, "/health/ready").await;
    assert!(
        draining_status.starts_with("HTTP/1.1 503 "),
        "first SIGTERM must flip /health/ready to 503 immediately: {draining_status}"
    );

    let second_stop = Instant::now();
    send_sigterm(pid);
    let _ = child.wait_for_line("temporal_drain_forced", Duration::from_secs(5));

    let (forced_metrics_status, forced_metrics_body) = http_get(metrics_addr, "/metrics").await;
    assert!(
        forced_metrics_status.starts_with("HTTP/1.1 200 "),
        "{forced_metrics_status}"
    );
    assert!(
        forced_metrics_body.contains("temporal_drain_forced_total 1"),
        "temporal_drain_forced_total counter must increment on forced drain:\n{forced_metrics_body}"
    );
    server.release_shutdown();
    server.release_polls();

    let (code, stdout, stderr) = child.wait_exit(Duration::from_secs(10));
    let elapsed = second_stop.elapsed();
    let cancel_rpcs = server.cancel_rpcs();
    server.stop().await;

    assert_eq!(code, Some(3), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        elapsed < Duration::from_secs(5),
        "second SIGTERM must bypass 45s graceful_shutdown_period (took {elapsed:?})"
    );
    assert_eq!(
        cancel_rpcs, 0,
        "forced preemption must never send RespondActivityTaskCanceled"
    );
    assert!(
        stdout.contains("temporal_drain_forced"),
        "forced drain log event must be emitted:\n{stdout}"
    );
    assert!(
        stdout.contains("close_hook_invoked"),
        "registered close hook must run during forced drain:\n{stdout}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tight_workflow_replay_does_not_starve_liveness_probe() {
    let server = LoopbackTemporalServer::start(MockMode::CpuSpinWorkflow { spin_ms: 350 }).await;
    let endpoint = server.endpoint();
    let mut env = base_local_temporal_env(&endpoint);
    env.push(("APP__RUNTIME__WORKER_THREADS", "1"));
    env.push(("APP__TEMPORAL_WORKER__TASK_QUEUE", "spin-queue"));

    let child = ManagedChild::spawn("child_cpu_spin_workflow", &env);
    let http_line = child.wait_for_line("http listener bound", Duration::from_secs(10));
    let http_addr = extract_bound_addr(&http_line);
    let _ = child.wait_for_line("cpu_spin_workflow_started", Duration::from_secs(10));

    let probe_start = Instant::now();
    for _ in 0..3 {
        let (status, _) = http_get(http_addr, "/health/live").await;
        assert!(status.starts_with("HTTP/1.1 200 "), "{status}");
    }
    let probe_elapsed = probe_start.elapsed();
    assert!(
        probe_elapsed < Duration::from_millis(200),
        "tight 350ms workflow CPU spin on isolated OS thread must not starve /health/live (took {probe_elapsed:?})"
    );

    let _ = child.wait_for_line("cpu_spin_workflow_finished", Duration::from_secs(5));
    let _ = child.wait_for_line("temporal_worker_ready", Duration::from_secs(5));

    send_sigterm(child.pid());
    server.release_polls();
    let (code, stdout, stderr) = child.wait_exit(Duration::from_secs(10));
    server.stop().await;

    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.contains("close_hook_invoked"), "{stdout}");
    assert!(stdout.contains("custom_bg_stopped"), "{stdout}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leaked_activity_context_exits_one_during_shutdown() {
    let server = LoopbackTemporalServer::start(MockMode::LeakingActivity).await;
    let endpoint = server.endpoint();
    let mut env = base_local_temporal_env(&endpoint);
    env.push(("APP__TEMPORAL_WORKER__TASK_QUEUE", "leak-queue"));

    let child = ManagedChild::spawn("child_leaking_activity", &env);
    let _ = child.wait_for_line("temporal_worker_ready", Duration::from_secs(10));
    let _ = child.wait_for_line("leaking_activity_completed", Duration::from_secs(10));

    send_sigterm(child.pid());
    server.release_polls();

    let (code, stdout, stderr) = child.wait_exit(Duration::from_secs(10));
    server.stop().await;

    assert_eq!(
        code,
        Some(1),
        "leaked ActivityContext preventing finalize_shutdown must exit 1:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("temporal_worker_finish_failed"), "{stdout}");
    assert!(stdout.contains("close_hook_invoked"), "{stdout}");
}

#[expect(
    clippy::exit,
    reason = "child-only process fixtures map temporal_worker::run ExitCode directly to process status"
)]
fn run_child(
    register: impl FnOnce(
        &mut temporal_worker::Registration<'_>,
    ) -> Result<(), temporal_worker::BuildError>,
) -> ! {
    assert!(std::env::var_os("TEMPORAL_WORKER_CHILD_MODE").is_some());
    let exit = temporal_worker::run([OsString::from("temporal-worker")], register);
    let code = if exit == std::process::ExitCode::SUCCESS {
        0
    } else if exit == std::process::ExitCode::from(2) {
        2
    } else if exit == std::process::ExitCode::from(3) {
        3
    } else {
        1
    };
    std::process::exit(code);
}

#[test]
#[ignore = "child-only fixture invoked by zero_handler_registration_exits_one_and_runs_close_hooks"]
fn child_zero_handlers() {
    run_child(|registration| {
        let _ = registration.config();
        let _ = registration.client();
        registration.register_close_hook(Box::new(|_deadline| {
            Box::pin(async {
                tracing::info!("close_hook_invoked");
                CloseOutcome::Complete
            })
        }));
        Ok(())
    });
}

#[test]
#[ignore = "child-only fixture invoked by default_queue_without_task_queue_config_exits_one"]
fn child_default_queue_missing_config() {
    run_child(|registration| {
        registration
            .temporal
            .default_queue()?
            .register_workflow::<CpuSpinWorkflow>()?;
        Ok(())
    });
}

#[test]
#[ignore = "child-only fixture invoked by unmatched_queue_override_exits_one"]
fn child_named_billing_queue() {
    run_child(|registration| {
        registration
            .temporal
            .queue("billing")?
            .register_workflow::<CpuSpinWorkflow>()?;
        Ok(())
    });
}

#[test]
#[ignore = "child-only fixture invoked by second_sigterm_forces_drain_exit_three_without_waiting_graceful_period"]
fn child_blocking_activity() {
    run_child(|registration| {
        let metrics_addr = registration.config().observability().metrics.addr;
        let _ = metrics_addr;
        registration
            .temporal
            .default_queue()?
            .register_activities(StubbornBlockingActivities);
        registration.register_close_hook(Box::new(|_deadline| {
            Box::pin(async {
                tracing::info!("close_hook_invoked");
                CloseOutcome::Complete
            })
        }));
        Ok(())
    });
}

#[test]
#[ignore = "child-only fixture invoked by tight_workflow_replay_does_not_starve_liveness_probe"]
fn child_cpu_spin_workflow() {
    run_child(|registration| {
        registration.add_readiness_probe(health::BoxedProbe::new(ReadyCustomProbe));
        let shutdown_token = registration.shutdown();
        registration.spawn("custom_bg", move |cancel| async move {
            tokio::select! {
                () = cancel.cancelled() => {}
                () = shutdown_token.cancelled() => {}
            }
            tracing::info!("custom_bg_stopped");
        });
        registration
            .temporal
            .default_queue()?
            .register_workflow::<CpuSpinWorkflow>()?;
        registration.register_close_hook(Box::new(|_deadline| {
            Box::pin(async {
                tracing::info!("close_hook_invoked");
                CloseOutcome::Complete
            })
        }));
        Ok(())
    });
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drain_budget_expiry_preempts_stuck_activity_preserves_tail_and_exits_three() {
    let server =
        LoopbackTemporalServer::start_with_shutdown_gate(MockMode::BlockingActivity, true).await;
    let endpoint = server.endpoint();
    let mut env = base_local_temporal_env(&endpoint);
    env.push(("APP__OBSERVABILITY__METRICS__ADDR", "127.0.0.1:0"));
    env.push(("APP__TEMPORAL_WORKER__TASK_QUEUE", "budget-queue"));
    env.push(("APP__TEMPORAL_WORKER__GRACEFUL_SHUTDOWN_PERIOD", "200ms"));
    env.push(("APP__TEMPORAL_WORKER__ACTIVITY_CANCEL_CLEANUP", "200ms"));
    env.push(("APP__TEMPORAL_WORKER__FINALIZE_BUDGET", "1500ms"));
    env.push(("APP__HTTP__DRAIN_TIMEOUT", "1900ms"));
    env.push(("APP__HTTP__GRACE_PERIOD", "18.4s"));

    let child = ManagedChild::spawn("child_blocking_activity", &env);
    let metrics_line = child.wait_for_line("diagnostics listener bound", Duration::from_secs(10));
    let metrics_addr = extract_bound_addr(&metrics_line);
    let _ = child.wait_for_line("temporal_worker_ready", Duration::from_secs(10));
    let _ = child.wait_for_line("stubborn_activity_started", Duration::from_secs(10));

    let first_stop = Instant::now();
    send_sigterm(child.pid());
    server.release_polls();

    let forced_line = child.wait_for_line("temporal_drain_forced", Duration::from_secs(5));
    let forced_elapsed = first_stop.elapsed();
    assert!(
        forced_line.contains("\"reason\":\"budget\""),
        "drain timeout must force preemption with reason=budget:\n{forced_line}"
    );
    assert!(
        forced_elapsed < Duration::from_millis(1200),
        "preemption must fire at graceful_shutdown_period + activity_cancel_cleanup (400ms), not drain_timeout (1900ms); took {forced_elapsed:?}"
    );

    let (metrics_status, metrics_body) = http_get(metrics_addr, "/metrics").await;
    assert!(
        metrics_status.starts_with("HTTP/1.1 200 "),
        "{metrics_status}"
    );
    assert!(
        metrics_body.contains("temporal_drain_forced_total 1"),
        "temporal_drain_forced_total must increment on budget expiry:\n{metrics_body}"
    );

    let _ = child.wait_for_line("temporal_forced_finish_timed_out", Duration::from_secs(5));
    let finish_timeout_elapsed = first_stop.elapsed();
    assert!(
        finish_timeout_elapsed < Duration::from_millis(2700),
        "forced finalize must stay within drain_deadline (1900ms) rather than adding finalize_budget on top (3400ms); took {finish_timeout_elapsed:?}"
    );

    server.release_shutdown();
    let (code, stdout, stderr) = child.wait_exit(Duration::from_secs(10));
    let cancel_rpcs = server.cancel_rpcs();
    server.stop().await;

    assert_eq!(code, Some(3), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(
        cancel_rpcs, 0,
        "budget-expired forced preemption must never send RespondActivityTaskCanceled"
    );
    assert!(
        stdout.contains("close_hook_invoked"),
        "registered close hook must still run inside preserved TEMPORAL_SHUTDOWN_TAIL:\n{stdout}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cooperative_activity_finishes_within_cancel_cleanup_and_exits_zero() {
    let server =
        LoopbackTemporalServer::start_with_shutdown_gate(MockMode::CooperativeActivity, true).await;
    let endpoint = server.endpoint();
    let mut env = base_local_temporal_env(&endpoint);
    env.push(("APP__OBSERVABILITY__METRICS__ADDR", "127.0.0.1:0"));
    env.push(("APP__TEMPORAL_WORKER__TASK_QUEUE", "coop-queue"));
    env.push(("APP__TEMPORAL_WORKER__GRACEFUL_SHUTDOWN_PERIOD", "250ms"));
    env.push(("APP__TEMPORAL_WORKER__ACTIVITY_CANCEL_CLEANUP", "1500ms"));
    env.push(("APP__TEMPORAL_WORKER__FINALIZE_BUDGET", "500ms"));
    env.push(("APP__HTTP__DRAIN_TIMEOUT", "2250ms"));
    env.push(("APP__HTTP__GRACE_PERIOD", "18.75s"));

    let child = ManagedChild::spawn("child_cooperative_activity", &env);
    let metrics_line = child.wait_for_line("diagnostics listener bound", Duration::from_secs(10));
    let metrics_addr = extract_bound_addr(&metrics_line);
    let _ = child.wait_for_line("temporal_worker_ready", Duration::from_secs(10));
    let _ = child.wait_for_line("cooperative_activity_started", Duration::from_secs(10));

    send_sigterm(child.pid());
    server.release_polls();

    let _ = child.wait_for_line("cooperative_activity_cancelled", Duration::from_secs(5));
    let (metrics_status, metrics_body) = http_get(metrics_addr, "/metrics").await;
    assert!(
        metrics_status.starts_with("HTTP/1.1 200 "),
        "{metrics_status}"
    );
    assert!(
        !metrics_body.lines().any(|line| {
            line.starts_with("temporal_drain_forced_total ")
                && line != "temporal_drain_forced_total 0"
        }),
        "cooperative drain must not increment temporal_drain_forced_total:\n{metrics_body}"
    );
    server.release_shutdown();

    let (code, stdout, stderr) = child.wait_exit(Duration::from_secs(10));
    let cancel_rpcs = server.cancel_rpcs();
    let fail_rpcs = server.fail_rpcs();
    server.stop().await;

    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("cooperative_activity_cancelled"),
        "activity must observe WorkerShutdown cancellation:\n{stdout}"
    );
    assert!(
        stdout.contains("temporal_drain_completed"),
        "cooperative drain must complete gracefully:\n{stdout}"
    );
    assert!(
        !stdout.contains("temporal_drain_forced"),
        "cooperative activity finishing within cancel cleanup must not force drain:\n{stdout}"
    );
    assert_eq!(cancel_rpcs, 0);
    assert_eq!(fail_rpcs, 1);
    assert!(stdout.contains("close_hook_invoked"), "{stdout}");
}

#[test]
#[ignore = "child-only fixture invoked by cooperative_activity_finishes_within_cancel_cleanup_and_exits_zero"]
fn child_cooperative_activity() {
    run_child(|registration| {
        registration
            .temporal
            .default_queue()?
            .register_activities(CooperativeShutdownActivities);
        registration.register_close_hook(Box::new(|_deadline| {
            Box::pin(async {
                tracing::info!("close_hook_invoked");
                CloseOutcome::Complete
            })
        }));
        Ok(())
    });
}

#[test]
#[ignore = "child-only fixture invoked by leaked_activity_context_exits_one_during_shutdown"]
fn child_leaking_activity() {
    let leaked = Arc::new(Mutex::new(None));
    let leaked_hold = Arc::clone(&leaked);
    run_child(move |registration| {
        registration
            .temporal
            .default_queue()?
            .register_activities(LeakingActivities {
                leaked: Arc::clone(&leaked),
            });
        registration.register_close_hook(Box::new(move |_deadline| {
            Box::pin(async move {
                tracing::info!("close_hook_invoked");
                drop(leaked_hold.lock().unwrap().take());
                CloseOutcome::Complete
            })
        }));
        Ok(())
    });
}
