---
name: rust-testing
description: "Behavior. Use when writing or reviewing Rust service tests and the observable failure, the deterministic control, or the smallest proving layer is not obvious."
---

# Rust Testing

**Behavior first.** Identify the observable promise a change could break, then choose the smallest layer that can watch it fail. Derive expectations from accepted behavior, not from the implementation's current output. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

The proving layers in this workspace, smallest first: a unit test beside the owner; the mounted router through a one-shot call, asserting status, content type, problem code, and headers; the bounded server on an ephemeral loopback port for connection behavior; and the built binary in the service crate's integration tests for lifecycle, exit codes, and standard error. Use the smallest layer whose failure would actually distinguish the defect. A status-only assertion proves neither order nor fallback, a passing compile proves no behavior, and a leaf handler called directly omits the chain.

Control time and scheduling. Start the test with a paused clock and advance it explicitly for anything that ticks or expires, which requires the code under test to use tokio's Instant. Synchronize on a watch change, a notify, or a joined task, never on a sleep. Bound every wait with a timeout so a hang fails instead of stalling the suite. For process tests, send SIGTERM through the nix signal call, because the child kill method sends SIGKILL, and only after readiness has been observed; always wait the child.

Keep fixtures small and independent, and name each test for the rule it proves. Preserve the distinctions the contract cares about: a byte length exactly at a limit, the Allow header, the retry hint, the exact problem code. A round trip can share a defect between encoder and decoder, so assert against an independent expectation. Integration test files allow unwrap, expect, and panic through a crate-level allow with its reason; production code does not.

For a regression, observe the assertion fail against the defect before the fix, or reuse the already observed failure. Do not add a Miri run, a fuzz campaign, a container, or a feature matrix because a skill mentions one; missing optional infrastructure is a gap to disclose, not a blocker to build.

For review, assess whether the tests distinguish the relevant defect without editing. For implementation, run the changed crate's tests, confirm the new test fails for the wrong behavior and passes for the accepted one, leave no running task, and report exercised behavior rather than untested process or platform guarantees.
