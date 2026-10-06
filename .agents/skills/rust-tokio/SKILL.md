---
name: rust-tokio
description: "Ownership. Use when Rust service tasks, select arms, locks across awaits, channels, blocking work, or startup and shutdown ordering need lifetime, cancellation, or completion guarantees."
---

# Rust Tokio

**Ownership.** For the affected work, identify who admits it, observes its failure, requests cancellation, and waits for completion. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Register process-lifetime work with bootstrap's existing tracker and a root child cancellation token. The parent observes early completion and panic, requests cancellation, then joins within its existing deadline. A dropped spawn handle leaves failure and completion unobserved. The service's registration capability lets feature managers join that ownership without creating another lifecycle. Cancelling a parent token cancels its children, never the reverse.

A select drops the losing futures, so only cancellation-safe futures may lose without corrupting state: channel receives, sleeps, watch changes, and cancellation waits are safe; a partially completed read or a send on a bounded channel is not. Use biased ordering when arm priority matters.

Use docs/architecture/runtime-lifecycle.md for business-work admission and lifetime. Keep cheap bounded computation inline; distinguish blocking I/O, sustained CPU work, and loops over immediately ready futures. An await alone does not yield. Bound each poll's work and arrange a wakeup when returning Pending. Yielding neither makes a blocking call nonblocking nor reserves CPU. Tokio's shared blocking pool also serves file and DNS operations; worker-count tuning does not replace admission, and a separate CPU executor needs an actual workload decision.

Before submitting blocking or CPU-heavy work, bound admission and move its capacity permit into actual execution until it ends or unwinds. Choose admission wait or rejection from the feature's accepted budget. Keep an owner observing completion and panic beyond waiter timeout; request cooperative cancellation and account for residual execution during shutdown. A started spawn_blocking closure survives handle abort and timeout, so a cancelled request must not release its capacity early. A retried job can overlap it and still needs its effect and fencing rules. Bound channels and queues with the work they feed. Generic serialization and formatting callbacks remain trusted synchronous code: callers bound their source, cost and concurrency; a byte-limited writer cannot preempt callback computation.

Use tokio's Instant and interval for paused-clock control; maintenance loops delay missed ticks rather than burst. Bootstrap retains signal streams for process lifetime because dropping one swallows later signals. The entry function owns runtime shutdown: shutdown_timeout stops waiting, but running blocking closures can survive until process exit; implicit runtime drop may wait indefinitely. Keep provider shutdown off Tokio workers and within the existing telemetry budget.

For review, name the owner and the termination path for every spawned task and every held guard without editing. For implementation, prove completion by joining a task or awaiting a change notification, never by sleeping; keep paused-clock tests deterministic and leave no running task behind a passing test. Expand beyond the affected path only for a concrete risk or a required project check.
