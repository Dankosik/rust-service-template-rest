---
name: rust-testing
description: "Use while writing or reviewing tests when the observable failure, the deterministic control, or the smallest proving layer is not obvious."
metadata:
  invocation: model
  kind: method
---

# Rust Testing

Choose a test that **rejects plausible wrong behavior** at the smallest layer
that can observe it, with expectations derived from accepted behavior rather
than the implementation's current output.

Proving layers in this workspace, smallest first: a unit test beside the
owner under `#[cfg(test)]`; the mounted router through
`tower::ServiceExt::oneshot`, asserting status, `Content-Type`, problem
`code`, and headers; `infra_http::Server` on `127.0.0.1:0` for connection
behavior; the built binary through `env!("CARGO_BIN_EXE_service")` in
`crates/service/tests` for lifecycle, exit codes, and stderr. Use the
smallest layer whose failure would actually distinguish the defect; a
status-only assertion proves neither order nor fallback, and a passing
compile proves no behavior.

Control time and scheduling. Use `#[tokio::test(start_paused = true)]` with
`tokio::time::advance` for anything that ticks or expires, which requires the
code under test to use `tokio::time::Instant`. Synchronize on
`watch::Receiver::changed`, `Notify`, or a joined task, never on `sleep`.
Bound every wait with `tokio::time::timeout` so a hang fails instead of
stalling the suite. For process tests, send `SIGTERM` through
`nix::sys::signal::kill` (`Child::kill` is SIGKILL) only after readiness has
been observed, and always `wait()` the child.

Fixtures stay small and independent; a test names the rule it proves in its
function name. Preserve the distinctions the contract cares about (byte
lengths at a limit, the `Allow` header, `Retry-After`, the exact problem
code). Round trips can share a defect between encoder and decoder; assert
against an independent expectation. Integration test files allow
`unwrap`/`expect`/`panic` with a crate-level `#![allow]` and its reason;
production code does not.

For a regression, observe the assertion fail against the defect before the
fix, or reuse the already-observed failure. Do not add a Miri run, a fuzz
campaign, a container, or a feature matrix because a skill mentions one;
missing optional infrastructure is a gap to disclose, not a blocker.

Complete when the new or changed test fails for the wrong behavior, passes
for the accepted one, leaves no running task, and runs under
`make test-package PKG=<crate>`.
