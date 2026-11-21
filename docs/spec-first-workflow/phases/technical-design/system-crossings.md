# System Crossings And Probes

Disclosed branch of [Technical Design](../technical-design.md) (audited in
[Design Review](../design-review.md)). Own runtime **crossings**, data and
trust invariants, failure/concurrency mechanics, budgets, and operator signals.

**Exhaustiveness bar:** Attack every exposed runtime **crossing** against all
seven probes below. A probe clears only with a concrete mechanism, exact
constraint or key name, budget equation from real config values, or 4-step
interleaving witness—or a verified primary-source reason why it does not apply;
abstract prose reassurances fail. Load a named domain skill only when its probe
opens an unresolved decision.

## Material Flows And Crossings

- Ground durable delivery, replay, outbox, and reconciliation contracts in
  [Production Contract](../../../production-contract.md#consistency-and-durability)
  and the owning `crates/infra-*` + `docs/decisions/*-decisions.md` before
  designing new mechanics. Delegate independent domain questions via [Read-Only
  Delegation](../../shared/read-only-delegation.md), resolving conflicts through
  [Parent-Owned Recovery](../../shared/transition.md#parent-owned-recovery).
- Trace each trigger from entry to caller-visible completion or durable
  finality using a compact **State × Event** or **Crossing table** (draw a
  diagram only when tables cannot make ordering or recovery reviewable).
- At each real **crossing** (service, datastore, cache, queue/outbox, worker,
  external provider, config/secret boundary), pin:
  - owning writer and transferred responsibility;
  - canonical representation (IDs, units, absence, ordering, freshness,
    consistency, finality);
  - produced effect (for signature-sensitive payloads, pin exact canonical
    bytes, algorithm, and one deterministic non-secret vector).

## Adversarial Crossing Probes

1. **Data model, tx scope & storage invariants** *(load `rust-sqlx`)*:
   - Who is the canonical writer per invariant, and what commits atomically vs
     outside any DB transaction (**zero network I/O inside an open tx or held
     pool connection**)?
   - Which exact `PK`/`UNIQUE`/`FK`/`CHECK` constraints, nullability/defaults,
     composite index order, and row-locking shape (`FOR UPDATE SKIP LOCKED`,
     optimistic version, append-only insert, keyset cursor) back hot queries
     without sequential scans or hot-row lock queues?
   - What bounds table, bucket, or queue growth (retention, lock-safe batch
     pruning, or TTL)?
   - When `infra-cache` is used, what pins tenant-isolated keys, TTL,
     invalidation, and cache-down fallback?
2. **Trust & attacker paths** *(load `rust-security`)*:
   - How could an untrusted caller bypass AuthN/AuthZ or tenant isolation
     across HTTP, gRPC, worker, or webhook redrive?
   - Where could secrets or PII leak into `infra_http::Problem`,
     `tonic::Status`, `Debug`, logs, metrics, or spans?
   - What bounds egress/SSRF and untrusted parser or decompression
     amplification?
3. **Failure semantics, `CommitUnknown` & replay** *(load `rust-errors`,
   `rust-reliability`)*:
   - Is each crossing error **retryable or terminal** (`service_failure::Code`,
     HTTP/gRPC status, worker disposition), and does each non-critical
     dependency **fail closed or fail open**?
   - Where commit, publish, or remote call can land despite timeout,
     disconnect, or crash (`CommitUnknown`), pin the exact resolution:
     idempotency (in-flight, committed, and post-expiry/pruned-row replay
     guard), read-back, or reconciliation with an explicit restoration point
     rather than blind retry.
4. **Cancellation & drop safety** *(load `rust-tokio`)*:
   - Drop the future both **before** and **after** the durable write
     (disconnect, deadline expiry, `tokio::select!` loser, shutdown drain).
   - What state remains, how are partial reads/pops kept cancel-safe, and what
     ensures locks, open txs, pooled connections, permits, and spawned tasks
     never leak or orphan?
5. **Concurrency, interleaving & races** *(load `rust-sqlx`, `rust-tokio`)*:
   - Which exact lock, unique constraint, isolation level, or atomic
     conditional write blocks TOCTOU, double-apply, duplicate delivery, and
     stampede?
   - For interacting flows, write a concrete 4-step **interleaving witness**
     (`Step 1 -> Interruption -> Actor 2 observes -> Permitted outcome`) and
     close every breaking or deadlocked path.
6. **Budgets, backpressure & config** *(load `rust-reliability`, `rust-config`,
   `rust-performance`)*:
   - Prove the arithmetic with real config defaults/limits
     (`sum(sequential hops + retry backoff) <= OperationContext deadline`;
     `drain <= bootstrap shutdown budget`) with **one retry owner per failure
     boundary**.
   - Pin per-unit caps (round trips, queries, locks, buffered bytes),
     concurrency/queue bounds (`Semaphore`, bounded channel,
     `buffer_unordered`), and shedding (`429`/`503` vs backpressure vs drop).
   - For changed `crates/config` keys, pin section, units, default/required
     rule, cross-field startup validation, and inert-profile behavior.
7. **Operator signals & lifecycle** *(load `rust-observability`, `rust-tokio`)*:
   - Which discriminating log event, **bounded-cardinality** metric label
     (never tenant/user/request IDs or raw errors), and span outcome expose
     failure?
   - How does correlation cross async/queue hops, and what changes in readiness
     (`crates/health`) or staged shutdown?

*Done when:* every trigger has a traced crossing or State × Event table (with
canonical bytes, algorithm, and a deterministic non-secret vector for
signature-sensitive payloads) and every exposed crossing closes all seven
probes with concrete mechanisms, arithmetic, or 4-step witnesses.
