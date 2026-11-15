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
consumes its available code or agreed contract. Task boundaries do not create
check, review, self-review, or acceptance stages. Writing tests, generating
required sources, and reading callers remain implementation work.

### Feedback During Coding

Use bounded compile-only or type diagnostics (`make lint-changed` or
`cargo check -p <crate> --all-targets --locked` per [Validation
budget](../../../AGENTS.md#validation-budget)), or one cheap local boundary
scenario when an unverified boundary assumption would otherwise propagate into
substantial rework. Coding feedback is not a task-transition gate or acceptance
receipt, though observed in-scope defects must be repaired before returning
`Implemented` (retain commands and results for possible reuse under the Evidence
Contract).

Establish shared contracts and generated sources before dependent lanes consume
them. When implementation reveals invalid accepted behavior or architecture,
resolve that smallest decision owner. Repair test cases and oracles locally
when the expected product behavior remains unchanged. Callers, error handling,
cleanup, command locators, fixture relationships, and runner repairs for the
same accepted result stay with the executor. A new write surface updates locks
and scheduling; it does not itself reopen Planning.

Join or stop lanes using the candidate, return [Delegated Result
V1](#delegated-result-v1) to the Acceptance-Unit Lead (or `Implemented` through
[Acceptance Result V1](../interfaces/acceptance-result-v1.md)), and release the
implementation scope. Do not wait for proof, a reviewer, or other independent
units before returning it. `Implemented` supplies unverified implementation for
local dependent work; it is neither accepted behavior nor release authority.
The same owner remains available for defects found in final validation.

## Final Validation

Start only when every planned code task is Implemented and assembled, with no
remaining implementation blocker or writer. Finishing one task, one wave, or
all currently runnable tasks does not satisfy this condition. Do not recast a
ledger task as a standalone delivery to start verification early.

The delivery owner selects one non-overlapping plan for the ordinary local
criterion in [AGENTS.md](../../../AGENTS.md#validation-budget) and any explicit
additions. Ordinary Rust delivery uses [Validation
budget](../../../AGENTS.md#validation-budget): matching build and relevant unit
tests, not an automatic `make verify`. Use [Validation
Routing](../../validation-routing.md) only for mixed surfaces or a specialized
command branch. Load the [Evidence Contract](../shared/evidence-contract.md)
when judging explicit additions, reuse, claim scope, or unavailable
infrastructure. Reconcile packet requirements with their actual source;
agent-added runtime scenarios do not become mandatory by accumulation. Combine
claims covered by the same command.
Apply [Review](../shared/review.md) only at this final boundary, never per task.
Keep the assembled candidate unchanged while checks or review consume it. Join
or stop those readers before repair, then rerun only invalidated evidence.

Within the selected validation plan, collect cheap mechanical failures before
expensive execution. Cover affected production/test projects, required build
variants, generated clients, and accepted intermediate release versions;
one failing project must not hide independent diagnostics.

When runtime/integration proof or an accepted fixture scenario is explicitly
required, run the smallest representative scenario covering setup, behavior,
observation, and cleanup before expanding, reusing valid prepared environments
and keeping cleanup bounded on failure or cancellation. For isolated or remote
execution, resolve candidate/image identities, checksums, and host-specific
paths from task state or its manifest before consuming results, updating the
replacement record and invalidating only affected evidence after repair. Before
a potentially long check or resource wait, set an observation checkpoint in
existing execution state and inspect logs, process/resource state, or lock
owners via background/yielding execution rather than blindly waiting, recording
concrete delay causes without creating extra status artifacts. Classify any
failure as product, test/oracle, or environment: for an optional check's broken
environment, record the material gap and stop that path rather than provisioning
infrastructure; repair in-scope defects and required-check failures at their
shared source (checking broken setup preconditions first and gathering related
failures in scope) within the same executor loop without a coordinator handoff,
releasing validation locks while editing and applying [Parent-Owned
Recovery](../shared/transition.md#parent-owned-recovery) if another attempt
yields no new discriminating evidence.

Final local acceptance requires the AGENTS.md local criterion, explicit task
additions, resolved blocking findings, and any final review selected by shared
Review. Stop once that boundary passes. Missing optional runtime proof does not
block local `Accepted`; report only material limitations.
Missing required proof remains `implementation complete; verification incomplete`
for that scope. Continue available authorized repairs without indefinite
environment recovery. A requested CI, release, deployment, or runtime result
remains outstanding until actually obtained; never claim it from local tests.

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
