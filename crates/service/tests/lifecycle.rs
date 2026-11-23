#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a broken fixture or setup fails the test"
)]

use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

mod common;

use common::{baseline_env, get, poll_until, spawn_stdout_lines, terminate};

#[expect(
    clippy::disallowed_types,
    reason = "process lifecycle fixture owns the spawned service Child handle"
)]
struct Service {
    child: Child,
    lines: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "process lifecycle fixture spawns and polls the compiled service binary synchronously"
)]
#[warn(clippy::let_underscore_must_use)]
impl Service {
    fn command(env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_service"));
        baseline_env(&mut command)
            .env("APP__HTTP__DRAIN_TIMEOUT", "3s")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.envs(env.iter().copied());
        command
    }

    fn spawn(env: &[(&str, &str)]) -> Self {
        let mut command = Self::command(env);
        let mut child = command.spawn().expect("spawn service binary");
        let (lines, reader) = spawn_stdout_lines(&mut child);
        Self {
            child,
            lines,
            reader: Some(reader),
        }
    }

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

    fn await_addr(&self, message: &str) -> String {
        self.await_record(message)["addr"]
            .as_str()
            .expect("addr field")
            .to_owned()
    }

    fn terminate(&self) {
        terminate(&self.child);
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
        std::io::Read::read_to_string(
            &mut self.child.stderr.take().expect("piped service stderr"),
            &mut stderr,
        )
        .expect("read service stderr as UTF-8");
        (status.code(), stdout, stderr)
    }
}

fn records(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "observes the standalone CLI's output streams and exit status"
)]
fn help_goes_to_stdout_and_usage_errors_go_to_stderr() {
    for (argument, code) in [("--help", 0), ("--unknown", 2)] {
        let output = Service::command(&[]).arg(argument).output().unwrap();
        assert_eq!(output.status.code(), Some(code));
        let (message, silent) = if code == 0 {
            (output.stdout, output.stderr)
        } else {
            (output.stderr, output.stdout)
        };
        assert!(String::from_utf8_lossy(&message).contains("Usage:"));
        assert_eq!(silent.as_slice(), b"");
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Drop reaps the owned service child process on teardown"
)]
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

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
)]
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

