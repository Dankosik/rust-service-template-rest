//! Subprocesses isolate the global subscriber and real stdout failure modes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test failures explain broken fixtures.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
#[allow(
    clippy::disallowed_types,
    reason = "terminal subprocess fixture owns a synchronous socket pair through child teardown"
)]
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;

use super::*;

const FIXTURE: &str = "MIGRATE_TERMINAL_FIXTURE";

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Clone, Copy)]
enum Sink {
    Healthy,
    Broken,
    Stalled,
}

#[allow(
    clippy::disallowed_methods,
    reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
)]
fn run_fixture(scenario: &str, sink: Sink) -> (i32, String, String, Duration) {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    reader
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut child = Process(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::terminal_fixture", "--nocapture"])
            .env(FIXTURE, scenario)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(OwnedFd::from(writer.try_clone().unwrap())))
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut reader = BufReader::new(reader);
    loop {
        let mut line = String::new();
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "fixture did not start"
        );
        if line.contains("fixture_ready") {
            break;
        }
    }
    let mut reader = match sink {
        Sink::Broken => {
            drop(reader);
            None
        }
        Sink::Stalled => {
            // Fill the actual stdout socket before releasing the fixture. The
            // logging thread must then wait for OS capacity until process exit.
            writer.set_nonblocking(true).unwrap();
            let bytes = [b'x'; 8192];
            let mut size = bytes.len();
            loop {
                match writer.write(&bytes[..size]) {
                    Ok(0) => panic!("stdout socket unexpectedly closed"),
                    Ok(_) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock && size > 1 => {
                        size /= 2;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(err) => panic!("fill stdout socket: {err}"),
                }
            }
            writer.set_nonblocking(false).unwrap();
            Some(reader)
        }
        Sink::Healthy => Some(reader),
    };
    drop(writer);
    let started = Instant::now();
    child.0.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "terminal path hung"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let elapsed = started.elapsed();
    let mut stdout = String::new();
    if matches!(sink, Sink::Healthy) {
        reader
            .as_mut()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
    }
    let mut stderr = String::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    (status.code().unwrap(), stdout, stderr, elapsed)
}

#[test]
fn terminal_records_keep_the_known_migration_outcome() {
    for (scenario, code, fields) in [
        (
            "success",
            0,
            vec![
                "\"outcome\":\"success\"",
                "\"migration.before\":5",
                "\"migration.after\":7",
                "\"migration.applied_count\":2",
            ],
        ),
        (
            "no_change",
            0,
            vec![
                "\"outcome\":\"no_change\"",
                "\"migration.before\":7",
                "\"migration.after\":7",
                "\"migration.applied_count\":0",
            ],
        ),
        (
            "error",
            1,
            vec!["\"outcome\":\"error\"", "\"stage\":\"config\""],
        ),
    ] {
        let (actual, stdout, stderr, _) = run_fixture(scenario, Sink::Healthy);
        assert_eq!(actual, code, "{scenario}: {stderr}");
        assert_eq!(stdout.matches("migration_run").count(), 1, "{stdout}");
        for field in fields {
            assert!(stdout.contains(field), "missing {field}: {stdout}");
        }
        assert!(stderr.is_empty(), "{stderr}");
    }
}

#[test]
fn stdout_failure_preserves_primary_migration_result_without_fallback() {
    for scenario in ["success", "no_change", "error"] {
        let (code, _, stderr, _) = run_fixture(scenario, Sink::Broken);
        assert_eq!(code, i32::from(scenario == "error"), "{scenario}: {stderr}");
        assert!(stderr.is_empty(), "{stderr}");
    }
}

#[test]
fn runtime_and_stalled_stdout_share_the_one_second_terminal_allowance() {
    let (code, _, stderr, elapsed) = run_fixture("runtime_blocked", Sink::Stalled);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    assert!(elapsed >= CLEANUP_TIMEOUT, "{elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(1750),
        "budget was extended: {elapsed:?}"
    );
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "isolated terminal child owns its control stdin and captured stdout until the parent finishes the fixture"
)]
fn terminal_fixture() {
    let Ok(scenario) = std::env::var(FIXTURE) else {
        return;
    };
    let logging = install_subscriber(&LoggingOptions {
        level: "info",
        format: LoggingFormat::Json,
        tracer_provider: None,
    })
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _blocking_release = if scenario == "runtime_blocked" {
        let (release, wait) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel();
        runtime.spawn_blocking(move || {
            started.send(()).unwrap();
            let _ = wait.recv();
        });
        running.recv_timeout(Duration::from_secs(5)).unwrap();
        Some(release)
    } else {
        None
    };
    std::io::stdout().write_all(b"fixture_ready\n").unwrap();
    std::io::stdout().flush().unwrap();
    let mut go = String::new();
    std::io::stdin().read_line(&mut go).unwrap();
    let outcome = match scenario.as_str() {
        "error" => Err(Failure::PostgresDisabled),
        "no_change" => Ok(Report {
            before: Some(7),
            applied: vec![],
        }),
        _ => Ok(Report {
            before: Some(5),
            applied: vec![6, 7],
        }),
    };
    let result = finish_run(runtime, logging, outcome, Some(7), Instant::now());
    // Bypass the test harness's stdout reporting after intentionally breaking
    // that descriptor; the parent owns the child and observes its actual exit.
    std::process::exit(i32::from(result != ExitCode::SUCCESS));
}
