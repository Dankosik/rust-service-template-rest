---
name: rust-tokio
description: "Use when a change spawns tasks, selects over futures, holds locks across awaits, adds channels or blocking work, or touches startup and shutdown ordering."
metadata:
  invocation: model
  kind: method
---

# Rust Tokio

Every task has an **owner** who admits it, observes its failure, cancels it,
and waits for it to finish.

Background work spawns through the `TaskTracker` in `crates/service`'s
`bootstrap` with a child of the root `CancellationToken`; the shutdown
sequence cancels, closes the tracker, and waits under the background-join
budget. A bare `tokio::spawn` whose handle is dropped has no owner: its
panic is invisible and its completion unprovable. Use `JoinSet` when results
must be collected, `TaskTracker` for fire-and-forget under a lifecycle.

`select!` drops the losing futures; only cancellation-safe futures (channel
receives, `sleep`, `changed()`, `cancelled()`) may lose without corrupting
state. A read halfway through a buffer or a `send` on a bounded channel is
not safe to drop mid-way. Reserve or `biased;` when order matters.

Bound channels and queues together with the work they feed; an unbounded
channel moves overload into memory. CPU-bound or blocking calls go through
`spawn_blocking`, which cannot be aborted once started, so bound its input
and give it a deadline the caller enforces. Never call `block_on` or the
OpenTelemetry provider `shutdown` on a runtime worker thread.

Use `tokio::time::Instant` and `tokio::time::interval` for anything a test
should control with `start_paused`; `std::time::Instant` is invisible to the
paused clock. `MissedTickBehavior::Delay` for maintenance loops.

Signal streams are created in `bootstrap` before anything can send a signal
and kept for the process lifetime; the runtime is owned by `bootstrap::run`
so `shutdown_timeout` can drop connection tasks that outlived the drain.

For review, name the owner and the termination path for every spawned task
and every held guard. For implementation, prove completion by joining or
awaiting a `changed()`, never by sleeping; keep the paused clock deterministic
and leave no running task behind a passing test.
