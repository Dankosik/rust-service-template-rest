# System Design

Load from [Technical Design](../technical-design.md) when Implementation would
otherwise choose a runtime boundary, source of truth, material crossing,
failure/cancellation/recovery behavior, scale/cost bound, operator signal, or
rollout mechanism. Own one behaviorally equivalent system mechanism and its
end-to-end flows without prescribing Rust function bodies or task slicing.

## Mechanism Selection And Reuse

- When several viable same-level substitutes could meet the same obligation,
  compare them using the same concrete scenario—including the current mechanism
  when viable—showing where their consequences differ on a material driver;
  weigh against all accepted drivers, choose the simplest when equivalent, or
  name the single fact that settles the choice when only one is viable. Never
  compare prerequisites, complements, or defense-in-depth layers as
  substitutes.
- Follow the reuse ladder: existing repository owner -> Rust `std` and locked
  workspace crates -> approved infrastructure or maintained OSS (checked
  against [Backend Library Selection](../../../backend-library-selection.md)
  and `docs/decisions/`) -> custom infrastructure mechanism only when a named
  current requirement or lifecycle cost rules out existing options and their
  supported extension points.
- Durable delivery, replay, and reconciliation have no dedicated skill; decide
  them from [Repository Architecture](../../../repo-architecture.md) and record
  the decision.
- For a hard-to-reverse open fork, use [Design It Twice](design-it-twice.md).
  For an independent bounded domain question, use [Read-Only
  Delegation](../../shared/read-only-delegation.md); resolve any surviving
  technical disagreement through [Parent-Owned
  Recovery](../../shared/transition.md#parent-owned-recovery) before closing.
- For any signature-sensitive wire or payload shape, fix exact bytes,
  algorithm, and one deterministic non-secret vector.

## End-To-End Material Flows

Trace each affected actor or trigger from entry to caller-visible completion or
durable finality. Show current vs target flows only when their difference
changes a decision. Use a diagram only when compact text cannot make ordering,
fan-out, transformation, or recovery reviewable; normative text and canonical
contracts remain authoritative.

At every real crossing (service boundary, datastore, queue, external provider,
background worker, or config/secret boundary), close only what Implementation
must not invent: initiating and receiving owners, transferred responsibility,
canonical contract and representation (identifiers, units, absence, ordering,
freshness, consistency, and finality), and produced state or effect.

## Six Failure And Operational Probes

Attack every real crossing in the selected mechanism with the probes it
exposes; omit inapplicable probes rather than writing `N/A` boilerplate:

1. **Source of truth & enforcement:** For each critical invariant, name the
   single canonical writer, the mechanism and boundary that enforce it, and
   every effectful path that could bypass it.
2. **Ambiguous outcome (`CommitUnknown`) & recovery:** Where a database commit,
   queue publish, or remote call can succeed despite a timeout, disconnect, or
   crash, name the retry and exhaustion owner, idempotency key and window,
   replay or reconciliation path, and restoration boundary. Never retry
   `CommitUnknown` blindly.
3. **Cancellation & drop safety:** When a caller disconnects, a request
   deadline expires, or the process drains mid-flight, state what happens to
   in-flight futures, held locks, open transactions, pooled connections, and
   spawned background work.
4. **Concurrency, interleaving & races:** Where concurrent requests, workers,
   or retries touch shared state (TOCTOU, double-apply, duplicate delivery,
   cache stampede), name the lock, unique constraint, transaction isolation
   level, or atomic conditional write that serializes them. When interacting
   flows affect the same guarantee, trace the smallest ordering, interruption,
   or replay that could break it: identify at the decisive step what happened,
   what the next actor can know, and what the design permits it to do,
   resolving any case where an allowed action breaks the guarantee or blocks
   required recovery.
5. **Unit cost & bound:** For each flow run per request, message, or job, or
   over caller-controlled input, state its cost per unit in round trips,
   queries, held locks, and buffered bytes—and what hard limit bounds
   amplification, queueing, and memory. Derive the cost from the mechanism;
   measure only when an accepted budget or Research requires it.
6. **Operator signal & lifecycle:** For each failure, degradation, or recovery
   path, name the specific log event, metric label, span outcome, or probe
   state that tells an operator this path apart from its neighbors and from
   success, plus how any new dependency or worker participates in readiness
   (`crates/health`) and staged shutdown (`crates/service/src/bootstrap`).

## Rollout And Mixed-Version Impact

Record deployment impact explicitly (`none` or `standard single-step deploy`
when verified, or the [Release Closure](../../rubrics/release-closure.md) gate
sequence and `rollout.md` when triggered), accounting for mixed-version rolling
deploys where old and new replicas serve traffic simultaneously.
