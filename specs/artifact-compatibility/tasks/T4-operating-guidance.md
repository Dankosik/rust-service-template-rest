# T4 — State adoption and recovery compatibility boundaries

Outcome: Existing guidance leaves build identity, mixed-version operation and
independent-store recovery assumptions implicit. Make the existing guides explain
their real guarantees and the derived service's unresolved operational duties.

Consumes:
- [Specification 4–7](../spec.md#4-initialization-and-update-guidance-preserves-source-identity).
- [Design: initialization and source identity](../design/design.md#initialization-and-source-identity)
  and [operating guidance ownership](../design/design.md#operating-guidance-ownership).
- The accepted source/inventory/derived-artifact decisions in Design; documentation
  may describe intended gates but cannot label pending implementation or CI as proof.

Provides: Existing adoption, capability and production guides express locked
bootstrap/update custody, retained-artifact rollback, rolling compatibility and
fenced recovery across independently retained stores.

Boundary: Keep lock projection and source identity unchanged; document explicit
connected `cargo fetch --locked` before offline initialization, portable sync
custody and template.lock provenance. Replace stale coverage totals with canonical
inventory references. Bound historical reproducibility evidence and mutable inputs.
Explain retained digest/deployment identity plus compatible configuration; schema
expansion before contraction, endpoint/pooling limits, compatible consumers before
new payloads, jobs custody, and PostgreSQL/broker/object/cache recovery limits.
Production Contract owns the cross-store fenced sequence and service-owned fields.
No runtime/schema change, provider procedure execution, default RPO/RTO/retention,
automatic upgrade/restore or universal reconstruction guarantee.

Mutable owners:
- `README.md`, `docs/template-sync.md`, `docs/ci-cd-production-ready.md`,
  `docs/validation/containers.md` and Dockerfile comments for artifact claims.
- Rollback guidance in `docs/railway-deployment-profile.md` (watch forms belong T1).
- `docs/architecture/persistence.md`, `docs/durable-messaging.md`,
  `docs/background-jobs.md`, `docs/configuration-source-policy.md`,
  `docs/object-storage.md`, `docs/cache.md`, `docs/production-contract.md`.

Exclusive locks: the listed guides and Dockerfile comments; serialize the Railway
guide with T1 and any source-reading freeze with final validation/review. No
runtime build/profile selection changes are authorized through comment editing.

Final validation:
- Claim: Each accepted guide obligation reaches its existing owner, with no
  contradiction to runtime/generated authorities or the retained custody rule.
- Checks: Static consistency and documentation links at final assembled validation.
  No live provider, restore or rehearsal is required or claimed by this task.
- Observable: An operator can distinguish resolution, built artifact, retained
  rollback artifact and observed restore evidence, and identify service decisions
  required before resuming writes after cross-store recovery.

Reopen if: Current canonical behavior contradicts an accepted guidance mechanism
or new provider evidence changes its validity; return the smallest decision to
Technical Design/supporting Research. User-owned policy remains with the service.