#[test]
fn serves_probes_and_metrics_then_drains_on_sigterm_with_exit_zero() {
    let service = Service::spawn(&[]);
    let api = service.await_addr("http listener bound");
    let diagnostics = service.await_addr("diagnostics listener bound");
    service.await_record("service_ready");

    let ready = format!("http://{api}/health/ready");
    let serving =
        poll_until(&ready, 200, Duration::from_secs(5)).expect("service never became ready");
    let (status, body) = get(&format!("http://{api}/health/live")).unwrap();
    assert_eq!((status, body.as_str()), (200, "ok"));
    assert!(!serving.headers().contains_key("connection"));

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
        "http_server_shed_requests_total 0",
    ] {
        assert!(
            metrics.contains(family),
            "metrics missing {family}:\n{metrics}"
        );
    }
    let unmatched = metrics
        .lines()
        .find(|line| {
            line.starts_with("http_server_request_duration_seconds_count{")
                && line.contains("http_response_status_code=\"404\"")
        })
        .expect("the unmatched 404 is observed");
    assert!(unmatched.contains("url_scheme=\"http\""), "{unmatched}");
    assert!(!unmatched.contains("http_route="), "{unmatched}");

    let started = Instant::now();
    service.terminate();
    let draining = poll_until(&ready, 503, Duration::from_millis(250))
        .expect("readiness must flip to 503 before the listener closes");
    assert_eq!(
        draining.headers()["connection"],
        "close",
        "a keep-alive client is told to reconnect while the listener still serves"
    );
    let (code, stdout, stderr) = service.wait_output();
    let took = started.elapsed();
    let records = records(&stdout);
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
#[expect(
    clippy::disallowed_methods,
    reason = "synchronous process fixture owns loopback connections and bounded child-state polling through teardown"
)]
fn a_full_application_listener_refuses_probes_with_or_without_diagnostics() {
    for diagnostics_enabled in [true, false] {
        let service = Service::spawn(&[
            (
                "APP__OBSERVABILITY__METRICS__ADDR",
                if diagnostics_enabled {
                    "127.0.0.1:0"
                } else {
                    ""
                },
            ),
            ("APP__HTTP__MAX_CONNECTIONS", "2"),
            ("APP__HTTP__MAX_IN_FLIGHT", "2"),
        ]);
        let api = service.await_addr("http listener bound");
        let diagnostics =
            diagnostics_enabled.then(|| service.await_addr("diagnostics listener bound"));
        service.await_record("service_ready");
        let app_live = format!("http://{api}/health/live");

        #[expect(
            clippy::disallowed_types,
            reason = "synchronous process fixture retains these connections until its listener-capacity assertion ends"
        )]
        let held: Vec<std::net::TcpStream> = Vec::from_fn(2, |_| {
            std::net::TcpStream::connect(&api).expect("hold a connection")
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while get(&app_live).is_ok() {
            assert!(
                Instant::now() < deadline,
                "the application listener must refuse a connection over its cap"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        assert!(
            get(&format!("http://{api}/health/ready")).is_err(),
            "readiness shares the full application listener"
        );
        if let Some(diagnostics) = diagnostics {
            let (status, body) = get(&format!("http://{diagnostics}/health/live"))
                .expect("liveness on the diagnostics listener");
            assert_eq!((status, body.as_str()), (200, "ok"));
            let (status, _) = get(&format!("http://{diagnostics}/health/ready")).unwrap();
            assert_eq!(
                status, 404,
                "readiness stays on the listener the traffic uses"
            );
        }

        drop(held);
        assert!(
            poll_until(&app_live, 200, Duration::from_secs(2)).is_some(),
            "the application listener must answer again once connections close"
        );

        service.terminate();
        let (code, stderr) = service.wait();
        assert_eq!(code, Some(0), "stderr: {stderr}");
    }
}

#[test]
fn invalid_configuration_exits_one_with_the_key_named() {
    let service = Service::spawn(&[("APP__HTTP__REQUEST_TIMEOUT", "not-a-duration")]);
    let (code, stderr) = service.wait();
    assert_eq!(code, Some(1));
    assert!(stderr.contains("request_timeout"), "stderr: {stderr}");
}

// template:begin grpc-authn:service-grpc-verifier-lifecycle
#[test]
fn missing_grpc_verifier_lists_only_the_supported_modes() {
    let (code, stdout, stderr) = Service::spawn(&[
        ("APP__APP__ENV", "local"),
        ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
        ("APP__GRPC__ENABLED", "true"),
        ("APP__GRPC__ADDR", "127.0.0.1:0"),
        ("APP__GRPC__SECURITY", "plaintext"),
        ("APP__GRPC__REQUEST_TIMEOUT", "2s"),
    ])
    .wait_output();
    assert_eq!(code, Some(1), "stdout: {stdout}\nstderr: {stderr}");
    let records = records(&stdout);
    let failure = records
        .iter()
        .find(|record| record["message"] == "service failed")
        .expect("missing verifier must be logged as a startup failure");
    let modes = expected_grpc_verifier_modes();
    assert_eq!(
        failure["error"].as_str(),
        Some(
            format!("configuration is invalid: grpc.enabled: gRPC business calls require {modes}")
                .as_str()
        )
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "inspects retained workspace profile files in a synchronous process test"
)]
fn expected_grpc_verifier_modes() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut modes = Vec::new();
    if root.join("crates/infra-bearerauthn/src/jwt.rs").is_file() {
        modes.push("authn.mode = oidc-jwt");
    }
    if root
        .join("crates/infra-bearerauthn/src/introspection.rs")
        .is_file()
    {
        modes.push("authn.mode = oidc-introspection");
    }
    if root.join("crates/infra-serviceauth/Cargo.toml").is_file() {
        modes.push("service_auth.mode = jwt");
    }
    assert!(
        !modes.is_empty(),
        "expected at least one retained gRPC verifier mode"
    );
    modes.join(" or ")
}
// template:end grpc-authn:service-grpc-verifier-lifecycle

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "broken-stderr fixture spawns the service binary directly with a closed pipe"
)]
fn invalid_configuration_exits_one_with_a_broken_stderr_pipe() {
    let (reader, writer) = std::io::pipe().expect("stderr pipe");
    drop(reader);
    let mut child = Service::command(&[("APP__HTTP__REQUEST_TIMEOUT", "not-a-duration")])
        .stdout(Stdio::null())
        .stderr(writer)
        .spawn()
        .expect("spawn service binary");
    let status = wait_child(&mut child, Duration::from_secs(5));
    assert_eq!(status.code(), Some(1));
}

