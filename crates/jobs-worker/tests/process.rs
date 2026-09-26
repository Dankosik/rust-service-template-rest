//! Process-level proof of the shipped binary with no configuration and no
//! database. It exits 1 after its retained job registrations because
//! PostgreSQL is disabled. `--help` exits 0 and prints usage.

// Integration tests are test code; the workspace's production lint levels
// for unwrap/expect/panic do not apply to them.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::Path;
use std::process::{Command, Output};

fn output(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jobs-worker"))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .output()
        .expect("spawn jobs-worker")
}

#[test]
fn shipped_binary_refuses_without_a_database() {
    let output = output(&[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim_end_matches('\n');
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if root.join("template.lock").exists() {
        assert!(
            stderr.contains("no job kind is registered")
                || stderr.contains("postgres.enabled must be true to run the jobs worker"),
            "stderr: {stderr}"
        );
    } else {
        #[allow(
            unused_variables,
            reason = "retained profiles replace the source expectation"
        )]
        let expected = "no job kind is registered: register this service's job kinds in crates/jobs-worker/src/main.rs";
        // template:begin webhooks-common:worker-webhooks-process-expectation
        let expected = "postgres.enabled must be true to run the jobs worker";
        // template:end webhooks-common:worker-webhooks-process-expectation
        assert!(stderr.contains(expected), "stderr: {stderr}");
    }
}

#[test]
fn help_exits_zero_and_prints_usage() {
    let output = output(&["--help"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stdout.contains("Usage"), "stdout: {stdout}");
}

// template:begin inbound-webhooks:worker-webhooks-inbound-process-tests
#[test]
fn configured_inbound_endpoint_refuses_without_a_consumer_before_database_admission() {
    let output = Command::new(env!("CARGO_BIN_EXE_jobs-worker"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env(
            "APP__INBOUND_WEBHOOKS__ENDPOINTS__PARTNER__ACTIVE_KEY",
            "partner_v1",
        )
        .output()
        .expect("spawn jobs-worker");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("inbound webhook endpoint partner has no consumer binding"),
        "stderr: {stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("worker_ready"),
        "unbound endpoint must refuse before the worker starts"
    );
}
// template:end inbound-webhooks:worker-webhooks-inbound-process-tests
