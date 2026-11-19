# System Probes

Single source of truth for the seven adversarial crossing probes applied in
[System Design](system-design.md) and verified in [Technical Design
Review](../technical-design-review.md). Attack every exposed runtime
**crossing**; omit inapplicable probes. Load a named skill only when its probe
fires on an open decision.

1. **Data model, tx scope & truth** *(load `rust-sqlx`)*: Who is the canonical
   writer per invariant, and what commits atomically vs outside any DB tx
   (**zero network I/O inside an open tx or held pool connection**)? Which
   `PK`/`UNIQUE`/`FK`/`CHECK` constraints, nullability/defaults, and indexes
   back each query—and if old rows are pruned, what stops a late retry from
   re-applying? For `crates/infra-cache`, what pins tenant-isolated keys, TTL,
   invalidation, and cache-down fallback?
2. **Trust & attacker paths** *(load `rust-security`)*: How could an untrusted
   caller bypass AuthN/AuthZ or tenant isolation across HTTP, gRPC, worker, or
   webhook redrive? Where could secrets or PII leak into `infra_http::Problem`,
   `tonic::Status`, `Debug`, logs, metrics, or spans? What bounds egress/SSRF
   and untrusted parser or decompression amplification?
3. **Failure semantics, `CommitUnknown` & degradation** *(load `rust-errors`,
   `rust-reliability`)*: Is each crossing error **retryable or terminal**
   (`service_failure::Code`, HTTP/gRPC status, worker disposition), and does
   each non-critical dependency **fail-closed or fail-open**? Where commit,
   publish, or remote call can land despite timeout, disconnect, or crash
   (`CommitUnknown`), resolve it via idempotency (in-flight, committed,
   post-expiry key/store/TTL), read-back, or reconciliation with an explicit
   restoration point rather than blind retry.
4. **Cancellation & drop safety** *(load `rust-tokio`)*: Drop the future both
   **before** and **after** the durable write (disconnect, deadline expiry,
   `tokio::select!` loser, shutdown drain). What state remains, how are partial
   reads/pops kept cancel-safe, and what ensures locks, open txs, pooled
   connections, permits, and spawned tasks never leak or orphan?
5. **Concurrency, interleaving & races** *(load `rust-sqlx` / `rust-tokio`)*:
   Which lock, unique constraint, isolation level, or atomic conditional write
   blocks TOCTOU, double-apply, duplicate delivery, and stampede? For
   interacting flows, write a 4-step **interleaving witness** (`Step 1 ->
   Interruption -> Actor 2 observes -> Permitted outcome`) and close every
   breaking or deadlocked path.
6. **Budgets, backpressure & config** *(load `rust-reliability`, `rust-config`,
   `rust-performance`)*: Prove the arithmetic (`sum(sequential hops + retry
   backoff) <= OperationContext deadline`; `drain <= bootstrap shutdown budget`
   in `crates/service` and `crates/jobs-worker`) with **one retry owner per
   failure boundary** (no stacked cross-layer retries). Pin per-unit caps
   (round trips, queries, locks, buffered bytes), concurrency/queue bounds
   (`Semaphore`, bounded channel, `buffer_unordered`), and shedding
   (`429`/`503` vs backpressure vs drop). For changed `crates/config` keys, fix
   section, units, default/required rule, cross-field startup validation, and
   inert-profile behavior.
7. **Operator signals & lifecycle** *(load `rust-observability`, `rust-tokio`)*:
   Which discriminating log event, **bounded-cardinality** metric label (never
   tenant/user/request IDs or raw errors), and span outcome expose failure? How
   does correlation cross async/queue hops, and what changes in readiness
   (`crates/health`) or staged shutdown (`crates/service/src/bootstrap`,
   `crates/jobs-worker`)?
