---
name: rust-debugging
description: "Causality. Use for an uncertain Rust defect: failing or flaky test, hang, wrong response, startup rejection, or teardown overrun."
---

# Rust Debugging

**Causality.** Locate the earliest observable divergence between intended and actual behavior, design one observation able to falsify the leading explanation, and keep symptom, evidence, hypothesis, and repair distinct.

**Failure boundary.** Match the probe to the failure class: compiler/lint output for static errors, and boundary-level state for decoding rejections, panics, deadlocks, missed wakeups, resource exhaustion, or uncertain external effects. Inspect structured error chains and runtime progress directly, since stalled executors can coexist with configured timeouts or task supervisors.

**Smallest reproducer.** Reproduce at the narrowest layer preserving the failure—pure function, controlled async test, mounted router, local loopback peer, or binary process—recording exit status, event order, and timing where discriminating. Respect `crates/infra-telemetry` and `crates/service/src/bootstrap` panic-hook redaction (which withholds panic payloads and backtraces even under `RUST_BACKTRACE=1`) as an intentional security boundary.

**Async and ownership hypotheses.** Test async stalls against concrete mechanisms: admission exhaustion, dropped futures losing partial progress, missed `Notify` permits, paused-clock mismatches, detached tasks, lock contention, or executor-thread blocking. For borrow or lifetime errors, trace true data flow and ownership lifetimes before reaching for `.clone()`, `Arc`, or mutexes.

**Controlled iteration and repair.** Vary one causal input at a time while preserving secrets redaction and separating pre-existing baseline failures. When an attempted fix fails, replace the falsified hypothesis rather than stacking retries, caught panics, or longer sleeps; once proven, fix the causal owner, replay the original reproducer, keep focused regression coverage, and remove temporary probes within existing test harnesses.
