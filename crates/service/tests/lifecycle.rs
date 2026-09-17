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
}

impl Service {
    fn spawn(env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_service"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("APP__HTTP__ADDR", "127.0.0.1:0")
            .env("APP__OBSERVABILITY__METRICS__ADDR", "127.0.0.1:0")
            .env("APP__HTTP__READINESS_PROPAGATION_DELAY", "300ms")
            .env("APP__HTTP__SHUTDOWN_TIMEOUT", "3s")
            .env("APP__HTTP__REQUEST_TIMEOUT", "2s")
            .env("APP__LOG__FORMAT", "json")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn service binary");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, lines) = mpsc::channel();
        // Drain stdout for the process lifetime so the child never blocks
        // on a full pipe.
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
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

    fn wait(mut self) -> (Option<i32>, String) {
        let status = self.child.wait().expect("wait for service");
        let mut stderr = String::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
        }
        (status.code(), stderr)
    }
}

fn get(url: &str) -> Result<(u16, String), ureq::Error> {
    match ureq::get(url).call() {
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
        "axum_http_requests_total",
        "axum_http_requests_duration_seconds",
        "process_resident_memory_bytes",
        "service_startup_trace_exporter_active 0",
    ] {
        assert!(
            metrics.contains(family),
            "metrics missing {family}:\n{metrics}"
        );
    }
    assert!(
        metrics.contains("endpoint=\"<unmatched>\""),
        "404 must be labelled with the bounded unmatched route:\n{metrics}"
    );

    let started = Instant::now();
    service.terminate();
    assert!(
        poll_until(&ready, 503, Duration::from_millis(250)),
        "readiness must flip to 503 before the listener closes"
    );
    let (code, stderr) = service.wait();
    let took = started.elapsed();
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_secs(5),
        "drain timing off: {took:?}"
    );
}

#[test]
fn invalid_configuration_exits_one_with_the_key_named() {
    let service = Service::spawn(&[("APP__HTTP__REQUEST_TIMEOUT", "not-a-duration")]);
    let (code, stderr) = service.wait();
    assert_eq!(code, Some(1));
    assert!(stderr.contains("request_timeout"), "stderr: {stderr}");
}

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
        ("APP__HTTP__SHUTDOWN_TIMEOUT", "5s"),
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
