//! Process-level proof of the shipped binary with no configuration and no
//! database. It exits 1. Where the workspace root has no `template.lock`
//! (the template source), stderr is exactly the no-kind refusal, so a
//! template change that registers a kind fails this test. A derived service
//! may register kinds and then refuse because PostgreSQL is disabled; either
//! refusal passes there. `--help` exits 0 and prints usage.

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
        assert_eq!(
            stderr,
            "no job kind is registered: register this service's job kinds in crates/jobs-worker/src/main.rs",
            "stderr: {stderr}"
        );
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
