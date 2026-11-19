# Ledger Execution

Use as Ledger Orchestrator when executing a ready
[Task Ledger V1](../../interfaces/task-ledger-v1.md). Each Lead returns
`Implemented` at the
[Implemented bar](../../interfaces/acceptance-result-v1.md#implementation-result).
The ledger records that milestone after integration. Global Completion owns
acceptance of the requested outcome.

## Ready Frontier

A task enters the frontier when its required decisions, coding inputs (agreed
contracts or integrated `Implemented` code), accepted ordering constraints, and
implementation authority are available, keeping `[acceptance]` and
`[effect:<action>]` gates at final validation or release. Check its writable
scope against active writers and its exclusive locks against held locks. If two
otherwise ready tasks need the same free lock, both belong to the frontier.

## Dispatch

Choose concurrent tasks from the frontier up to capacity, with disjoint writable
scopes and exclusive locks. Use priority to choose among eligible tasks. Refill
the frontier as each `Implemented` result integrates and its locks are released:

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

On a current `Implemented`, integrate the change serially into the candidate:
in a shared checkout, confirm the returned paths lie inside the granted scope
and match the tree; from an isolated worktree, merge the returned candidate.
Record the result with its `decisions` and `unverified` entries, release its
locks, and refill the ready frontier. A repeat after all four steps requires no action. If interrupted,
resume at the first unfinished step. Preserve external-effect boundaries
(push, deploy, auto-deploy branches). Return semantic merge conflicts to the
owning Lead against the assembled tree and invalidate affected proof. On a
current `Blocked`, retain partial code and the missing input, reconcile writers,
and route [Parent-Owned
Recovery](../../shared/transition.md#parent-owned-recovery). A duplicate of
that result leaves the existing recovery with its owner.

Maintain one replaceable result per task (using Git for history and checkboxes
strictly for `Implemented` progress), keeping `status: ready` whenever coding,
final validation, owner-held repair, or an authorized requested external step
can advance. This includes waiting for a running required check. Reserve
`blocked` for genuinely unavailable required inputs, capabilities, or authority.

### Completion Gates

Once all planned code tasks are `Implemented` and assembled and all writers
have joined, assign the assembled candidate to the Delivery Owner for one
final validation. Assign a Lead that implemented none of the ledger's units; a
one-task ledger may keep its Lead. Start local validation even if a requested external action
is pending.

### Delivery

- **Consolidated proof and review**: The Delivery Owner follows [Final
  Validation](final-validation.md) and reports one
  [Completion Result](../../interfaces/acceptance-result-v1.md#completion-result)
  for the requested scope.
- **Repairs and verdict**: Route defects back to their implementation Leads
  (granting one Lead a combined writable scope across both sides of an
  `INTEGRATION_DEFECT` seam while no conflicting writer is active). The Delivery
  Owner resumes the validation plan after repair. The Orchestrator records the
  resulting verdict directly.

Mark `done` once final `Accepted` closes Completion, then apply
[Cleanup](../../shared/cleanup.md).
