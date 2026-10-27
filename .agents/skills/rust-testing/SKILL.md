---
name: rust-testing
description: "Behavior. Use when writing, changing, or reviewing Rust service tests: what to assert, which layer and real dependency prove it, how to control time, and whether the suite would catch a defect."
---

# Rust Testing

**Behavior first.** Identify the observable promise a change could break, then choose the smallest layer that can watch it fail. Assert what a caller, client, or operator sees: status, body, problem code and headers, committed rows, a published message, a log record or metric, an exit code. Write each expected value as a literal derived from accepted behavior, never from the implementation's current output or a helper that recomputes it. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

The layers, smallest first: a unit test beside the owner; the mounted router through a one-shot call; the bounded server on an ephemeral loopback port; the built binary in the service crate's integration tests for lifecycle and exit codes; and the integration-tests crate against the Compose PostgreSQL, NATS, Valkey, and S3 services. Drive each layer through the interface its callers use. A test that breaks under a behavior-preserving refactor asserts structure: private fields, call counts, Debug output, statement text. A status-only assertion proves neither order nor fallback, and a leaf handler called directly omits the chain.

Use the real dependency wherever the workspace can run it: the real type over a trait double, a loopback server speaking the actual protocol over a stubbed client, the Compose database through sqlx::test over an in-memory store; that harness already isolates each test's database, so testcontainers stays rejected. A double is justified only for a provider the workspace cannot run or a fault the real one cannot produce on demand; it answers in the provider's wire format and never implements the behavior under assertion.

Control time. Advance a paused clock explicitly, which requires tokio's Instant in the code under test. Synchronize on a watch change, a notify, or a joined task, never a sleep, and bound every wait with a timeout. Send SIGTERM to a process through the nix signal call only after readiness, and always wait the child.

Keep logic out of test bodies: no loop, branch, or arithmetic computes an expectation; a table of literal cases replaces a generator. Keep fixtures small, name each test for the rule it proves, and keep the distinctions the contract cares about, such as a length exactly at a limit. A round trip shares any defect between encoder and decoder, so assert against an independent expectation.

For a defect, write the test first and watch it fail for the reported reason. For a parser, encoder, or validator with an invariant, add a bounded proptest property stated independently of the implementation beside the boundary cases. To judge a suite, make mutants PKG=crate FILE=path lists mutations no test catches; each survivor is a missing assertion or dead code, not a score to maximize.

For review, assess whether the tests distinguish the relevant defect without editing. For implementation, run the changed crate's tests, leave no running task, and report exercised behavior. A mutation run, Miri, fuzzing, or a feature matrix is a discovery tool, not a completion gate.
