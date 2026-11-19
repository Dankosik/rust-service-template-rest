# Planning Ledger Contract

Use only after Planning selects a persisted [Task Ledger
V1](../../interfaces/task-ledger-v1.md).

## Task And Acceptance Boundaries

Each ledger task owns one coherent repository outcome and its final acceptance
criteria, keeping layers of that outcome together while using subtask lanes for
parallel implementation and recording partial subresults inside the existing
task status. Separate tasks only for independently consumable outcomes or
distinct external gates.

The Lead returns `Implemented` when its planned code, tests, and cleanup are
ready and its writers have joined. The ledger records that milestone after
integration. Global Completion owns acceptance of the requested outcome.

## Ready Frontier

Attach each dependency to the earliest stage that genuinely consumes it—coding,
final acceptance, or an external effect—using [Task Packet
V1](../../interfaces/task-packet-v1.md) annotations so agreed contracts unlock
consumer coding immediately while live providers gate only required runtime or
external-effect steps.

Start a task once its required decisions, consumed contracts or `Implemented`
code, disjoint writable scopes, and implementation authority are present,
keeping acceptance and external-effect gates at final validation or release.

Dispatch ready independent work up to capacity when writers and exclusive locks
are disjoint, serializing overlapping mutations and refilling the frontier as
soon as each `Implemented` result integrates:

- **Pending later gates**: Complete local contract-backed coding, return
  `Implemented`, and leave later acceptance or provider gates in the ledger;
  return `Blocked` only when coding itself lacks a required input or authority.
- **Lead reuse**: Reassign a Lead to a related ready task once its prior delta
  is integrated and its lanes have joined, re-reading the new packet and keeping
  at most one active writable scope per Lead while reserving Orchestrator
  capacity for integration and recovery.
- **Scoped stops and steering**: On a write overlap, invalidated contract, or
  reconciled decision/guidance delta, update the owning artifacts and ledger in
  place, pause only conflicting writers or invalidated consumers, and notify
  affected live Leads one-to-one—continuing a Lead on a parent-granted disjoint
  scope expansion for the same outcome, or replacing it when outcome, reassigned
  scope, or capability changes—under [Agent
  Harness](../../../agent-harness.md#context-and-lifetime) while unrelated tasks
  continue.

## Acceptance Transition

During orchestrated execution, the Orchestrator is the sole writer of canonical
ledger state, verifying isolated candidate identities as integration
bookkeeping when consuming [Acceptance Result
V1](../../interfaces/acceptance-result-v1.md).

When an assignment or its inputs change, execution resumes, or a result arrives
late or twice, use the dispatch or steering record to tie the return to the
current task, Lead, and accepted inputs. Check any returned candidate against
the tree. If the same Lead could be reporting on an earlier packet, have the
current owner confirm which assignment and inputs produced the result or
return a fresh result. Only a current return can change ledger state, release
locks, or unlock dependents. Preserve useful edits from an obsolete return for
the current owner.

On a current `Implemented`, integrate the change serially into the local
development candidate, record it, release its locks, and refill the ready
frontier. A repeat after all four steps requires no action. If interrupted,
resume at the first unfinished step. Preserve external-effect boundaries
(push, deploy, auto-deploy branches). Return semantic merge conflicts to the
owning Lead against the assembled tree and invalidate affected proof.
On a current `Blocked`, retain
partial code and the missing input, reconcile writers, and route [Parent-Owned
Recovery](../../shared/transition.md#parent-owned-recovery). A duplicate of
that result leaves the existing recovery with its owner.

Maintain one replaceable result per task (using Git for history and checkboxes
strictly for `Implemented` progress), keeping `status: ready` whenever coding,
final validation, owner-held repair, or an authorized requested external step
can advance. This includes waiting for a running required check. Reserve
`blocked` for genuinely unavailable required inputs, capabilities, or authority.

Run one consolidated final validation stage across the ledger:

- **Entry gate**: Assign the delivery owner final validation only after every
  planned code task in the ledger is `Implemented` and assembled with zero
  active writers.
- **Pending external actions**: Run local final validation as soon as the
  candidate is assembled, even when a requested external step is pending.
- **Consolidated proof and review**: The delivery owner follows [Final
  Validation](../implementation.md#final-validation) and reports one
  [Completion Result](../../interfaces/acceptance-result-v1.md#completion-result)
  for the requested scope.
- **Repairs and verdict**: Route defects back to their implementation Leads
  (granting one Lead a combined writable scope across both sides of an
  `INTEGRATION_DEFECT` seam while no conflicting writer is active). The delivery
  owner resumes the validation plan after repair. The Orchestrator records the
  resulting verdict directly.

Mark `done` once final `Accepted` closes Completion, then apply
[Cleanup](../../shared/cleanup.md).

## Stop Rule

Apply the [Readiness Dry Run](../planning.md#readiness-dry-run) and the
[Planning review verdict](../task-review-readiness.md#verdict) to the fixed
ledger. Name a future live prerequisite as a final gate only when the accepted
task explicitly requires that execution or external effect.
