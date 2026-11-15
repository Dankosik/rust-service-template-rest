# Implementation

Use when accepted implementation work is ready and authorized. Implement the
planned tasks and their tests; validate the assembled result at the end.

## Carrier

Use one root-local Acceptance-Unit Lead for a single fixed unit. Use the Ledger
Orchestrator when several units, isolated handoffs, or durable scheduling need
one routing owner. The Orchestrator schedules; Leads implement their assigned
units. The [Planning Ledger Contract](planning/ledger-contract.md) owns
implementation progress, dependency unlocking, and final acceptance.

## Execution

Load the current task packet and consumed outputs. Choose and write the task's
tests alongside production code from accepted behavior and existing test
patterns. Reuse sufficient tests; add cases for changed behavior that existing
tests would miss. Select fixtures, assertions, and the smallest proving layer
locally, without a preliminary matrix, approval, or separate test-planning task.
For a non-obvious test technique, consult the matching method within this task.
Reopen only genuinely unresolved product behavior or architecture; a missing or
incorrect test case stays with the executor.

For a non-trivial fixture, find an existing valid fixture or producer while
writing the test. Otherwise derive the required entities, relationships, and
state transitions from the authoritative contract before expanding scenarios.
Keep that construction in test code, not a separate preparation artifact.
Execution follows the feedback and final-validation boundaries below.

Implement directly when handoff costs more than it saves. Run independent tasks
and useful subtask lanes in parallel when accepted interfaces are stable, writable owners and exclusive
locks are disjoint, and capacity permits. Integrate mutations serially. Apply
[Agent Harness](../../agent-harness.md) when delegating or replacing a lane.

A finished implementation immediately unlocks downstream implementation that
consumes its available code or agreed contract; writing tests, generating
required sources, and reading callers stay inside implementation work, while
validation and review wait for the final assembled boundary.

### Feedback During Coding

Use bounded compile or type checks (`make lint-changed` or
`cargo check -p <crate> --all-targets --locked` per [Validation
budget](../../../AGENTS.md#validation-budget)), or one cheap local boundary
probe when an unverified assumption could cause substantial rework. Repair
observed in-scope defects before returning `Implemented`, and retain command
results for reuse under the Evidence Contract.

Establish shared contracts and generated sources before dependent lanes consume
them: when parallel tasks or lanes share a new or changed Rust seam, land the
boundary types, traits, and signatures first (verifying them with `cargo check`
and replacing any temporary placeholder bodies before returning `Implemented`)
so producers and consumers code against checked types rather than coordinating
in prose. Reopen the smallest decision owner only when implementation exposes
invalid accepted behavior or architecture; keep test-case, oracle, caller, error,
cleanup, locator, fixture, and runner repairs with the executor. When a write
surface grows, route the scope delta to the scheduling owner (Orchestrator, or a
root-local Lead directly) before editing outside the assigned scope, updating
locks and continuing the lane when disjoint without reopening Planning.

Join or stop child lanes, return [Delegated Result V1](#delegated-result-v1) to
the Acceptance-Unit Lead (or `Implemented` via [Acceptance Result
V1](../interfaces/acceptance-result-v1.md)) immediately, and release the
writable scope so downstream units can proceed while keeping the Lead available
for final-validation repairs.

## Final Validation

Start final validation once every planned code task in the unit or ledger is
`Implemented` and assembled with all writers joined.

Select one non-overlapping validation plan covering the ordinary local criterion
in [AGENTS.md](../../../AGENTS.md#validation-budget) (matching build and
relevant tests) plus any explicit task additions, consulting [Validation
Routing](../../validation-routing.md) for mixed or specialized surfaces and the
[Evidence Contract](../shared/evidence-contract.md) for explicit additions,
reuse, claim scope, or unavailable infrastructure. Reconcile packet annotations
with their authoritative source and combine claims satisfied by one command.
Run [Review](../shared/review.md) once at this assembled boundary while keeping
the candidate frozen; join or stop readers before repairing, then rerun only
invalidated checks.

Run fast mechanical checks ahead of expensive suites, covering affected
production and test targets, required build variants, generated clients, and
intermediate versions so independent diagnostics surface together:

- **Runtime or fixture scenarios**: When explicitly required, run the smallest
  end-to-end scenario covering setup, behavior, observation, and bounded cleanup
  first, reusing valid prepared environments.
- **Isolated or remote execution**: Resolve candidate or image digests,
  checksums, and host paths from task state or its manifest before consuming
  results, updating replacement locators and invalidating only affected proof
  after repair.
- **Long checks and resource waits**: Record an observation checkpoint in
  existing execution state and inspect logs, processes, or lock owners via
  background execution so concrete delay causes are visible in place.
- **Failure classification and repair**: Classify failures as product,
  test/oracle, or environment. Record material gaps when an optional check's
  environment is unavailable; repair required-check and in-scope product or test
  failures at their shared root inside the executor loop (releasing validation
  locks while editing) and apply [Parent-Owned
  Recovery](../shared/transition.md#parent-owned-recovery) when repeated
  attempts add no discriminating evidence.

Accept locally once the AGENTS.md criterion, explicit additions, blocking
findings, and any selected final review pass: report optional runtime gaps as
limitations, mark missing mandatory proof `implementation complete; verification
incomplete`, and keep requested CI, release, deployment, or runtime outcomes
open until directly observed.

## Delegated Result V1

Every mutable delegated worker returns:

```text
result: <completed bounded outcome or no result>
changed_paths: <repository-relative paths or none>
proof: <omit for initial implementation; actual scoped results and gaps for final-validation repair or standalone verification>
candidate: <identity when isolated or none>
gap: <remaining blocker or none>
```

The result is input to the Acceptance-Unit Lead, not acceptance, review, or
ledger state.
