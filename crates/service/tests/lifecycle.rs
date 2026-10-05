//! Process-level proof of the built binary: startup, probes, metrics,
//! SIGTERM drain, and exit codes. Cargo builds the binary and sets
//! `CARGO_BIN_EXE_service` for integration tests.

// Integration tests are test code; the workspace's production lint levels
// for unwrap/expect/panic do not apply to them.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

struct Service {
    child: Child,
    lines: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Service {
    fn command(env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_service"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("APP__HTTP__ADDR", "127.0.0.1:0")
            .env("APP__OBSERVABILITY__METRICS__ADDR", "127.0.0.1:0")
            .env("APP__HTTP__READINESS_PROPAGATION_DELAY", "300ms")
            .env("APP__HTTP__DRAIN_TIMEOUT", "3s")
            .env("APP__HTTP__REQUEST_TIMEOUT", "2s")
            .env("APP__LOG__FORMAT", "json")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.envs(env.iter().copied());
        command
    }

    fn spawn(env: &[(&str, &str)]) -> Self {
        let mut command = Self::command(env);
        let mut child = command.spawn().expect("spawn service binary");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, lines) = mpsc::channel();
        // Drain stdout for the process lifetime so the child never blocks
        // on a full pipe.
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            lines,
            reader: Some(reader),
        }
    }

    /// Wait for a log record with `message`, returning its JSON.
    fn await_record(&self, message: &str) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("no {message:?} record before the deadline"));
            let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
                panic!("stdout must be one JSON object per line, got {line:?}");
            };
            if record["message"] == message {
                return record;
            }
        }
    }

    fn terminate(&self) {
        kill(
            Pid::from_raw(self.child.id().cast_signed()),
            Signal::SIGTERM,
        )
        .expect("send SIGTERM");
    }

    fn wait(self) -> (Option<i32>, String) {
        let (code, _, stderr) = self.wait_output();
        (code, stderr)
    }

    fn wait_output(mut self) -> (Option<i32>, String, String) {
        let status = wait_child(&mut self.child, Duration::from_secs(20));
        self.reader
            .take()
            .expect("stdout reader")
            .join()
            .expect("join stdout reader");
        let stdout = self.lines.try_iter().collect::<Vec<_>>().join("\n");
        let mut stderr = String::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
        }
        (status.code(), stdout, stderr)
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn wait_child(child: &mut Child, budget: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not exit within {budget:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn get(url: &str) -> Result<(u16, String), ureq::Error> {
    match ureq::get(url)
        .config()
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .call()
    {
        Ok(mut response) => {
            let status = response.status().as_u16();
            let body = response.body_mut().read_to_string().unwrap_or_default();
            Ok((status, body))
        }
        Err(ureq::Error::StatusCode(status)) => Ok((status, String::new())),
        Err(err) => Err(err),
    }
}

fn poll_until(url: &str, want: u16, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Ok((status, _)) = get(url)
            && status == want
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn serves_probes_and_metrics_then_drains_on_sigterm_with_exit_zero() {
    let service = Service::spawn(&[]);
    let api = service.await_record("http listener bound")["addr"]
        .as_str()
        .expect("addr field")
        .to_owned();
    let diagnostics = service.await_record("diagnostics listener bound")["addr"]
        .as_str()
        .expect("addr field")
        .to_owned();
    service.await_record("service_ready");

    let ready = format!("http://{api}/health/ready");
    assert!(
        poll_until(&ready, 200, Duration::from_secs(5)),
        "service never became ready"
    );
    let (status, body) = get(&format!("http://{api}/health/live")).unwrap();
    assert_eq!((status, body.as_str()), (200, "ok"));

    let (status, _) = get(&format!("http://{api}/missing")).unwrap();
    assert_eq!(status, 404);

    let (status, metrics) = get(&format!("http://{diagnostics}/metrics")).unwrap();
    assert_eq!(status, 200);
    for family in [
        "http_server_request_duration_seconds_count",
        "http_server_active_requests",
        "process_resident_memory_bytes",
        "service_startup_trace_exporter_active 0",
        "readiness_checks_total{outcome=\"ok\"}",
        "readiness_ready 1",
    ] {
        assert!(
            metrics.contains(family),
            "metrics missing {family}:\n{metrics}"
        );
    }
    assert!(
        metrics.contains("http_route=\"<unmatched>\""),
        "404 must be labelled with the bounded unmatched route:\n{metrics}"
    );

    let started = Instant::now();
    service.terminate();
    assert!(
        poll_until(&ready, 503, Duration::from_millis(250)),
        "readiness must flip to 503 before the listener closes"
    );
    let (code, stdout, stderr) = service.wait_output();
    let took = started.elapsed();
    let records: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let trace = records
        .iter()
        .find(|record| record["message"] == "trace_shutdown_completed")
        .expect("trace completion");
    assert_eq!(trace["delivery_confirmed"], false);
    let final_record = records.last().expect("terminal record");
    assert_eq!(final_record["message"], "shutdown_finishing");
    assert_eq!(final_record["logger_pending"], true);
    assert!(
        stderr.is_empty(),
        "post-install cleanup writes only through the logger: {stderr}"
    );
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_secs(5),
        "drain timing off: {took:?}"
    );
}

#[test]
fn a_full_application_listener_refuses_liveness_and_the_diagnostics_listener_answers() {
    let service = Service::spawn(&[
        ("APP__HTTP__MAX_CONNECTIONS", "2"),
        ("APP__HTTP__MAX_IN_FLIGHT", "2"),
    ]);
    let api = service.await_record("http listener bound")["addr"]
        .as_str()
        .expect("addr field")
        .to_owned();
    let diagnostics = service.await_record("diagnostics listener bound")["addr"]
        .as_str()
        .expect("addr field")
        .to_owned();
    service.await_record("service_ready");
    let app_live = format!("http://{api}/health/live");
    let diagnostics_live = format!("http://{diagnostics}/health/live");

    // Idle connections hold every permit until the header timeout (5 s).
    let held: Vec<std::net::TcpStream> = (0..2)
        .map(|_| std::net::TcpStream::connect(&api).expect("hold a connection"))
        .collect();
    let deadline = Instant::now() + Duration::from_secs(2);
    while get(&app_live).is_ok() {
        assert!(
            Instant::now() < deadline,
            "the application listener must refuse a connection over its cap"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let (status, body) = get(&diagnostics_live).expect("liveness on the diagnostics listener");
    assert_eq!((status, body.as_str()), (200, "ok"));
    let (status, _) = get(&format!("http://{diagnostics}/health/ready")).unwrap();
    assert_eq!(
        status, 404,
        "readiness stays on the listener the traffic uses"
    );

    drop(held);
    assert!(
        poll_until(&app_live, 200, Duration::from_secs(2)),
        "the application listener must answer again once connections close"
    );

    service.terminate();
    let (code, stderr) = service.wait();
    assert_eq!(code, Some(0), "stderr: {stderr}");
}

#[test]
fn invalid_configuration_exits_one_with_the_key_named() {
    let service = Service::spawn(&[("APP__HTTP__REQUEST_TIMEOUT", "not-a-duration")]);
    let (code, stderr) = service.wait();
    assert_eq!(code, Some(1));
    assert!(stderr.contains("request_timeout"), "stderr: {stderr}");
}

// template:begin cache:service-cache-lifecycle-admission
#[test]
fn a_cache_outage_at_startup_still_becomes_ready() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("closed port");
    let port = listener.local_addr().expect("closed port").port();
    drop(listener);
    let dsn = format!("redis://127.0.0.1:{port}");
    let service = Service::spawn(&[
        ("APP__APP__ENV", "local"),
        ("APP__CACHE__DSN", &dsn),
        ("APP__CACHE__ALLOW_PLAINTEXT", "true"),
        ("APP__CACHE__ALLOW_UNAUTHENTICATED", "true"),
    ]);
    let startup = service.await_record("cache_unavailable_at_startup");
    let reason = startup["reason"].as_str().expect("reason field");
    assert!(!reason.is_empty(), "{startup}");
    let api = service.await_record("http listener bound")["addr"]
        .as_str()
        .expect("addr field")
        .to_owned();
    service.await_record("service_ready");
    let ready = format!("http://{api}/health/ready");
    assert!(
        poll_until(&ready, 200, Duration::from_secs(5)),
        "a cache outage must not keep the service from becoming ready"
    );
    service.terminate();
    let (code, stderr) = service.wait();
    assert_eq!(code, Some(0), "stderr: {stderr}");
}

#[test]
fn production_plaintext_cache_dsn_exits_before_the_listener() {
    let mut service = Service::spawn(&[
        ("APP__APP__ENV", "production"),
        ("APP__CACHE__DSN", "redis://127.0.0.1:6379"),
    ]);
    let deadline = Instant::now() + Duration::from_secs(15);
    while service.child.try_wait().expect("poll startup").is_none() {
        if Instant::now() >= deadline {
            let _ = service.child.kill();
            let _ = service.child.wait();
            panic!("plaintext cache DSN did not exit before the listener");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let (code, stdout, stderr) = service.wait_output();
    assert_eq!(code, Some(1), "stdout: {stdout}; stderr: {stderr}");
    assert!(stdout.contains("plaintext"), "startup refusal: {stdout}");
    assert!(stdout.contains("shutdown_finishing"), "{stdout}");
    assert!(stderr.is_empty(), "duplicate failure fallback: {stderr}");
}

#[test]
fn a_stop_signal_during_startup_ends_it_before_a_listener_is_bound() {
    // Accepts the connection and never answers, so the cache startup check
    // holds startup for its whole bound.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("silent listener");
    let port = silent.local_addr().expect("silent listener").port();
    let dsn = format!("redis://127.0.0.1:{port}");
    let service = Service::spawn(&[
        ("APP__APP__ENV", "local"),
        ("APP__CACHE__DSN", &dsn),
        ("APP__CACHE__ALLOW_PLAINTEXT", "true"),
        ("APP__CACHE__ALLOW_UNAUTHENTICATED", "true"),
        ("APP__CACHE__COMMAND_TIMEOUT", "1s"),
    ]);
    service.await_record("service_starting");
    service.terminate();
    let mut messages = Vec::new();
    while let Ok(line) = service.lines.recv_timeout(Duration::from_secs(20)) {
        let record: serde_json::Value = serde_json::from_str(&line).expect("JSON log record");
        messages.push(record["message"].as_str().unwrap_or_default().to_owned());
    }
    let (code, stderr) = service.wait();
    drop(silent);
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(
        messages
            .iter()
            .any(|message| message == "shutdown_finishing"),
        "{messages:?}"
    );
    for skipped in ["http listener bound", "service_ready", "readiness_disabled"] {
        assert!(
            !messages.iter().any(|message| message == skipped),
            "a stopped startup must not reach {skipped:?}: {messages:?}"
        );
    }
}
// template:end cache:service-cache-lifecycle-admission

// template:begin object-storage:service-object-storage-lifecycle-admission
#[test]
fn an_unreachable_bucket_still_becomes_ready_without_a_request() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("closed port");
    let port = listener.local_addr().expect("closed port").port();
    drop(listener);
    let endpoint = format!("http://127.0.0.1:{port}");
    let service = Service::spawn(&[
        ("APP__APP__ENV", "local"),
        ("APP__OBJECT_STORAGE__PROVIDER", "local"),
        ("APP__OBJECT_STORAGE__ENDPOINT", &endpoint),
        ("APP__OBJECT_STORAGE__BUCKET", "template-bucket"),
        ("APP__OBJECT_STORAGE__ACCESS_KEY_ID", "template"),
        (
            "APP__OBJECT_STORAGE__SECRET_ACCESS_KEY",
            "hunter2-object-storage",
        ),
    ]);
    let configured = service.await_record("object_storage_configured");
    assert_eq!(
        configured["object_storage.provider"], "local",
        "{configured}"
    );
    let api = service.await_record("http listener bound")["addr"]
        .as_str()
        .expect("addr field")
        .to_owned();
    service.await_record("service_ready");
    let ready = format!("http://{api}/health/ready");
    assert!(
        poll_until(&ready, 200, Duration::from_secs(5)),
        "object storage is not a readiness dependency"
    );
    service.terminate();
    let (code, stderr) = service.wait();
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(
        !stderr.contains("hunter2-object-storage"),
        "the secret must not reach logs: {stderr}"
    );
}

#[test]
fn production_emulator_provider_exits_before_the_listener() {
    let (code, stderr) = Service::spawn(&[
        ("APP__APP__ENV", "production"),
        ("APP__OBJECT_STORAGE__PROVIDER", "local"),
        ("APP__OBJECT_STORAGE__ENDPOINT", "http://127.0.0.1:7070"),
        ("APP__OBJECT_STORAGE__BUCKET", "template-bucket"),
        ("APP__OBJECT_STORAGE__ACCESS_KEY_ID", "template"),
        (
            "APP__OBJECT_STORAGE__SECRET_ACCESS_KEY",
            "hunter2-object-storage",
        ),
    ])
    .wait();
    assert_eq!(code, Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("object_storage.provider"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("hunter2-object-storage"),
        "stderr: {stderr}"
    );
}

#[test]
fn an_r2_endpoint_outside_cloudflare_exits_before_the_listener() {
    let (code, stdout, stderr) = Service::spawn(&[
        ("APP__APP__ENV", "production"),
        ("APP__OBJECT_STORAGE__PROVIDER", "cloudflare_r2"),
        (
            "APP__OBJECT_STORAGE__ENDPOINT",
            "https://storage.example.com",
        ),
        ("APP__OBJECT_STORAGE__BUCKET", "template-bucket"),
        ("APP__OBJECT_STORAGE__ACCESS_KEY_ID", "template"),
        (
            "APP__OBJECT_STORAGE__SECRET_ACCESS_KEY",
            "hunter2-object-storage",
        ),
    ])
    .wait_output();
    assert_eq!(code, Some(1), "stderr: {stderr}");
    // Provider endpoint admission runs after the shared logger is installed.
    let records: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let failure = records
        .iter()
        .find(|record| record["message"] == "service failed")
        .expect("provider refusal must be logged before cleanup");
    assert!(
        failure["error"]
            .as_str()
            .is_some_and(|error| error.contains("object_storage.endpoint")),
        "{failure}"
    );
    assert!(stdout.contains("shutdown_finishing"), "{stdout}");
    assert!(!stdout.contains("http listener bound"), "{stdout}");
    assert!(!stdout.contains("hunter2-object-storage"), "{stdout}");
    assert!(stderr.is_empty(), "duplicate fallback: {stderr}");
}
// template:end object-storage:service-object-storage-lifecycle-admission

// template:begin inbound-webhooks:service-webhooks-lifecycle-tests
#[test]
fn active_inbound_webhook_endpoint_refuses_without_postgres_before_listener_admission() {
    let (code, stdout, stderr) = Service::spawn(&[
        (
            "APP__INBOUND_WEBHOOKS__ENDPOINTS__PARTNER__ACTIVE_KEY",
            "partner_v1",
        ),
        (
            "APP__INBOUND_WEBHOOKS__SECRETS__PARTNER_V1",
            "whsec_Q0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0M=",
        ),
    ])
    .wait_output();
    assert_eq!(code, Some(1));
    // Cross-section configuration validation refuses before telemetry exists.
    assert!(stderr.contains("postgres.enabled"), "stderr: {stderr}");
    assert!(
        stdout.is_empty(),
        "pre-telemetry failure must not install logging: {stdout}"
    );
    assert!(
        !stderr.contains("Q0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0M="),
        "a webhook secret must not reach startup diagnostics: {stderr}"
    );
}

#[test]
fn inert_inbound_webhook_route_rejects_unknown_endpoint_without_signature_work() {
    let service = Service::spawn(&[]);
    let api = service.await_record("http listener bound")["addr"]
        .as_str()
        .expect("addr field")
        .to_owned();
    service.await_record("service_ready");

    let status = match ureq::post(&format!("http://{api}/webhooks/unknown")).send_empty() {
        Ok(response) => response.status().as_u16(),
        Err(ureq::Error::StatusCode(status)) => status,
        Err(error) => panic!("webhook response: {error}"),
    };
    assert_eq!(status, 404);

    service.terminate();
    let (code, stderr) = service.wait();
    assert_eq!(code, Some(0), "stderr: {stderr}");
}
// template:end inbound-webhooks:service-webhooks-lifecycle-tests

#[test]
fn unknown_key_and_malformed_env_exit_one() {
    let (code, stderr) = Service::spawn(&[("APP__HTTP__BOGUS", "1")]).wait();
    assert_eq!(code, Some(1));
    assert!(stderr.contains("bogus"), "stderr: {stderr}");

    let (code, stderr) = Service::spawn(&[("APP____X", "1")]).wait();
    assert_eq!(code, Some(1));
    assert!(stderr.contains("APP____X"), "stderr: {stderr}");
}

#[test]
fn grace_period_must_cover_the_teardown_tail() {
    let (code, stderr) = Service::spawn(&[
        ("APP__HTTP__GRACE_PERIOD", "10s"),
        ("APP__HTTP__DRAIN_TIMEOUT", "5s"),
    ])
    .wait();
    assert_eq!(code, Some(1));
    assert!(stderr.contains("http.grace_period"), "stderr: {stderr}");
}

#[test]
fn ambient_otlp_credentials_are_refused_under_a_typed_endpoint() {
    let (code, stderr) = Service::spawn(&[
        (
            "APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_ENDPOINT",
            "http://127.0.0.1:1/v1/traces",
        ),
        ("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer x"),
    ])
    .wait();
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("OTEL_EXPORTER_OTLP_HEADERS"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("Bearer x"),
        "credential must not be echoed: {stderr}"
    );
}

// template:begin authn:service-lifecycle-disabled-authn
#[test]
fn disabled_authentication_leaves_public_probes_unaffected() {
    let service = Service::spawn(&[("APP__AUTHN__MODE", "none")]);
    let api = service.await_record("http listener bound")["addr"]
        .as_str()
        .expect("addr field")
        .to_owned();
    service.await_record("service_ready");

    let response = ureq::get(&format!("http://{api}/health/live"))
        .header("Authorization", "not a bearer credential")
        .call()
        .expect("public liveness response");
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.into_body().read_to_string().unwrap(), "ok");

    service.terminate();
    let (code, stderr) = service.wait();
    assert_eq!(code, Some(0), "stderr: {stderr}");
}
// template:end authn:service-lifecycle-disabled-authn

fn available_address() -> String {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string()
}

#[test]
fn stopped_stdout_does_not_block_requests_or_process_exit() {
    let api = available_address();
    let diagnostics = available_address();
    let (_, lines) = mpsc::channel();
    let mut service = Service {
        child: Service::command(&[
            ("APP__HTTP__ADDR", &api),
            ("APP__OBSERVABILITY__METRICS__ADDR", &diagnostics),
            ("APP__RUNTIME__WORKER_THREADS", "2"),
        ])
        .spawn()
        .unwrap(),
        lines,
        reader: None,
    };
    assert!(poll_until(
        &format!("http://{api}/health/ready"),
        200,
        Duration::from_secs(10)
    ));
    // Keep the OS pipe open and completely unread. Queue saturation, observed
    // through the independent metrics listener, proves the writer cannot drain.
    for _ in 0..2048 {
        assert_eq!(get(&format!("http://{api}/missing")).unwrap().0, 404);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, metrics) = get(&format!("http://{diagnostics}/metrics")).unwrap();
        let dropped = metrics
            .lines()
            .find_map(|line| {
                line.strip_prefix("telemetry_log_records_dropped_total{reason=\"queue_full\"} ")
                    .and_then(|value| value.parse::<u64>().ok())
            })
            .unwrap_or_default();
        if dropped > 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the unread sink never saturated: {metrics}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    service.terminate();
    let status = wait_child(&mut service.child, Duration::from_secs(8));
    assert_eq!(
        status.code(),
        Some(3),
        "an undrained logger makes normal shutdown incomplete"
    );
    assert!(
        service.child.stdout.is_some(),
        "stdout remained undrained at the exit assertion"
    );
}

#[test]
fn inbound_private_values_are_absent_from_real_json_and_text_logs() {
    use std::io::{Read, Write};
    for format in ["json", "text"] {
        let api = available_address();
        let service = Service::spawn(&[
            ("APP__HTTP__ADDR", &api),
            ("APP__LOG__FORMAT", format),
            ("APP__LOG__LEVEL", "trace"),
        ]);
        assert!(poll_until(
            &format!("http://{api}/health/ready"),
            200,
            Duration::from_secs(10)
        ));
        for method in ["GET", "PRIVATE_METHOD_SENTINEL"] {
            let mut socket = std::net::TcpStream::connect(&api).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            write!(socket, "{method} /private_path_sentinel?private_query_sentinel HTTP/1.1\r\nHost: private_host_sentinel\r\nUser-Agent: private_agent_sentinel\r\nX-Request-Id: retained-correlation\r\ntraceparent: 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01\r\nConnection: close\r\n\r\n").unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).unwrap();
            assert!(
                response.starts_with("HTTP/1.1 404"),
                "HTTP fixture did not return the expected 404 response"
            );
        }
        service.terminate();
        let (code, stdout, stderr) = service.wait_output();
        assert_eq!(code, Some(0), "{stdout} {stderr}");
        for private in [
            "private_path_sentinel",
            "private_query_sentinel",
            "private_host_sentinel",
            "private_agent_sentinel",
            "PRIVATE_METHOD_SENTINEL",
        ] {
            assert!(
                !stdout.contains(private) && !stderr.contains(private),
                "{format} leaked {private}: {stdout} {stderr}"
            );
        }
        assert!(
            stdout.contains("retained-correlation"),
            "{format}: {stdout}"
        );
        assert!(
            stdout.contains("4bf92f3577b34da6a3ce929d0e0e4736"),
            "{format}: {stdout}"
        );
        assert!(stdout.contains("_OTHER"), "{format}: {stdout}");
        if format == "json" {
            let accesses: Vec<serde_json::Value> = stdout
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .filter(|record: &serde_json::Value| record["message"] == "http_request")
                .collect();
            assert_eq!(accesses.len(), 2);
            for record in accesses {
                assert_eq!(record["route"], "<unmatched>");
                assert_eq!(record["request_id"], "retained-correlation");
                assert_eq!(record["trace_id"], "4bf92f3577b34da6a3ce929d0e0e4736");
            }
        }
    }
}
