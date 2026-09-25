# Intent: simplify durable jobs without weakening durable outcomes

Status: ready

## Problem

Stage 10.4 is merged. The supplied review identifies excessive lease upkeep,
unknown-commit attribution, shutdown bookkeeping, handwritten propagation and
randomness, opaque storage, and unbounded queue sampling. Its factual claims are
hypotheses to check against the current implementation.

## Desired outcome

Implement all adopted recommendations in one separate pull request: timeout-based
leases; simpler fenced outcome retries; preservation of known outcomes at
shutdown; installed OpenTelemetry propagation including tracestate; JSONB/text
storage with safe migration and input rejection; bounded observable queue
sampling; atomic transactional completion and explicit retry delay/snooze; smaller
claim lock footprint; and consolidation of redundant enqueue tests. Record the
remaining fairness limitation and future infrastructure reuse in the roadmap.

## Affected actors and systems

Producers enqueue inside a business transaction. Job adapters register kinds and
run handlers. The jobs worker owns attempts and process drain. Operators inspect
PostgreSQL, metrics and traces. Derived repositories select the optional jobs
profile; future webhook and outbox implementations consume its infrastructure.

## Scope and non-goals

Change the existing Rust jobs capability, its migration, documentation, proof and
profile carriers. Evaluate shared service/worker lifecycle extraction from actual
duplication. Stages 10.5 and 10.6 receive reuse decisions only; no webhook,
JetStream or outbox product is implemented. No merge, deployment or production
data modification is authorized.

## Constraints

Preserve transactional enqueue, `ON CONFLICT DO NOTHING` and `Duplicate`, unknown
kind nonconsumption, sequence-based fencing, database time, the 25-attempt cap,
ordinary retry delay `n^4` seconds with ±10% jitter, and terminal retention of
24 hours completed / 7 days failed. Existing migration history remains immutable.
Do not silently drop, sanitize or discard legacy rows to enable conversion.

## Success signal

One reviewable PR contains the requested simplification and capabilities,
consistent adopter contracts, and applicable passing local and CI evidence for
the exact candidate. Local proof and CI proof remain distinct; no deployed
outcome is claimed.