// template:begin cache:service-cache-lifecycle-admission
#[test]
fn a_cache_outage_at_startup_still_becomes_ready() {
    let dsn = format!("redis://{}", available_address());
    let service = Service::spawn(&[
        ("APP__APP__ENV", "local"),
        ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
        ("APP__CACHE__DSN", &dsn),
        ("APP__CACHE__ALLOW_PLAINTEXT", "true"),
        ("APP__CACHE__ALLOW_UNAUTHENTICATED", "true"),
        ("APP__CACHE__COMMAND_TIMEOUT", "1s"),
    ]);
    let api = service.await_addr("http listener bound");
    service.await_record("service_ready");
    let ready = format!("http://{api}/health/ready");
    assert!(
        poll_until(&ready, 200, Duration::from_secs(5)).is_some(),
        "a cache outage must not keep the service from becoming ready"
    );
    let outage = service.await_record("cache_connection_failed");
    assert_eq!(outage["error.type"], "io", "{outage}");
    service.terminate();
    let (code, stderr) = service.wait();
    assert_eq!(code, Some(0), "stderr: {stderr}");
}

#[test]
fn production_plaintext_cache_dsn_exits_before_the_listener() {
    let service = Service::spawn(&[
        ("APP__APP__ENV", "production"),
        ("APP__CACHE__DSN", "redis://127.0.0.1:6379"),
        ("APP__CACHE__COMMAND_TIMEOUT", "1s"),
    ]);
    let (code, stdout, stderr) = service.wait_output();
    assert_eq!(code, Some(1), "stdout: {stdout}; stderr: {stderr}");
    assert!(stdout.contains("plaintext"), "startup refusal: {stdout}");
    assert!(stdout.contains("shutdown_finishing"), "{stdout}");
    assert!(stderr.is_empty(), "duplicate failure fallback: {stderr}");
}

#[cfg(unix)]
#[test]
fn a_cache_password_fifo_is_rejected_before_binding_the_listener() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("password");
    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRWXU).unwrap();
    let service = Service::spawn(&[
        ("APP__APP__ENV", "local"),
        ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
        ("APP__CACHE__DSN", "redis://127.0.0.1:1"),
        ("APP__CACHE__ALLOW_PLAINTEXT", "true"),
        ("APP__CACHE__COMMAND_TIMEOUT", "500ms"),
        ("APP__CACHE__PASSWORD_FILE", path.to_str().unwrap()),
    ]);
    let (code, stdout, stderr) = service.wait_output();
    assert_eq!(code, Some(1), "stdout: {stdout}; stderr: {stderr}");
    assert!(
        stdout.contains("cache password file could not be read"),
        "{stdout}"
    );
    assert!(!stdout.contains("http listener bound"), "{stdout}");
}
// template:end cache:service-cache-lifecycle-admission

