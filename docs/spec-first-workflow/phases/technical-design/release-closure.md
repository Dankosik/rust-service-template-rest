# Release Closure

Single source of truth for deployment impact, rolling mixed-version
compatibility, and ordered release gates in [Technical
Design](../technical-design.md) and [Design Review](../design-review.md). Load
`rust-delivery-platform` when delivery gates or profiles change.

## Deployment Impact And Mixed-Version Compatibility

Classify every change from current primary evidence:

- **`no deployment impact`:** verified evidence that no deployed binary,
  migration, worker, wire contract, persisted payload, config/secret, or
  operator path changes.
- **`standard single-step deploy`:** single deployable rollout with verified
  mixed-version compatibility and zero external ordering prerequisites.
- **Embedded `Release Closure` gates:** required whenever deploy ordering,
  schema/payload evolution, new required config/trust material, or managed
  dependency changes span live versions.

For any change touching persisted state, wire contracts, or multiple binaries,
prove rolling mixed-version survival across retained binaries (`crates/migrate`,
`crates/service`, `crates/jobs-worker`):

- **Lock-safe DDL (`rust-sqlx`):** migrations must run safely while old and new
  replicas execute concurrently.
- **Forward/backward payload compatibility:** old and new binaries must safely
  read/write serialized DB rows/`JSONB`, cache (`infra-cache`), job
  (`infra-jobs`), and event (`infra-messaging`) payloads during overlap.
- **`expand–migrate–contract` release boundary:** state explicitly whether
  `Contract` lands in **`same-release`** (compile/blast-radius refactor only) or
  **`multi-release`** (post-deploy release boundary kept outside Release 1's
  ledger).

## Ordered Release Gates

When the rollout sequence is non-trivial, embed the following directly inside
`design.md`'s `## Crossings, failure semantics & rollout` section:

- **Affected deployment graph:** smallest set of touched nodes and edges,
  naming each owner, current-to-target state, canonical contract/config,
  network path, affected producers/consumers, and mixed-version obligation.
- **Last rollback-safe state** and integrated completion signal through the
  user-visible or durable path (provider deploy status or one component's
  health is not system proof).
- **Ordered gates** recorded as:

```text
owner/node | prerequisite | action | success signal | distinct safe-failure signal | duration or behavior-changing horizon | rollback/roll-forward | proof/readback
```

A remote latency-sensitive edge without current budget evidence is blocked;
missing authority or target-only proof narrows the completion claim.

*Done when:* deployment impact is classified from primary evidence, mixed-version
compatibility and `same-release` vs `multi-release` boundaries are explicit, and
every non-trivial rollout carries its graph, last rollback-safe state, ordered
gates, and integrated proof signal.
