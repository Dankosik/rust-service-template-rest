# System Design

Disclosed branch of [Technical Design](../technical-design.md). Own the system
mechanism, data and trust invariants, and end-to-end material flows.

## Mechanism Selection And Material Flows

- Compare only viable same-level substitutes against one concrete scenario
  (never prerequisites, complements, or defense-in-depth layers). Delegate
  independent domain questions via [Read-Only
  Delegation](../../shared/read-only-delegation.md), resolving disagreements
  through [Parent-Owned Recovery](../../shared/transition.md#parent-owned-recovery).
- Decide durable delivery, replay, and reconciliation from [Repository
  Architecture](../../../repo-architecture.md). For signature-sensitive
  payloads, fix exact bytes, algorithm, and one deterministic non-secret vector.
- Trace each trigger from entry to caller-visible completion or durable
  finality. Use a compact **State × Event table** for stateful lifecycles and a
  **Crossing table** for multi-hop flows; draw a diagram only when tables or
  text cannot make ordering or recovery reviewable.
- At every real **crossing** (service, datastore, cache, queue, external
  provider, worker, config/secret boundary), fix owners, transferred
  responsibility, canonical representation (IDs, units, absence, ordering,
  freshness, consistency, finality), and produced effect.

## Seven Failure, Trust, And Operational Probes

Attack every exposed **crossing**; omit inapplicable probes:

1. **Data model, tx scope & truth:** Canonical writer and enforcing boundary
   per invariant; exact atomic tx scope (**zero network I/O inside an open DB
   tx or held pool connection**); access patterns and indexes; pruning bounds
   preserving replay/dedup; for `crates/infra-cache`, namespace,
   tenant-isolated key, TTL, invalidation, and cache-down fallback.
2. **Trust & attacker paths:** AuthN/AuthZ and tenant isolation on every
   effectful entry point (HTTP, gRPC, worker, webhook redrive) with bypasses
   closed; secret/PII custody and redaction across logs, metrics, spans,
   `Debug`, and `infra_http::Problem` / `tonic::Status` details; egress/SSRF
   and untrusted parser/decompression bounds.
3. **Failure semantics, `CommitUnknown` & degradation:** Classify crossing
   errors as **retryable vs terminal** (`service_failure::Code`, HTTP/gRPC, or
   worker disposition) and fix **fail-closed vs fail-open / fallback** per
   non-critical dependency. Where commit, publish, or remote call can succeed
   despite timeout, disconnect, or crash (`CommitUnknown`), fix retry owner and
   budget, idempotency key/store/TTL (in-flight, committed, post-expiry),
   replay/reconciliation, and restoration point. Never retry `CommitUnknown`
   blindly.
4. **Cancellation & drop safety:** Behavior on disconnect, deadline expiry, or
   drain for in-flight futures, `tokio::select!` arms (cancel safety of partial
   reads/pops), locks, open txs, pooled connections, response permits, and
   spawned tasks.
5. **Concurrency, interleaving & races:** Lock, unique constraint, isolation
   level, or atomic conditional write serializing callers, workers, or retries
   (TOCTOU, double-apply, duplicate delivery, stampede). For interacting flows,
   write a 4-step **interleaving witness** (`Step 1 -> Interruption -> Actor 2
   observes -> Permitted outcome`) and close breaking paths or blocked
   recovery.
6. **Budgets, backpressure & config:** End-to-end deadline, retry, and drain
   arithmetic (`sum(sequential hops + retry backoff) <= OperationContext
   deadline`; drain `<= bootstrap shutdown budget` in `crates/service` and
   `crates/jobs-worker`). Per-unit cost (round trips, queries, locks, buffered
   bytes), aggregate concurrency/queue cap (`Semaphore`, bounded channel,
   `buffer_unordered`), and overload shedding (`429`/`503` vs backpressure vs
   drop). For changed `crates/config` keys, fix section owner, units,
   default/required rule, cross-field startup validation, and inert-profile
   behavior.
7. **Operator signals & lifecycle:** Discriminating log event,
   **bounded-cardinality** metric label (never tenant/user/request IDs or raw
   errors), span outcome, cross-async-hop correlation propagation, readiness
   (`crates/health`), and staged shutdown (`crates/service/src/bootstrap`,
   `crates/jobs-worker`).

## Rollout And Mixed-Version Compatibility

Record deployment impact (`none`, `standard single-step deploy`, or [Release
Closure](../../rubrics/release-closure.md) in `rollout.md`), covering rolling
mixed-version overlap across `crates/migrate`, `crates/service`, and
`crates/jobs-worker`: lock-safe DDL (`expand -> migrate -> contract`) **and**
forward/backward compatibility of serialized rows, cache values
(`infra-cache`), job payloads (`infra-jobs`), and events (`infra-messaging`).
