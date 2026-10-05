//! The panic hook and installed subscriber are process-wide. The existing
//! isolated binary captures their actual output in one bounded child per format.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use infra_telemetry::{
    LoggerShutdown, LoggingFormat, LoggingOptions, install_panic_hook, install_subscriber,
};

const CHILD_FORMAT: &str = "TELEMETRY_PANIC_TEST_FORMAT";
const TEST_NAME: &str = "panic_payloads_and_thread_identity_are_withheld";

#[test]
fn panic_payloads_and_thread_identity_are_withheld() {
    if let Ok(format) = std::env::var(CHILD_FORMAT) {
        emit_panics(&format);
        return;
    }

    for format in ["json", "text"] {
        let capture = tempfile::NamedTempFile::new().expect("capture file");
        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", TEST_NAME, "--nocapture", "--quiet"])
            .env(CHILD_FORMAT, format)
            .stdout(Stdio::from(
                capture.as_file().try_clone().expect("stdout capture"),
            ))
            .stderr(Stdio::from(
                capture.as_file().try_clone().expect("stderr capture"),
            ))
            .spawn()
            .expect("isolated hook child");
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().expect("child status") {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().expect("kill stalled child");
                child.wait().expect("reap stalled child");
                panic!("panic-hook child exceeded its deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = std::fs::read_to_string(capture.path()).expect("captured UTF-8");
        assert!(status.success(), "{format}: {output}");
        for forbidden in [
            "literal-secret",
            "formatted-secret",
            "owned-secret",
            "nonstring-secret",
            "thread-secret",
            "panic.message",
            "panic.thread",
            "backtrace",
        ] {
            assert!(!output.contains(forbidden), "{format}: {output}");
        }
        if format == "json" {
            let records: Vec<serde_json::Value> = output
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            assert_eq!(records.len(), 4, "{output}");
            for record in records {
                assert_eq!(record["level"], "ERROR");
                assert_eq!(record["message"], "panicked");
                assert!(
                    record["panic.file"]
                        .as_str()
                        .unwrap()
                        .ends_with("panic_hook.rs")
                );
                assert!(record["panic.line"].as_u64().unwrap() > 0);
                assert!(record["panic.column"].as_u64().unwrap() > 0);
            }
        } else {
            assert_eq!(output.matches("panicked").count(), 4, "{output}");
            assert_eq!(output.matches("ERROR").count(), 4, "{output}");
            assert_eq!(output.matches("panic_hook.rs").count(), 4, "{output}");
        }
    }
}

#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "isolated test child deliberately panics to verify hook privacy and fails on fixture setup errors"
)]
fn emit_panics(format: &str) {
    let logger = install_subscriber(&LoggingOptions {
        level: "trace",
        format: if format == "json" {
            LoggingFormat::Json
        } else {
            LoggingFormat::Text
        },
        tracer_provider: None,
    })
    .expect("installed logger");
    let original_hook = std::panic::take_hook();
    install_panic_hook();
    let literal = std::panic::catch_unwind(|| panic!("literal-secret"));
    let formatted = std::panic::catch_unwind(|| {
        let detail = std::hint::black_box("formatted-secret");
        panic!("refused {detail}");
    });
    let owned = std::panic::catch_unwind(|| std::panic::panic_any(String::from("owned-secret")));
    let nonstring = std::thread::Builder::new()
        .name("thread-secret\nforged-log".to_owned())
        .spawn(|| std::panic::panic_any(["nonstring-secret"]))
        .expect("named panic thread")
        .join();
    std::panic::set_hook(original_hook);
    assert!(literal.is_err() && formatted.is_err() && owned.is_err() && nonstring.is_err());
    assert!(matches!(
        logger.shutdown(Instant::now() + Duration::from_secs(1)),
        LoggerShutdown::Completed(_)
    ));
}
