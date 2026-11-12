---
name: rust-reliability
description: "Budget arithmetic. Use for a Rust decision about deadlines, timeouts, retries, overload, readiness, drain, shutdown budgets, or the consequence of a dependency failure."
---

# Rust Reliability

**Budget arithmetic.** Follow an operation's accepted deadline, cancellation, capacity, and effect ownership through each hop to a terminal result or durable handoff. A shorter caller wait does not establish that execution, retained resources, or external effects have ended. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Use the current deadline and remaining budget rather than restarting a full timeout at each layer. Bound admission, attempts, and backoff within it, including any terminal reserve the owning policy requires. Do not invent a universal reserve or a number merely to make an inequality fit. Inspect the resolved client's timeout, retry, and redirect defaults; a maintained client does not supply an end-to-end budget automatically. Resolve technical values from accepted workload and policy with a stated assumption and reopen condition; escalate only a missing user-owned constraint.

Separate timeout observation from cancellation and completion. Cooperative timers require runtime progress, and an operation may have sent work or committed an effect before a waiter expires. Give residual work an owner and a bounded lifetime where the contract requires reclamation. Preserve the original cutoff and cancellation lineage through provider and response handoffs. In this service, `crates/operation-context` and the composition root in `crates/service/src/bootstrap` own those scopes and their intentional differences.

A retry needs a known eligible failure and a safe effect or reconciliation identity. Account for earlier attempts that may still execute. Backoff and jitter belong to the accepted retry policy; retry count alone says nothing about whether the complete budget fits. A queue needs a finite system bound and a defined admission or rejection behavior. An unavailable dependency needs an explicit outcome such as refusal, degraded service, or durable handoff, and an observable signal. Do not add retries, circuit breakers, or a new knob when the existing contract needs none.

Select startup, readiness, and cleanup consequences by dependency criticality and resource lifetime. A pooled client does not automatically require a readiness probe. A service that can degrade may stay ready while reporting the dependency failure; a service that cannot must use the existing admission/readiness owner. Work that must survive process loss needs a durable handoff, whereas a waiter stopping does not by itself justify a job. In this service, the composition root in `crates/service/src/bootstrap` owns profile-specific choices.

Shutdown consumes a remaining parent budget. Stop new admission where required, propagate cancellation, observe owned completion, and close resources in their accepted dependency order. A slow stage reduces the time left for later stages rather than silently extending the total. Distinguish clean completion, bounded abandonment, and unresolved external outcome. Preserve existing loss, drain, and exit policies instead of adding a parallel shutdown mechanism.

For review, show the complete budget and ownership path for the affected failure without editing; reject a claimed bound that omits an admitted wait, residual execution, or retained resource. For implementation, use controlled coordination and existing proof appropriate to the changed promise. Readiness and process-drain tests are needed when those behaviors change, not for every local timeout calculation. State assumptions and proof limits, and follow the project's validation budget without new environments or unrelated reliability gates.
