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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("telemetry_flushed"),
        "startup refusal must flush its installed provider: {stdout}"
    );
    assert!(
        stdout.contains("shutdown_completed"),
        "startup refusal must finish common cleanup: {stdout}"
    );
    assert!(
        !stdout.contains("jobs_worker_ready"),
        "refused startup cannot become ready: {stdout}"
    );
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

// template:begin inbound-webhooks:worker-webhooks-inbound-process-tests
#[test]
fn configured_inbound_endpoint_refuses_without_a_consumer_before_database_admission() {
    let output = Command::new(env!("CARGO_BIN_EXE_jobs-worker"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("APP__POSTGRES__ENABLED", "true")
        .env(
            "APP__POSTGRES__DSN",
            "postgres://app:pw@127.0.0.1:1/app?sslmode=disable",
        )
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

// template:begin jobs:worker-operator-process-tests
const JOB_ID: &str = "00000000-0000-4000-8000-000000000001";

#[test]
fn operator_usage_is_validated_before_configuration_without_echoing_input() {
    let cases: &[&[&str]] = &[
        &["inspect", "sensitive-rejected-value"],
        &["inspect", "00000000-0000-0000-0000-000000000000"],
        &["failed", "--after", "sensitive-rejected-value"],
        &["failed", "--limit", "0"],
        &["failed", "--limit", "501"],
        &["failed", "--limit", "sensitive-rejected-value"],
        &["unhandled"],
        &[
            "unhandled",
            "--handled-kinds",
            "kind, sensitive-rejected-value",
        ],
        &["redrive", JOB_ID, "--kind", "bad kind", "--version", "1"],
        &[
            "redrive",
            JOB_ID,
            "--kind",
            "kind",
            "--version",
            "9223372036854775808",
        ],
        &["discard", JOB_ID, "--kind", "kind", "--version", "+1"],
        &["discard", JOB_ID, "--kind", "kind"],
        &["--sensitive-rejected-value"],
    ];
    for args in cases {
        let mut with_config = vec!["--config", "/missing-jobs-operator-config.toml"];
        with_config.extend_from_slice(args);
        let output = output(&with_config);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(
            output.stdout.is_empty(),
            "usage must not report a database outcome"
        );
        assert!(!stderr.contains("sensitive-rejected-value"), "{stderr}");
        assert!(
            !stderr.contains("missing-jobs-operator-config"),
            "configuration was reached: {stderr}"
        );
    }
}

#[test]
fn operator_commands_use_only_postgres_configuration_and_safe_identity_receipts() {
    let cases: &[(&[&str], &str)] = &[
        (&["inspect", JOB_ID], "inspect"),
        (&["failed"], "failed"),
        (&["unhandled", "--handled-kinds", ""], "unhandled"),
        (
            &["unhandled", "--handled-kinds", "zebra,alpha,zebra"],
            "unhandled",
        ),
        (
            &[
                "redrive",
                JOB_ID,
                "--kind",
                "kind",
                "--version",
                "9007199254740993",
            ],
            "redrive",
        ),
        (
            &[
                "discard",
                JOB_ID,
                "--kind",
                "kind",
                "--version",
                "9007199254740993",
            ],
            "discard",
        ),
    ];
    for (args, action) in cases {
        let output = output_with_env(
            args,
            &[
                ("APP__HTTP__REQUEST_TIMEOUT", "invalid-duration"),
                ("APP__JOBS__MAX_WORKERS", "0"),
                ("APP__MESSAGING__ENABLED", "true"),
            ],
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {stderr}");
        let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(receipt["schema_version"], 1);
        assert_eq!(receipt["action"], *action);
        assert_eq!(receipt["cause"], "postgres_disabled");
        assert!(
            stderr.is_empty(),
            "unrelated configuration was read: {stderr}"
        );
        if matches!(*action, "redrive" | "discard") {
            assert_eq!(receipt["outcome"], "failed");
            assert_eq!(receipt["id"], JOB_ID);
            assert_eq!(receipt["kind"], "kind");
            assert_eq!(receipt["expected_version"], "9007199254740993");
            assert!(receipt.get("new_version").is_none());
        } else {
            assert_eq!(receipt["outcome"], "unavailable");
            assert!(receipt.get("items").is_none());
            assert!(receipt.get("next_cursor").is_none());
        }
        if *action == "unhandled" {
            let expected = if args[2].is_empty() {
                serde_json::json!([])
            } else {
                serde_json::json!(["alpha", "zebra"])
            };
            assert_eq!(receipt["handled_kinds"], expected);
        } else {
            assert!(receipt.get("handled_kinds").is_none());
        }
    }
}

#[test]
fn operator_dsn_refusal_keeps_credentials_and_arbitrary_parameters_private() {
    let output = output_with_env(
        &["inspect", JOB_ID],
        &[
            ("APP__POSTGRES__ENABLED", "true"),
            ("APP__POSTGRES__MAX_CONNECTIONS", "1"),
            (
                "APP__POSTGRES__DSN",
                "postgres://operator:private-password@127.0.0.1:1/jobs?sslmode=disable&sensitive-rejected-value=private-value",
            ),
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["cause"], "dsn");
    assert_eq!(receipt["outcome"], "unavailable");
    for secret in [
        "private-password",
        "sensitive-rejected-value",
        "private-value",
    ] {
        assert!(
            !stdout.contains(secret) && !stderr.contains(secret),
            "{stdout} {stderr}"
        );
    }
}

#[test]
fn operator_help_is_available_before_configuration() {
    let output = output(&[
        "--config",
        "/missing-jobs-operator-config.toml",
        "discard",
        "--help",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("Permanently abandon"), "{stdout}");
    assert!(
        stdout.contains("--kind") && stdout.contains("--version"),
        "{stdout}"
    );
    assert_eq!(output.stderr, [] as [u8; 0]);
}
// template:end jobs:worker-operator-process-tests
