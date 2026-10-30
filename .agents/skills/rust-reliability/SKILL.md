---
name: rust-reliability
description: "Budget arithmetic. Use when deadlines, retries, overload, degradation, readiness, or shutdown need a decision about allowed time, capacity, or failure consequences."
---

# Rust Reliability

Trace the promise made to the caller through every stage that spends it. A
collection of individually bounded operations can still exceed the parent
deadline. Account for preparation, admission, acquisition, attempts, backoff,
and the response or cleanup required by the accepted contract. A later stage
spends the remaining allowance; it does not restart the original clock.

## Time and repeatability

Compare the complete worst-case path with the parent budget, including work
before dispatch and after the last attempt. Use an existing deadline and local
ceilings. A response reserve is useful when the contract needs one, not a reason
to invent a new configuration key or universal fixed margin. Evaluate each
proposed attempt count: showing that three attempts do not fit does not rule
out one. A nominally fitting attempt is still pointless if its remaining time
cannot produce a useful result.

Retry only when the failure is eligible and the effect can safely repeat.
Separate a confirmed rejection from a lost acknowledgement: a timeout after
dispatch can leave an unknown effect. Idempotency, reconciliation, or fencing
may make another attempt safe; another timeout alone cannot. Backoff and jitter
reduce synchronized pressure when retries are justified, but both consume time
and do not create capacity. Account for residual work from an earlier attempt
before admitting its replacement.

## Capacity and failure consequence

Choose bounded waiting, immediate rejection, or degradation from the workload
and caller promise. A queue absorbs a finite burst at the cost of retained
memory and waiting time; reject when its bound or a useful deadline is exhausted.
Track queued and executing work separately, including retained payloads. A
semaphore around execution does not bound an upstream queue. Degradation must
preserve the accepted minimum behavior and expose the changed result where
callers or operators need to distinguish it.

Readiness controls admission; it is not a list of every dependency. A required
dependency can justify withholding traffic, while an optional provider may
justify a narrower degraded operation. Consider failure detection, recovery,
and stale observations as well as the healthy case. Do not turn a transient
optional failure into loss of all service capacity by default.

Shutdown has the same arithmetic: drain, task joins, dependency close, and
telemetry compete for remaining process time. Expiring a wait does not establish
resource termination. Work deliberately continuing after a request needs an
accepted lifetime and effect owner; a durable handoff is appropriate when the
promise requires recovery across process loss.

When technical values are missing, use evidence and bounded assumptions through
the responsible decision owner, stating what would reopen them. Missing numbers
are not automatically a user decision. Bind admitted deadlines to
[Operation Budgets](../../../docs/operation-budgets.md), keys and cross-field
constraints to [Configuration Source Policy](../../../docs/configuration-source-policy.md),
and process admission, readiness, and teardown to
[Runtime Lifecycle](../../../docs/architecture/runtime-lifecycle.md).
