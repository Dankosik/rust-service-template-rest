---
name: rust-reliability
description: "Budget arithmetic. Use for deadlines, timeouts, retries, overload, readiness, drain, shutdown budgets, or dependency-failure handling."
---

# Rust Reliability

**Budget arithmetic.** Follow an operation's accepted deadline, cancellation, capacity, and effect ownership through each hop to a terminal result or durable handoff. A shorter caller wait does not establish that execution, retained resources, or external effects have ended.

Propagate the current deadline and remaining budget rather than restarting a full timeout at each layer, and bound admission, attempts, and backoff within it including any required terminal reserve without inventing arbitrary numbers. Inspect the resolved client's timeout, retry, and redirect defaults; a maintained client does not supply an end-to-end budget automatically.

Separate timeout observation from cancellation and completion: cooperative timers require runtime progress, and an operation may have sent work or committed an effect before a waiter expires. Give residual work an owner and a bounded lifetime where reclamation is required, preserving the original cutoff and cancellation lineage through provider and response handoffs. `crates/operation-context` and `crates/service/src/bootstrap` own those scopes and their intentional differences.

A retry needs a known eligible failure and a safe effect or reconciliation identity, accounting for earlier attempts that may still execute; retry count alone says nothing about whether the complete budget fits. A queue needs a finite bound and defined admission or rejection behavior. An unavailable dependency needs an explicit outcome (refusal, degraded service, or durable handoff) and an observable signal; do not add retries, circuit breakers, or knobs when the contract needs none.

Select startup, readiness, and cleanup consequences by dependency criticality and resource lifetime using `crates/service/src/bootstrap`. A pooled client does not automatically require a readiness probe; a service that can degrade may stay ready while reporting the failure, whereas one that cannot must use the admission/readiness owner. Work that must survive process loss needs a durable handoff, whereas a waiter stopping does not by itself justify a job.

Shutdown consumes a remaining parent budget: stop new admission where required, propagate cancellation, observe owned completion, and close resources in dependency order. Subtract elapsed time across sequential graceful-shutdown stages so a slow stage reduces time left for later stages rather than silently extending the total or restarting full budgets. Distinguish clean completion, bounded abandonment, and unresolved external outcome without adding a parallel shutdown path.

Readiness and process-drain tests are needed when those behaviors change, not for every local timeout calculation.
