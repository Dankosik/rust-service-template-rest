# System Design

Disclosed branch of [Technical Design](../technical-design.md). Own one
behaviorally equivalent system mechanism, its data/trust invariants, and its
end-to-end material flows.

## Mechanism Selection And Material Flows

- Compare only viable same-level substitutes against the same concrete scenario
  (including the current mechanism when viable); choose the simplest when
  equivalent, or name the decisive fact when only one is viable. Never compare
  prerequisites, complements, or defense-in-depth layers as substitutes.
- Decide durable delivery, replay, and reconciliation from [Repository
  Architecture](../../../repo-architecture.md). For signature-sensitive wire or
  payload shapes, fix exact bytes, algorithm, and one deterministic non-secret
  vector.
- Use [Design It Twice](design-it-twice.md) for hard-to-reverse forks and
  [Read-Only Delegation](../../shared/read-only-delegation.md) for independent
  domain questions, resolving disagreements via [Parent-Owned
  Recovery](../../shared/transition.md#parent-owned-recovery).
- Trace each trigger from entry to caller-visible completion or durable
  finality (showing current vs target only where the delta changes a decision,
  using a diagram only when compact text cannot make ordering or recovery
  reviewable). At every real crossing (service, datastore, cache, queue,
  external provider, worker, config/secret seam), fix initiating/receiving
  owners, transferred responsibility, canonical representation (identifiers,
  units, absence, ordering, freshness, consistency, finality), and produced
  effect.

## Seven Failure, Trust, And Operational Probes

Attack every real crossing with the probes it exposes; omit inapplicable probes:

1. **Data model, transaction scope & source of truth:** Single canonical writer
   and enforcing boundary per invariant; exact atomic transaction scope (**zero
   network I/O inside an open DB transaction or held pool connection**); query
   access patterns and indexes; retention/pruning bounds preserving
   replay/dedup invariants; for shared caching (`crates/infra-cache`),
   namespace, tenant-isolated key, TTL, invalidation, and cache-down fallback.
2. **Trust boundary & attacker paths:** AuthN/AuthZ and tenant isolation on
   every effectful entry point (HTTP, gRPC, worker, webhook redrive) with
   bypass paths closed; secret/PII custody and redaction across logs, metrics,
   spans, `Debug`, and `infra_http::Problem` / `tonic::Status` /
   `service_failure::Code` details; egress/SSRF and untrusted
   parser/decompression bounds.
3. **Ambiguous outcome (`CommitUnknown`) & recovery:** Where DB commit, queue
   publish, or remote call can succeed despite timeout, disconnect, or crash,
   fix retry owner and attempt budget, exact idempotency key, storage owner,
   and TTL (covering duplicate-in-flight, duplicate-after-commit, and
   post-expiry), replay/reconciliation path, and restoration point. Never retry
   `CommitUnknown` blindly.
4. **Cancellation & drop safety:** Behavior on client disconnect, deadline
   expiry, or process drain for in-flight futures, held locks, open
   transactions, pooled connections, streaming response permits, and spawned
   background tasks.
5. **Concurrency, interleaving & races:** Lock, unique constraint, isolation
   level, or atomic conditional write serializing concurrent callers, workers,
   or retries (TOCTOU, double-apply, duplicate delivery, stampede). For
   interacting flows sharing a guarantee, trace the smallest ordering,
   interruption, or replay that could break it (what happened, what the next
   actor knows, what the design permits) and close any breaking path or blocked
   recovery.
6. **Budgets & unit cost:** End-to-end deadline, retry, and drain budget
   arithmetic (`sum(sequential hops + retry backoff) <= caller/operation
   deadline` via `OperationContext`; worker/task drain `<= bootstrap shutdown
   budget` across `crates/service` and `crates/jobs-worker`). Per-unit cost
   (round trips, queries, held locks, buffered bytes) and the hard limit
   enforcing it.
7. **Operator signals & lifecycle:** Exact log event, metric label, span
   outcome, or probe state distinguishing each failure, degradation, and
   recovery path from neighbors and success, plus readiness (`crates/health`)
   and staged shutdown (`crates/service/src/bootstrap`, `crates/jobs-worker`).

## Rollout And Mixed-Version Impact

Record deployment impact (`none`, `standard single-step deploy`, or [Release
Closure](../../rubrics/release-closure.md) in `rollout.md`), covering
rolling-deploy mixed-version overlap (old and new replicas/workers running
together) and lock-safe DDL (`expand -> migrate -> contract`).