// template:begin postgres:service-postgres-lifecycle-admission
#[test]
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the startup cancellation fixture holds a silent PostgreSQL loopback connection until child teardown"
)]
fn a_stop_signal_during_startup_ends_it_before_a_listener_is_bound() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("silent listener");
    silent.set_nonblocking(true).expect("nonblocking listener");
    let port = silent.local_addr().expect("silent listener").port();
    let dsn =
        format!("postgres://service:startup-fixture@127.0.0.1:{port}/service?sslmode=disable");
    let service = Service::spawn(&[
        ("APP__APP__ENV", "local"),
        ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
        ("APP__POSTGRES__ENABLED", "true"),
        ("APP__POSTGRES__DSN", &dsn),
    ]);
    service.await_record("service_starting");
    let deadline = Instant::now() + Duration::from_secs(5);
    let peer = loop {
        match silent.accept() {
            Ok((peer, _)) => break peer,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "PostgreSQL setup did not connect"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept PostgreSQL setup: {error}"),
        }
    };
    service.terminate();
    let mut messages = Vec::new();
    while let Ok(line) = service.lines.recv_timeout(Duration::from_secs(20)) {
        let record: serde_json::Value = serde_json::from_str(&line).expect("JSON log record");
        messages.push(record["message"].as_str().unwrap_or_default().to_owned());
    }
    let (code, stderr) = service.wait();
    drop(peer);
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

#[cfg(unix)]
#[test]
fn a_mounted_postgres_password_fifo_is_rejected_before_binding_the_listener() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("password");
    let target = directory.path().join("target");
    nix::unistd::mkfifo(&target, nix::sys::stat::Mode::S_IRWXU).unwrap();
    std::os::unix::fs::symlink(target, &path).unwrap();
    let service = Service::spawn(&[
        ("APP__POSTGRES__PASSWORD_FILE", path.to_str().unwrap()),
        ("APP__POSTGRES__ENABLED", "true"),
        (
            "APP__POSTGRES__DSN",
            "postgres://service@127.0.0.1:1/service?sslmode=verify-full",
        ),
    ]);
    let (code, stdout, stderr) = service.wait_output();
    assert_eq!(code, Some(1), "stdout: {stdout}; stderr: {stderr}");
    assert!(
        stdout.contains("postgres password file could not be read"),
        "{stdout}"
    );
    assert!(!stdout.contains("http listener bound"), "{stdout}");
}

#[test]
fn postgres_refuses_unverified_connections_without_opt_in_and_keeps_the_dsn_private() {
    for (environment, query, reason) in [
        (
            "production",
            "sslmode=require",
            "requires sslmode=verify-full",
        ),
        (
            "development",
            "sslmode=disable",
            "requires sslmode=verify-full",
        ),
        (
            "production",
            "sslmode=verify-full&private-marker=x",
            "unsupported parameter",
        ),
    ] {
        let dsn = format!("postgres://service:private-marker@127.0.0.1:1/service?{query}");
        let (code, stdout, stderr) = Service::spawn(&[
            ("APP__APP__ENV", environment),
            ("APP__POSTGRES__ENABLED", "true"),
            ("APP__POSTGRES__DSN", &dsn),
        ])
        .wait_output();
        assert_eq!(code, Some(1));
        assert!(stdout.contains(reason), "{stdout}");
        assert!(!stdout.contains("http listener bound"));
        assert!(!stdout.contains("private-marker") && !stderr.contains("private-marker"));
    }
}
// template:end postgres:service-postgres-lifecycle-admission

// template:begin object-storage:service-object-storage-lifecycle-admission
fn storage_env<'a>(
    app_env: &'a str,
    provider: &'a str,
    endpoint: &'a str,
) -> [(&'static str, &'a str); 6] {
    [
        ("APP__APP__ENV", app_env),
        ("APP__OBJECT_STORAGE__PROVIDER", provider),
        ("APP__OBJECT_STORAGE__ENDPOINT", endpoint),
        ("APP__OBJECT_STORAGE__BUCKET", "template-bucket"),
        ("APP__OBJECT_STORAGE__ACCESS_KEY_ID", "template"),
        (
            "APP__OBJECT_STORAGE__SECRET_ACCESS_KEY",
            "hunter2-object-storage",
        ),
    ]
}

