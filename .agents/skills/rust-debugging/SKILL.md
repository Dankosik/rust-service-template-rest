---
name: rust-debugging
description: "Causality. Use for an uncertain Rust service defect: a failing or flaky test, a hang, a wrong response, a startup rejection, or a teardown that overran its budget."
---

# Rust Debugging

**Causality.** Find the first observable divergence between the intended and the actual execution path, and choose the observation that could disprove the leading explanation rather than confirm it. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Separate the failure classes before reproducing anything. A compiler or clippy diagnostic needs no process. A configuration rejection prints the offending key on standard error and exits with failure. A startup failure logs a startup-failed event. A degraded shutdown exits with its distinct code and names the overrun stage in the shutdown events. A hang has a task without an owner or a wait without a bound. Read an error chain to its source, and read the shutdown and drain events in order before guessing which stage stalled.

Reproduce at the smallest layer that still fails: a unit test with a paused clock, the router through a one-shot call, the bounded server on an ephemeral port, then the binary. Preserve raw standard output, standard error, exit status, and timings when their differences explain the symptom. Debug level and text format through the APP environment make a local run readable, and a backtrace variable explains a panic; the tokio console is a diagnostic session behind an unstable cfg flag, not a dependency.

For a flaky async test, suspect an unbounded wait, a sleep used as synchronization, the standard library's Instant under a paused clock, a task that outlives the test, or a select arm that dropped a future that was not cancellation-safe. Make the wait explicit and bounded before touching timing constants. For an ownership error, identify the actual owner and required lifetime before adding a clone, an Arc, or a lock.

Minimize a reproducer only while it retains the same failure, and change one causal variable at a time. After an ineffective fix, revisit the explanation instead of stacking speculative changes. A retry, a caught panic, or a discarded error can hide the mechanism without repairing it. Keep unrelated baseline failures distinct from the reported defect, and keep secrets and credentials out of diagnostic captures.

For diagnosis, finish with the supported explanation or the next discriminating observation; do not edit unless fixing is requested. For a fix, change the causal owner, replay the original failure, retain the regression test that would have caught it, remove temporary instrumentation, and report actual verification and remaining uncertainty without expanding to unrelated diagnostics.
