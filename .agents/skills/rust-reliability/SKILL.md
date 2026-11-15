---
name: rust-reliability
description: "Budget arithmetic. Use for deadlines, timeouts, retries, overload, readiness, drain, shutdown budgets, or dependency-failure handling."
---

# Rust Reliability

**Budget arithmetic.** Propagate an operation's deadline, cancellation lineage, capacity bound, and effect ownership across every hop to a terminal outcome or durable handoff, accounting for execution and external effects that outlive a caller's wait.

**Remaining budgets.** Pass remaining deadlines forward rather than resetting full per-layer timeouts, fitting admission wait, retry attempts, backoff, and terminal cleanup reserves inside the parent budget. Inspect third-party client defaults for connect, read, retry, and redirect timeouts explicitly.

**Cancellation and residual custody.** Pair cooperative timeouts with executor progress and bounded custody for in-flight or streaming work, preserving cutoff and cancellation lineage across `crates/operation-context` and `crates/service/src/bootstrap`.

**Retries, queues, and degradation.** Gate retries on retry-safe failure kinds, idempotent effect identity, and remaining budget while accounting for concurrent prior attempts. Pair every queue with a finite capacity and explicit overload behavior, and map unavailable dependencies to their accepted refusal, degraded-mode, or durable-handoff contract.

**Readiness and graceful drain.** Align startup, readiness, and durable job handoffs with dependency criticality in `crates/service/src/bootstrap`, keeping degradable dependencies out of readiness gates unless configured. During shutdown, halt admission, propagate cancellation, subtract elapsed time across sequential drain stages from the parent shutdown budget, and close resources in dependency order, running process-drain tests whenever lifecycle behavior changes.