#[test]
fn an_unreachable_bucket_still_becomes_ready_without_a_request() {
    let endpoint = format!("http://{}", available_address());
    let mut config = storage_env("local", "local", &endpoint).to_vec();
    config.push(("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"));
    let service = Service::spawn(&config);
    let configured = service.await_record("object_storage_configured");
    assert_eq!(
        configured["object_storage.provider"], "local",
        "{configured}"
    );
    let api = service.await_addr("http listener bound");
    service.await_record("service_ready");
    let ready = format!("http://{api}/health/ready");
    assert!(
        poll_until(&ready, 200, Duration::from_secs(5)).is_some(),
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
fn the_emulator_provider_exits_before_the_listener_without_development_opt_in() {
    for environment in ["production", "local"] {
        let (code, stderr) =
            Service::spawn(&storage_env(environment, "local", "http://127.0.0.1:7070")).wait();
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
}

#[test]
fn an_r2_endpoint_outside_cloudflare_exits_before_the_listener() {
    let (code, stdout, stderr) = Service::spawn(&storage_env(
        "production",
        "cloudflare_r2",
        "https://storage.example.com",
    ))
    .wait_output();
    assert_eq!(code, Some(1), "stderr: {stderr}");
    let records = records(&stdout);
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

// template:begin clickhouse:service-clickhouse-lifecycle-admission
#[test]
fn inert_omitted_clickhouse_starts_ready_and_logs_disabled() {
    let service = Service::spawn(&[]);
    let starting = service.await_record("service_starting");
    assert_eq!(starting["clickhouse.enabled"], false, "{starting}");
    let api = service.await_addr("http listener bound");
    let diagnostics = service.await_addr("diagnostics listener bound");
    service.await_record("service_ready");
    assert!(
        poll_until(
            &format!("http://{api}/health/ready"),
            200,
            Duration::from_secs(5)
        )
        .is_some(),
        "service with omitted [clickhouse] must become ready"
    );
    let (status, metrics) = get(&format!("http://{diagnostics}/metrics")).unwrap();
    assert_eq!(status, 200);
    assert!(
        !metrics.contains("clickhouse_operations_in_flight"),
        "inert clickhouse must not register metrics:\n{metrics}"
    );
    service.terminate();
    let (code, stdout, stderr) = service.wait_output();
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(!stdout.contains("clickhouse_closed"), "{stdout}");
}

#[test]
fn unreachable_https_clickhouse_still_binds_becomes_ready_and_exposes_metrics() {
    let url = format!("https://{}", available_address());
    let secret = "clickhouse-secret-sentinel";
    let service = Service::spawn(&[
        ("APP__APP__ENV", "production"),
        ("APP__CLICKHOUSE__URL", &url),
        ("APP__CLICKHOUSE__DATABASE", "app"),
        ("APP__CLICKHOUSE__USERNAME", "app"),
        ("APP__CLICKHOUSE__PASSWORD", secret),
    ]);
    let starting = service.await_record("service_starting");
    assert_eq!(starting["clickhouse.enabled"], true, "{starting}");
    let api = service.await_addr("http listener bound");
    let diagnostics = service.await_addr("diagnostics listener bound");
    service.await_record("service_ready");
    assert!(
        poll_until(
            &format!("http://{api}/health/ready"),
            200,
            Duration::from_secs(5)
        )
        .is_some(),
        "unreachable clickhouse must not keep service from becoming ready"
    );
    let (status, metrics) = get(&format!("http://{diagnostics}/metrics")).unwrap();
    assert_eq!(status, 200);
    assert!(
        metrics
            .lines()
            .any(|line| line == "clickhouse_operations_in_flight 0"),
        "metrics missing zero-initialized clickhouse_operations_in_flight:\n{metrics}"
    );
    service.terminate();
    let (code, stdout, stderr) = service.wait_output();
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(stdout.contains("clickhouse_closed"), "{stdout}");
    assert!(
        !stdout.contains(secret) && !stderr.contains(secret),
        "clickhouse secret leaked into process output"
    );
}

#[test]
fn invalid_production_clickhouse_config_exits_before_listener_without_leaking_secrets() {
    let secret = "ch-invalid-url-secret-sentinel";
    for (url, allow_plaintext, reason) in [
        ("http://127.0.0.1:8123", "false", "clickhouse.url"),
        (
            "http://127.0.0.1:8123",
            "true",
            "clickhouse.allow_plaintext",
        ),
        (
            "https://alice:ch-invalid-url-secret-sentinel@127.0.0.1:8443",
            "false",
            "clickhouse.url",
        ),
    ] {
        let (code, stdout, stderr) = Service::spawn(&[
            ("APP__APP__ENV", "production"),
            ("APP__CLICKHOUSE__URL", url),
            ("APP__CLICKHOUSE__ALLOW_PLAINTEXT", allow_plaintext),
            ("APP__CLICKHOUSE__DATABASE", "app"),
            ("APP__CLICKHOUSE__USERNAME", "app"),
            ("APP__CLICKHOUSE__PASSWORD", secret),
        ])
        .wait_output();
        assert_eq!(
            code,
            Some(1),
            "url={url}; stdout: {stdout}; stderr: {stderr}"
        );
        assert!(stdout.is_empty(), "url={url}; stdout: {stdout}");
        assert!(stderr.contains(reason), "url={url}; stderr: {stderr}");
        assert!(
            !stdout.contains(secret) && !stderr.contains(secret),
            "secret leaked for url={url}"
        );
    }
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "process lifecycle fixture runs the service binary to observe rejected config exit status"
)]
fn clickhouse_toml_password_is_rejected_before_binding_the_listener() {
    use std::io::Write as _;

    let mut file = tempfile::NamedTempFile::new().unwrap();
    let secret = "ch-toml-secret-sentinel";
    writeln!(
        file,
        "[clickhouse]\nurl = \"https://127.0.0.1:8443\"\ndatabase = \"app\"\nusername = \"app\"\npassword = \"{secret}\""
    )
    .unwrap();
    let output = Service::command(&[])
        .args(["--config", file.path().to_str().unwrap()])
        .output()
        .expect("run service with clickhouse password in TOML");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(stderr.contains("clickhouse.password"), "stderr: {stderr}");
    assert!(
        !stdout.contains(secret) && !stderr.contains(secret),
        "TOML secret leaked"
    );
}

#[cfg(unix)]
#[test]
fn a_clickhouse_password_fifo_is_rejected_before_binding_the_listener() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("password");
    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRWXU).unwrap();
    let service = Service::spawn(&[
        ("APP__APP__ENV", "production"),
        ("APP__CLICKHOUSE__URL", "https://127.0.0.1:8443"),
        ("APP__CLICKHOUSE__DATABASE", "app"),
        ("APP__CLICKHOUSE__USERNAME", "app"),
        ("APP__CLICKHOUSE__PASSWORD_FILE", path.to_str().unwrap()),
    ]);
    let (code, stdout, stderr) = service.wait_output();
    assert_eq!(code, Some(1), "stdout: {stdout}; stderr: {stderr}");
    assert!(
        stdout.contains("clickhouse password_file could not be read"),
        "{stdout}"
    );
    assert!(stdout.contains("shutdown_finishing"), "{stdout}");
    assert!(!stdout.contains("http listener bound"), "{stdout}");
    assert!(!stdout.contains("diagnostics listener bound"), "{stdout}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
}
// template:end clickhouse:service-clickhouse-lifecycle-admission

// template:begin temporal:service-temporal-lifecycle-tests
#[test]
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the inert Temporal fixture binds a non-blocking loopback socket to assert zero dials"
)]
fn inert_temporal_disabled_becomes_ready_without_dialing_endpoint() {
    let canary = std::net::TcpListener::bind("127.0.0.1:0").expect("canary listener");
    canary.set_nonblocking(true).expect("nonblocking canary");
    let endpoint = format!("http://{}", canary.local_addr().expect("canary addr"));
    let service = Service::spawn(&[
        ("APP__APP__ENV", "local"),
        ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
        ("APP__TEMPORAL__ENABLED", "false"),
        ("APP__TEMPORAL__ENDPOINT", &endpoint),
        ("APP__TEMPORAL__ALLOW_PLAINTEXT", "true"),
        ("APP__TEMPORAL__READINESS_CRITICAL", "true"),
    ]);
    let starting = service.await_record("service_starting");
    assert_eq!(starting["temporal.enabled"], false, "{starting}");
    let api = service.await_addr("http listener bound");
    service.await_record("service_ready");
    assert!(
        poll_until(
            &format!("http://{api}/health/ready"),
            200,
            Duration::from_secs(5)
        )
        .is_some(),
        "inert Temporal configuration must reach ready 200"
    );
    assert!(
        matches!(
            canary.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ),
        "inert Temporal configuration must never dial its endpoint"
    );
    service.terminate();
    let (code, stderr) = service.wait();
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
}

