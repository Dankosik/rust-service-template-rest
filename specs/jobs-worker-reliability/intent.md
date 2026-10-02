# Intent: recoverable, bounded jobs-worker

Status: ready

## Problem

The jobs-worker audit found that admitted handler capacity does not bound
attempts waiting to persist their outcomes; failed jobs, including durable
publication intent, expire without a supported recovery action; and work left
under a kind no current worker handles is difficult to discover operationally.
The same audit identified static crash-recovery latency, one-slot publication,
and combined dependency admission as limits requiring explicit disposition.

## Desired outcome

Deliver one coherent reliability correction and open one separate pull request.
The requester delegated technical choices and asked for complete fixes that do
not leave the same failure modes for another patch. Work must remain bounded
when outcome writes stall, and operators must be able to find and safely
resolve failed or stranded jobs without silently losing durable intent.

## Affected actors and systems

Adopters who enqueue jobs transactionally; ordinary and webhook handlers;
transactional-outbox publishers and deduplicating consumers; workers sharing a
PostgreSQL queue; and operators inspecting or recovering retained work.

## Scope and non-goals

Cover jobs admission through outcome persistence, failed-job custody and
recovery, operator visibility, the affected documentation and generated
template surfaces, and regression proof. Give every material audit point a
fix or an explicit retained-tradeoff disposition with a reopen condition.

Preserve PostgreSQL transactional enqueue, generation fencing, Tokio-supervised
lifecycle, existing handler policies, and immutable outbox identities. Replacing
the queue framework, redesigning JetStream consumption, adding a public
administration API, or adding workflow scheduling is outside this correction.

Assumption: no new crash-recovery, publication-throughput, or independent
failure-domain SLO is requested. Reopen if such a requirement is supplied or
current measurements establish that a retained limitation prevents the
requested service outcome.

## Constraints

The human authorized scoped local edits, validation, commits, push, and a
separate PR. This does not authorize merge, deployment, or execution of recovery
or deletion against a live queue. Existing unrelated work must be preserved.
Repository phase and validation owners remain applicable; phase artifacts do
not create additional infrastructure or proof gates.

Assumption: a failed row represents unresolved work, so preserving it until an
explicit operator decision is preferable to silently deleting it after an
arbitrary time. Reopen for an adopter's mandatory retention/deletion policy;
never infer that the template may discard unresolved publication intent.

## Success signal

The PR contains a reviewed, tested correction whose configured concurrency
bounds the full attempt lifetime; failed work remains inspectable and can be
redriven or explicitly discarded with stale/concurrent commands refused;
outbox recovery retains the same publication identity and bytes; and an
operator can identify live work outside the declared handled-kind set without
requiring a functioning broker. Delivery reports distinguish local proof,
the actual PR/CI state, and retained operational limitations.
