---
name: rust-tokio
description: "Ownership. Use when Rust service tasks, select arms, locks across awaits, channels, blocking work, or startup and shutdown ordering need lifetime, cancellation, or completion guarantees."
---

# Rust Tokio

**Ownership.** For the affected work, identify who admits it, observes its failure, requests cancellation, and waits for completion. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Background work spawns through the task tracker in bootstrap with a child of the root cancellation token, and the shutdown sequence cancels, closes the tracker, and waits under the background-join budget. A bare spawn whose handle is dropped has no owner: its panic is invisible and its completion unprovable. Use a JoinSet when results must be collected and the tracker for fire-and-forget work under a lifecycle. Cancelling a parent token cancels its children, never the reverse.

A select drops the losing futures, so only cancellation-safe futures may lose without corrupting state: channel receives, sleeps, watch changes, and cancellation waits are safe; a partially completed read or a send on a bounded channel is not. Use biased ordering when arm priority matters.

Bound channels and queues together with the work they feed; an unbounded channel moves overload into memory. Blocking or CPU-bound calls go through spawn_blocking, which cannot be aborted once started, so bound its input and let the caller enforce the deadline. Never block on a runtime worker thread, and never call the OpenTelemetry provider shutdown there; the telemetry stage runs it on a blocking thread under a timeout.

Use tokio's Instant and interval for anything a test should control with a paused clock; the standard library's Instant is invisible to it. Maintenance loops use the delay behavior for missed ticks so a stalled tick does not burst.

Signal streams are created in bootstrap before anything can send a signal and kept for the process lifetime, because a dropped stream swallows a later signal instead of letting it terminate the process. The runtime is owned by the entry function so its shutdown timeout can drop connection tasks that outlived the drain; a runtime dropped implicitly waits forever on blocking work.

For review, name the owner and the termination path for every spawned task and every held guard without editing. For implementation, prove completion by joining a task or awaiting a change notification, never by sleeping; keep paused-clock tests deterministic and leave no running task behind a passing test. Expand beyond the affected path only for a concrete risk or a required project check.