#[test]
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the startup cancellation fixture holds a silent Temporal loopback connection until child teardown"
)]
fn a_stop_signal_during_temporal_startup_ends_cleanly_before_binding_listeners() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("silent listener");
    silent.set_nonblocking(true).expect("nonblocking listener");
    let endpoint = format!("http://{}", silent.local_addr().expect("silent addr"));
    let service = Service::spawn(&[
        ("APP__APP__ENV", "local"),
        ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
        ("APP__TEMPORAL__ENABLED", "true"),
        ("APP__TEMPORAL__ENDPOINT", &endpoint),
        ("APP__TEMPORAL__ALLOW_PLAINTEXT", "true"),
        ("APP__TEMPORAL__CONNECT_TIMEOUT", "10s"),
    ]);
    let starting = service.await_record("service_starting");
    assert_eq!(starting["temporal.enabled"], true, "{starting}");
    let deadline = Instant::now() + Duration::from_secs(5);
    let peer = loop {
        match silent.accept() {
            Ok((peer, _)) => break peer,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "Temporal setup did not connect");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept Temporal setup: {error}"),
        }
    };
    service.terminate();
    let mut messages = Vec::new();
    while let Ok(line) = service.lines.recv_timeout(Duration::from_secs(20)) {
        let record: serde_json::Value = serde_json::from_str(&line).expect("JSON log record");
        messages.push(record["message"].as_str().unwrap_or_default().to_owned());
    }
    let (code, stderr) = service.wait();
    drop(peer);
    drop(silent);
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(
        messages
            .iter()
            .any(|message| message == "shutdown_finishing"),
        "{messages:?}"
    );
    for skipped in ["http listener bound", "service_ready", "readiness_disabled"] {
        assert!(
            !messages.iter().any(|message| message == skipped),
            "a stopped Temporal startup must not reach {skipped:?}: {messages:?}"
        );
    }
}

