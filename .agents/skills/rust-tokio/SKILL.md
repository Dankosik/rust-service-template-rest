---
name: rust-tokio
description: "Execution custody. Use when Rust tasks, select arms, guards, channels, or blocking work need a decision about cancellation, progress, or observed completion."
---

# Rust Tokio

Follow work from admission through execution to observed completion. The caller
waiting for a result, the owner of the running work, and the owner requesting
cancellation can be different. Keep custody of the result and resources with
an owner that survives for as long as the work can run; a timeout records that
waiting ended, not that execution or its effects ended.

## Cancellation and completion

A losing `select!` future is dropped. Inspect the particular operation at that
point: what was consumed, reserved, written, or transferred, and who owns any
partial progress? Dropping a pending bounded-channel send can discard its value
and queue position; rebuilding it is not automatically an equivalent retry.
An operation that keeps progress in caller-owned state may be safely resumed
where a convenience future that owns that progress cannot. Check the resolved
Tokio version's official documentation when cancellation or API behavior decides
the design, rather than relying on a blanket list of safe operations.

Distinguish requesting cancellation, acknowledging it, and joining completion.
Retain observation of task failure and panic when they affect the caller or
process. Dropping a `JoinHandle` detaches its task; requesting abort does not by
itself prove termination. Select arm priority is a policy choice: biased polling
can preserve required priority, but a continuously ready earlier arm can starve
later work. Choose it only with that consequence accounted for.

## Actual resource lifetime

Admit blocking or CPU-heavy work before submission and keep its capacity permit
inside the actual execution until completion or unwind. Releasing it when a
request times out lets residual closures exceed the advertised bound. A started
`spawn_blocking` closure can survive abort or timeout of its waiter; retain a
completion observer, use cooperative cancellation where supported, and account
for overlap with retries. Effect repeatability remains a separate question.
A bounded worker count is insufficient if queued inputs grow without a bound.

Short bounded computation can stay inline. Blocking I/O and sustained CPU work
have different scheduling costs; an extra executor needs an actual workload
reason. Tokio's shared blocking pool also serves library work. An `.await` on
an immediately ready future need not yield: bound work per poll, and arrange a
wakeup when a custom future returns `Pending`. Yielding cannot preempt a blocking
call. Likewise, a byte-limited writer cannot bound arbitrary synchronous
serialization or formatting computation; its caller must bound input and cost.

A guard can cause deadlock without crossing an await: inspect callback reentry,
lock ordering, and blocking work under it. Moving a callback outside the lock
helps only if the protected operation remains atomic where required. Prefer a
short critical section when it preserves the invariant; retain a bounded guarded
operation when moving it would expose inconsistent state.

Choose missed-tick behavior from the work: catch-up bursts can amplify overload
for maintenance, while skipping ticks can discard required logical work. Delay
or skip when only the latest refresh matters; preserve explicit accounting when
each interval represents an obligation. Use the runtime clock when timer behavior
must be controlled by paused-clock tests.

For process registration, cancellation lineage, signal custody, readiness, and
shutdown, use [Runtime Lifecycle](../../../docs/architecture/runtime-lifecycle.md).
Its existing owner also accounts for residual blocking execution after the
runtime's shutdown wait expires. These local policies decide where custody lives;
this method decides whether custody lasts until the work actually ends.
