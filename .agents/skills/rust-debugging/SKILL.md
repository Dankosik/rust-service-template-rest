---
name: rust-debugging
description: "Use when a test, the service, or a lifecycle stage fails, hangs, flakes, or produces wrong output and the cause is not yet known."
metadata:
  invocation: model
  kind: method
---

# Rust Debugging

**Causality.** Find the first observable divergence between intended and
actual behavior, and choose the observation that could disprove the leading
explanation rather than confirm it.

Separate the failure classes first: a compiler or clippy diagnostic needs no
process; a configuration rejection prints the offending key on stderr and
exits 1; a startup failure logs `startup failed`; a degraded shutdown exits 3
with the overrun stage in the log; a hang has a task without an owner or a
wait without a bound. Read the error chain to its `#[source]`; read the
`shutdown_*` and `drain_*` events in order before guessing which stage
stalled.

Reproduce at the smallest layer that still fails: a unit test with a paused
clock, the router through `oneshot`, `Server` on an ephemeral port, then the
binary. Keep raw stdout, stderr, exit status, and timings when their
differences explain the symptom. `APP__LOG__LEVEL=debug` and
`APP__LOG__FORMAT=text` make local runs readable; `RUST_BACKTRACE=1` for
panics. `tokio-console` needs `--cfg tokio_unstable` and is a diagnostic
session, not a dependency.

For a flaky async test, suspect an unbounded wait, a `sleep` used as
synchronization, `std::time::Instant` under a paused clock, a task that
outlives the test, or a `select!` dropping a non-cancellation-safe future.
Make the wait explicit and bounded before touching timing constants. For an
ownership error, identify the actual owner and lifetime before reaching for
`clone` or `Arc`.

Change one causal variable at a time. After an ineffective fix, revisit the
explanation instead of stacking speculative changes. A retry, a caught panic,
or a discarded `Err` can hide the mechanism without repairing it. Keep
unrelated baseline failures distinct from the reported defect, and keep
secrets out of captures.

For diagnosis, finish with the supported explanation or the next
discriminating observation; do not edit unless a fix is requested. For a fix,
change the causal owner, replay the original failure, add the regression test
that would have caught it, remove temporary instrumentation, and report what
was verified and what remains uncertain.
