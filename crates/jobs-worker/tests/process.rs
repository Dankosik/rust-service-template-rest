//! Process-level proof of the shipped binary with no configuration and no
//! database. It refuses before broker I/O when no retained registration is
//! available, and a retained jobs registry still requires PostgreSQL.
//! `--help` exits 0 and prints usage.

// Integration tests are test code; the workspace's production lint levels
// for unwrap/expect/panic do not apply to them.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::Path;
use std::process::{Command, Output};

fn output(args: &[&str]) -> Output {
    output_with_env(args, &[])
}

fn output_with_env(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jobs-worker"))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(env.iter().copied())
        .output()
        .expect("spawn jobs-worker")
}

#[test]
fn shipped_binary_refuses_before_unconfigured_dependency_admission() {
    let output = output(&[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim_end_matches('\n');
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if root.join("template.lock").exists() {
        assert!(
            stderr.contains("no job kind or typed message handler is registered")
                || stderr.contains("postgres.enabled must be true to run the jobs worker"),
            "stderr: {stderr}"
        );
    } else {
        #[allow(
            unused_variables,
            reason = "retained profiles replace the source expectation"
        )]
        let expected = "no job kind or typed message handler is registered: register this service's retained capabilities in crates/jobs-worker/src/main.rs";
        // template:begin jobs:worker-process-jobs-expectation
        let expected = "postgres.enabled must be true to run the jobs worker";
        // template:end jobs:worker-process-jobs-expectation
        assert!(stderr.contains(expected), "stderr: {stderr}");
    }
}

// template:begin outbox:worker-process-outbox-capacity
#[test]
fn retained_outbox_reserves_profile_specific_connection_capacity() {
    let output = output_with_env(
        &[],
        &[
            ("APP__POSTGRES__ENABLED", "true"),
            (
                "APP__POSTGRES__DSN",
                "postgres://app:pw@127.0.0.1:1/app?sslmode=disable",
            ),
            ("APP__POSTGRES__MAX_CONNECTIONS", "2"),
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        !stderr.contains("postgres.dsn") && !stderr.contains("messaging."),
        "capacity must refuse before another configuration failure: {stderr}"
    );
    let outbox_only = "must be at least 3 (3) for the outbox worker";
    let ordinary_jobs = "must be at least jobs.max_workers + 5 (6) for the outbox worker";
    assert!(
        stderr.contains(outbox_only) || stderr.contains(ordinary_jobs),
        "stderr: {stderr}"
    );
}
// template:end outbox:worker-process-outbox-capacity

#[test]
fn help_exits_zero_and_prints_usage() {
    let output = output(&["--help"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stdout.contains("Usage"), "stdout: {stdout}");
}
