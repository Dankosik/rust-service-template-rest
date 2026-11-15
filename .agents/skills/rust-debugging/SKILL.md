---
name: rust-debugging
description: "Causality. Use for an uncertain Rust defect: failing or flaky test, hang, wrong response, startup rejection, or teardown overrun."
---

# Rust Debugging

**Causality.** Find the first observable divergence between intended and actual behavior, and choose an observation that could disprove the leading explanation. Keep symptom, evidence, hypothesis, and proposed repair distinct.

Classify the failure before selecting an environment. Compiler and lint diagnostics need no process; a decoding rejection, runtime panic, deadlock, missing wakeup, exhausted resource, or uncertain external effect needs evidence from its own boundary. A task owner or timeout can exist while runtime progress is stalled, so do not infer the cause of a hang merely from their presence or absence. Inspect structured errors and their causes before interpreting a top-level message.

Reproduce at the smallest available layer that retains the same failure: pure operation, controlled async test, mounted router, bounded local peer, or process. Preserve output, exit status, event order, and timing when those distinguish explanations. `crates/infra-telemetry` and the composition root in `crates/service/src/bootstrap` own startup and shutdown signals; the installed panic hook intentionally redacts panic payloads and backtraces even when `RUST_BACKTRACE=1` is set—do not bypass or weaken that redaction to obtain a convenient trace.

For uncertain async behavior, inspect admission, ownership, notification registration, cancellation points, lock scope, and executor progress: sleep-based synchronization, incompatible clocks under paused time, a detached task, a dropped future with meaningful partial progress, or blocking work on an executor thread. These are hypotheses to discriminate, not a checklist of defects presumed to exist. For an ownership diagnostic, recover actual lifetime and data flow before adding a clone, `Arc`, or lock.

Minimize the reproducer only while it retains the relevant cause and boundary, changing one causal variable at a time and comparing observation with prediction. After an ineffective fix, revisit the explanation instead of stacking compensating branches, retries, or timing increases; a swallowed error, caught panic, or longer wait conceals a mechanism without repairing it. Keep unrelated baseline failures separate and diagnostic captures free of secrets.

For an authorized fix, change the causal owner, replay the original failure, retain focused regression coverage, and remove task-owned temporary instrumentation without building a new environment merely because an optional diagnostic is unavailable.
