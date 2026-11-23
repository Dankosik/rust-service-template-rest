# Behavioral Probes

Disclosed exhaustive check for [Specification](../specification.md) (audited in
[Design Review](../design-review.md) and [Plan Review](../plan-review.md)).

**Exhaustiveness bar:** Apply all seven probes below to every touched entity and
external seam recorded in `spec.md`'s `## Actors, baseline & terminology`. A
probe clears **only** with a concrete observable rule, state/boundary/precedence
table, exact status or wire error code, or concrete interleaving scenario in
`R<n>` (pairing any low-stakes reversible convention with a labeled `A<n>` +
`Reopen if` behind that pinned rule, never as a substitute for pinning it), a
risk-anchored `G<n>`, or a verified primary-source reason why it does not apply.
**Abstract prose reassurances** (*"handled safely"*, *"rejected appropriately"*,
*"backward compatible"*) **fail**; an unresolved product or risk fork **reopens
the user** rather than clearing the probe. Keep every closure strictly WHAT
(leave storage, cache, migration, and profile mechanics to [Technical
Design](../technical-design.md)). Load a probe's paired domain skill only when
that probe leaves an external-observer decision open.

1. **Domain model, terminology & state transitions:**
   - Does any term collide with live code, `api/`, or [Domain
     Vocabulary](../../../repo-architecture.md#domain-vocabulary), or overload
     distinct concepts (`cancel` vs `void` vs `expire`, `account` vs `user` vs
     `tenant`)? Pin `Means here` / `Does not mean` and `task-local` vs
     `cross-task`.
   - What defines entity identity, grain, and units (minor currency units vs
     decimal, UTC timestamps, inclusive vs exclusive ranges)?
   - For each touched entity lifecycle, which state transitions are legal,
     forbidden (with exact rejection status/code), or idempotent on repeat?
2. **Input boundaries, collections & time** *(load `rust-api-contract`)*:
   - How do `omitted`, `null`, empty string/array, whitespace-only, duplicate
     keys/items, and unknown fields behave?
   - What are exact zero, negative, and upper-bound rules, and how do exact
     boundary comparisons (`== expires_at`, `== limit`, timezone/date cutoffs)
     resolve?
   - For collections and batches: what are the sort key, deterministic
     tie-breaker, empty-result shape, behavior when items mutate between pages,
     and partial-success vs all-or-nothing batch semantics?
3. **Concurrency, ordering & precedence:**
   - If two calls mutate the same entity concurrently (for example, pay vs
     cancel, update vs delete), which wins and what exact status, error code,
     and state does the loser observe? How do **out-of-order or stale-version**
     deliveries behave?
   - If multiple failure conditions collide at once (unauthenticated + malformed
     payload + conflict/replay), what is the deterministic precedence order?
   - Must the next read observe a completed write immediately
     (`read-after-write`), or is bounded staleness/async lag permitted (pinning
     the observable bound)?
4. **Promises, failure & replay** *(load `rust-reliability`, `rust-api-contract`)*:
   - On every relied-on response, status, event, webhook, or ack, can an
     observer see success before the promised effect holds (*false-positive
     success*) or miss a landed effect (*unobservable completion*)?
   - How do retries, **in-flight concurrent duplicates** (same key while the
     first attempt is still executing: wait vs `409` / retry signal),
     identical-body replays, modified-body replays, and **post-window replays**
     (after an idempotency or retention window expires) behave?
   - What is externally observed on caller timeout/disconnect mid-operation and
     on partial or unavailable dependency failures (fail closed vs degraded
     fallback)?
5. **Trust, authorization & privacy** *(load `rust-security`)*:
   - Which actor/identity, tenant, or scope may invoke each entry seam or
     observe each resource, and how do unauthenticated (`401`), forbidden
     (`403`), and existence-hiding not-found (`404`) responses differ?
   - Can any response, pagination cursor, idempotency key, error detail, or
     rate/quota bucket collide across callers or tenants or reveal resource
     existence?
   - What untrusted-input caps (body size, nesting, item count) and secret/PII
     redaction rules apply across wire responses, errors, logs, and audit
     signals?
6. **Pre-existing state & caller compatibility** *(load `rust-api-contract`)*:
   - What is the **externally observed fate of pre-existing persisted entities
     and in-flight work** once this ships (keep legacy rule, follow new rule
     immediately, or reject on next action—leaving backfill vs lazy-upgrade
     mechanics to Technical Design)?
   - Which existing callers, consumers, response shapes, defaults, headers, or
     exhaustive client variants must remain compatible?
   - Is zero-downtime caller compatibility required across deploy, or is a
     breaking change / maintenance window explicitly authorized—and if breaking,
     what exact response do former callers observe?
7. **Operational, config & quantitative contract** *(load `rust-config`,
   `rust-observability`, `rust-performance`)*:
   - For operator-facing config inputs: what are exact omitted, blank,
     malformed, and source-precedence semantics, and which invalid config or
     dependency failures reject startup vs degrade at runtime?
   - Which operator or audit signals (names, finite bounded-cardinality
     dimensions, absence meaning) are externally required?
   - For performance, capacity, or cost targets: what are the labeled numeric
     target, workload envelope, comparable baseline, and measurement boundary?

*Done when:* every touched entity and external seam in `## Actors, baseline &
terminology` clears all seven probes under the exhaustiveness bar above, with
zero unresolved product/risk forks.
