# T2 — Durable-effect adoption on the admitted worker pool

Outcome:
A compilable opt-in producer/consumer example applies each equivalent logical
event once through a receipt and counter mutation in the existing caller Tx.

Consumes:
- [B3](../spec.md#b3-compilable-durable-effect-adoption-example), [effect and composition design](../design/system.md#durable-effect-example-and-uncertain-commit-b3), [ownership B/G](../design/ownership.md#responsibilities).

Provides:
- `test/examples/messaging_recovery.rs`, shared example `effect.rs`, example SQL and same-logic real-store proof; producer and worker modes reused by T4/T5.
- Outbox-gated `Registration::with_postgres_messages` using the sole admitted pool and message registry.

Boundary:
Keep receipt meaning, conflicts, READ COMMITTED unique-key arbitration and later
fresh-statement reconciliation exactly as selected. Unknown COMMIT remains
unresolved, never automatic closure replay or plain-read absence. Factory
declares consumer intent before I/O, activates before messaging admission and
uses existing startup cleanup. Include manifest, profile-pruning/inventory and
adoption docs needed for this outcome to stand alone. No default migration,
receipt TTL, second pool, generic inbox or role/concurrency policy change.

Mutable owners:
- `jobs-worker` registration/bootstrap and adjacent startup/lifecycle tests.
- New integration-package example/effect/schema and `test/tests/messaging_recovery.rs` B3 proof.
- `test/Cargo.toml`, associated existing profile markers/inventory and self-tests; example path classifier as needed; `docs/postgres-transactional-outbox.md` adoption sections.

Exclusive locks:
- Worker composition; integration-example manifest; profile/inventory/classifier contract; PostgreSQL-outbox document.

Final validation:
- Claim: B3 receipt/effect atomicity and uncertainty semantics hold in the actual worker/shared-pool path; retained/removed profiles compile coherently.
- Checks: Matching build/relevant tests, required real-PG/broker composition and existing projection gates; executor chooses exact cases/commands for consolidated Completion.
- Observable: First effect, persistent duplicate/restart, competing duplicate, rollback, conflict and uncertain-commit outcomes match B3; factory error/panic/cancellation retains the existing one-pool cleanup owner.

Reopen if:
The selected single factory cannot compose through current worker ownership,
or receipt meaning/retention needs different behavior. Reopen Technical Design
or Specification respectively; test technique is executor-owned.
