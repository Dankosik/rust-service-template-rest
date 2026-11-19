# System Design

Disclosed branch of [Spec And Design](../spec-and-design.md). Own system
mechanisms, runtime **crossings**, data and trust invariants, and rollout
compatibility.

## Mechanism Selection And Material Flows

- Compare only mutually exclusive substitutes solving the same problem against
  one concrete scenario. Delegate independent domain questions via [Read-Only
  Delegation](../../shared/read-only-delegation.md), resolving conflicts through
  [Parent-Owned Recovery](../../shared/transition.md#parent-owned-recovery).
- Decide durable delivery, replay, and reconciliation from [Repository
  Architecture](../../../repo-architecture.md). For signature-sensitive
  payloads, pin exact bytes, algorithm, and one deterministic non-secret vector.
- Trace each trigger from entry to caller-visible completion or durable
  finality using a compact **State × Event table** or **Crossing table** (draw
  a diagram only when tables cannot make ordering or recovery reviewable). At
  each real **crossing** (service, datastore, cache, queue, external provider,
  worker, config/secret boundary), fix owners, transferred responsibility,
  canonical representation (IDs, units, absence, ordering, freshness,
  consistency, finality), and produced effect.

## Crossing Probes

Load [System Probes](system-probes.md) and attack every exposed **crossing**,
loading only the domain skills whose probes fire.

## Rollout And Mixed-Version Compatibility

Load `rust-delivery-platform` when delivery gates or profiles change. Record
deployment impact (`no deployment impact`, `standard single-step deploy`, or
embedded [Release Closure](../../rubrics/release-closure.md) gates in
`spec.md` / `design.md`), covering rolling mixed-version overlap across
`crates/migrate`, `crates/service`, and `crates/jobs-worker`: lock-safe DDL
(`rust-sqlx`) **and** forward/backward compatibility of serialized rows/`JSONB`,
cache (`infra-cache`), job (`infra-jobs`), and event (`infra-messaging`)
payloads.

When `expand -> migrate -> contract` applies, state explicitly whether
`Contract` lands in **`same-release`** (compile/blast-radius refactor) or
**`multi-release`** (rolling mixed-version deploy boundary in the Rollout
section).