#[test]
fn production_plaintext_temporal_endpoint_exits_one_before_logging_and_redacts_api_key() {
    let (code, stdout, stderr) = Service::spawn(&[
        ("APP__APP__ENV", "production"),
        ("APP__TEMPORAL__ENABLED", "true"),
        ("APP__TEMPORAL__ENDPOINT", "http://127.0.0.1:7233"),
        ("APP__TEMPORAL__API_KEY", "temporal-secret-sentinel"),
    ])
    .wait_output();
    assert_eq!(code, Some(1), "stdout: {stdout}; stderr: {stderr}");
    assert!(stderr.contains("temporal.endpoint"), "stderr: {stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(
        !stdout.contains("temporal-secret-sentinel")
            && !stderr.contains("temporal-secret-sentinel"),
        "Temporal api_key must not leak in diagnostics"
    );
}
// template:end temporal:service-temporal-lifecycle-tests

// template:begin inbound-webhooks:service-webhooks-lifecycle-tests
#[sqlx::test(migrator = "migrate::MIGRATOR")]
#[ignore = "the database integration runner supplies a migrated PostgreSQL test database"]
async fn restarting_with_an_old_inbound_receipt_acknowledges_redelivery_without_another_job(
    pool: sqlx::PgPool,
) {
    use infra_webhooks::protocol::KeyRing;
    use std::time::{SystemTime, UNIX_EPOCH};

    const KEY: &str = "whsec_Q0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0M=";
    let id = "historical-delivery";
    sqlx::query(
        "INSERT INTO webhook_receipts (endpoint_id, message_id, received_at) \
         VALUES ('partner', $1, timestamptz '2000-01-01 00:00:00+00')",
    )
    .bind(id.as_bytes())
    .execute(&pool)
    .await
    .unwrap();
    let mut dsn = url::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    dsn.set_path(pool.connect_options().get_database().unwrap());
    let service = Service::spawn(&[
        ("APP__APP__ENV", "local"),
        ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
        ("APP__POSTGRES__ENABLED", "true"),
        ("APP__POSTGRES__DSN", dsn.as_str()),
        ("APP__POSTGRES__MAX_CONNECTIONS", "1"),
        ("APP__HEALTH__REFRESH_INTERVAL", "100ms"),
        (
            "APP__INBOUND_WEBHOOKS__ENDPOINTS__PARTNER__ACTIVE_KEY",
            "v1",
        ),
        ("APP__INBOUND_WEBHOOKS__SECRETS__V1", KEY),
    ]);
    let api = service.await_addr("http listener bound");
    let diagnostics = service.await_addr("diagnostics listener bound");
    service.await_record("service_ready");
    let metrics_url = format!("http://{diagnostics}/metrics");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let (status, scrape) = get(&metrics_url).unwrap();
            assert_eq!(status, 200);
            let checks: u64 = scrape
                .lines()
                .find_map(|line| line.strip_prefix("readiness_checks_total{outcome=\"ok\"} "))
                .and_then(|count| count.parse().ok())
                .unwrap_or(0);
            let receipt_observer =
                "postgres_maintenance_observer_enabled{population=\"webhook_receipts\"} 1";
            let receipt_pass =
                "postgres_cleanup_passes_total{cleanup=\"webhook_receipts\",outcome=\"completed\"}";
            if checks >= 3 && (!scrape.contains(receipt_observer) || scrape.contains(receipt_pass))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("database readiness rounds and any initial receipt cleanup complete");

    let body = b"freshly signed redelivery";
    let timestamp = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let signature = KeyRing::from_encoded(KEY, None)
        .unwrap()
        .signatures(id.as_bytes(), timestamp, body)
        .unwrap();
    let response = common::agent()
        .post(&format!("http://{api}/webhooks/partner"))
        .header("webhook-id", id)
        .header("webhook-timestamp", timestamp.to_string())
        .header("webhook-signature", signature)
        .send(body.as_slice())
        .unwrap();
    assert_eq!(response.status().as_u16(), 204);
    service.terminate();
    assert_eq!(service.wait().0, Some(0));
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM webhook_receipts \
                 WHERE received_at = timestamptz '2000-01-01 00:00:00+00'), \
                (SELECT count(*) FROM background_jobs)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 0), "(original receipts, new processing jobs)");
}

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
    let api = service.await_addr("http listener bound");
    service.await_record("service_ready");

    let status = common::agent()
        .post(&format!("http://{api}/webhooks/unknown"))
        .send_empty()
        .expect("webhook response")
        .status()
        .as_u16();
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
    let api = service.await_addr("http listener bound");
    service.await_record("service_ready");

    let response = common::agent()
        .get(&format!("http://{api}/health/live"))
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
    #[expect(
        clippy::disallowed_methods,
        clippy::disallowed_types,
        reason = "the synchronous process fixture briefly binds a loopback listener to select its child address"
    )]
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().to_string()
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
)]
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
    assert!(
        poll_until(
            &format!("http://{api}/health/ready"),
            200,
            Duration::from_secs(10)
        )
        .is_some()
    );
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
            ("APP__HTTP__TRUST_TRACE_CONTEXT", "true"),
        ]);
        assert!(
            poll_until(
                &format!("http://{api}/health/ready"),
                200,
                Duration::from_secs(10)
            )
            .is_some()
        );
        for method in ["GET", "PRIVATE_METHOD_SENTINEL"] {
            #[expect(
                clippy::disallowed_methods,
                clippy::disallowed_types,
                reason = "the synchronous wire fixture owns this client socket through its bounded response read"
            )]
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
