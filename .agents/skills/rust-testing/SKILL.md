---
name: rust-testing
description: "Behavior. Use when writing or reviewing Rust tests: assertions, proving layer, real dependencies, time control, and defect detection."
---

# Rust Testing

**Behavior first.** Identify the observable promise a change could break, then choose the smallest layer that can watch it fail. Assert what a caller, client, or operator sees (status, body, problem code and headers, committed rows, published message, log record or metric, exit code) using expected literals or independent properties rather than current output or a mirroring helper.

Choose the smallest layer that observes the promise: unit tests beside the owner, the mounted router, a bounded loopback server, the built binary for lifecycle and exit codes, or the integration-tests crate against Compose services. Drive the interface callers use; coupling to incidental layout, call counts, `Debug` output, or statement text without an observable contract breaks behavior-preserving refactors. A status-only assertion proves neither order nor fallback, and calling a leaf handler directly omits the middleware chain.

Use the real dependency when its behavior is part of the claim and the selected harness can observe it, following the [database validation owner](../../../docs/validation/postgres.md) for actual database behavior instead of creating a second container harness. A double can control an otherwise unavailable provider, nondeterministic input, or fault, but must not implement the behavior under assertion or stand in as proof of real provider semantics.

Control time and coordination separately: code tested with paused Tokio time must use the compatible clock, accounting for automatic advancement when idle. Synchronize on an observable state change, notification, or joined task rather than sleep, and bound waits; for process proof, observe readiness before sending the shutdown signal and wait for the child to terminate using the existing harness.

Organize tests by behavior rather than method, keeping expected results independent of production constants or branches, fixtures small, and boundary cases (such as exact length limits) explicit. Use an independent wire expectation when compatibility is the promise rather than relying on a round trip that can hide matching encoder/decoder errors.

For a defect, write the test first and watch it fail for the reported reason; use a bounded independent property when a parser, encoder, or validator misses material combinations. `make mutants PKG=crate FILE=path` lists uncaught mutations as a discovery aid rather than a score to maximize.

Verify that intended cases actually ran (zero matched tests, excluded targets, or ignored cases prove nothing), and treat `make mutants`, Miri, fuzzing, and feature matrices as discovery tools rather than completion gates.
