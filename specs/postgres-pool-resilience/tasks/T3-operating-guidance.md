# T3 — Complete connection budgeting and sizing guidance

Outcome:
Replace incomplete connection sizing guidance with a usable deployment-wide
allocation method and a measurement-based way to consider later pool changes,
without claiming a production optimum or changing current defaults.

Consumes:
- [R3](../spec.md#r3-a-complete-connection-budget-and-sizing-method) — all connection owners, units, worker constraints, pooler separation and physical-session caveat.
- [Operating guidance design](../design/design.md#connection-budget-and-operating-guidance) — accepted formula, valid 82/100 example, worker/LISTEN treatment and workload evidence.
- [Persistence architecture](../../../docs/architecture/persistence.md), [configuration policy](../../../docs/configuration-source-policy.md) and current worker validation — canonical retained budgets and policy.
- T1/T2 assembled behavior/documentation — consumed at final consistency acceptance; accepted Design already supports independent authoring.

Provides:
- Direct PostgreSQL and PgBouncer budgeting guidance with rollout overlap, complete ownership, reserves, valid worker minima and a consistent worked allocation.
- A bounded workload-sizing method based on acquisition, occupancy, latency and database pressure, with explicit reopen evidence.

Boundary:
Own sizing/operating prose in existing documentation. Count peak service and
worker pools, separate LISTEN sessions, direct migrators, other users and reserve
capacity; distinguish pooler client and database backend limits. Preserve
ordinary and outbox-specific worker minima and current capacity defaults/range.
State that configured local ceilings do not cap transient lingering physical
sessions. T1 owns the return/custody explanation and T2 the observation-coverage
explanation; consume those sections without duplicating or overwriting them.
No configuration, runtime or production workload change is part of this unit.

Mutable owners:
- Connection-budget and sizing portions of `docs/architecture/persistence.md` and corresponding operating-evidence guidance in `docs/validation/postgres.md`.

Exclusive locks:
- The two guide files while changed; T1/T2 must release overlapping writes first. The accepted inputs permit authoring without waiting for their validation.

Final validation:
- Claim: An operator can account for every accepted connection owner and compare allocation to real configured limits without confusing average utilization or pooler clients with PostgreSQL sessions.
- Checks: Static arithmetic/semantic consistency and repository documentation link validation in the one assembled final boundary. No additional runtime scenario or benchmark is added by this packet.
- Observable: The worked allocation fits its stated allowance and respects worker minima; the guidance names overload/recovery evidence that can reopen a sizing decision and makes no production-optimal setting claim.

Reopen if:
Specification for a changed capacity/support/readiness requirement; Technical
Design for contradictory canonical allocation or ownership. Documentation
corrections within the accepted method remain here.

## Execution notes

Authored the direct-deployment formula and accepted 82/100 worked allocation,
including rollout overlap, worker LISTEN, direct migrators, other sessions/apps
and reserve. Current configuration confirms ordinary N+2, combined outbox N+5
and outbox-only three pool minima; the separate listener remains additional.
The PgBouncer section separates client limits from backend partition/cap
allocation across peak pooler replicas. The existing T1 physical-session caveat
is referenced and applied to allocation without changing its behavior contract.

The sizing method pairs acquisition/occupancy with useful-work latency and
database pressure, records fixed workload/configuration inputs, changes one
value within the valid allocation, observes recovery and restores a worsening
setting. T1 return/custody and T2 diagnostic/recovery sections were preserved.
No runtime/configuration edits, benchmark, validation command or review occurred;
static consistency and docs-check remain at assembled Completion.
