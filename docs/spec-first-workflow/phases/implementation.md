# Implementation

## Carrier

For a fixed unit without a ledger, use [Inline Unit Mode](implementation/modes.md#inline-unit-mode).
For a named ledger, inspect its status. Apply [Resume](../shared/resume.md)
when runtime state exists or execution was interrupted. Report a completed
ledger, route an unready plan to its recorded owner, or execute a ready ledger
as root Ledger Orchestrator (`LEDGER_ORCHESTRATOR`), including a one-task ledger.

## Orchestrator Loop

The Orchestrator schedules, integrates, and alone writes [Ledger Runtime
State](../interfaces/acceptance-result-v1.md#ledger-runtime-state). Delegate
candidate edits and test execution. Use the adapter's Acceptance-Unit Lead
(`ACCEPTANCE_UNIT_LEAD`) for Unit, Repair and Delivery assignments.

### 1. Verify Controls And Pin Baseline

Confirm full-ledger controls under [Agent Harness](../../agent-harness.md).
Select the integration worktree under [Branches and Worktrees](../../../AGENTS.md#branches-and-worktrees)
before writing ledger state. Keep the recorded Base on resume. Otherwise pin
the starting integration HEAD as Base. Keep the writable ledger in the integration worktree. If the accepted bundle
is untracked in the source checkout, bring its current contents into this
worktree before updating it. Keep it out of candidate commits and provide
its actual location to isolated Leads. Verify
the plan, packets, upstream readiness and frontier with `make ledger-check TASK=<task>`.
Resolve a missing control or invalid input under [Parent-Owned Recovery](../shared/transition.md#parent-owned-recovery).

Use local commits unless the user forbids them. In that case apply
[No-Commit Mode](implementation/modes.md#no-commit-mode) throughout.
Include active mode and user constraints in every affected brief.

### 2. Ready Frontier Dispatch

Dispatch a fresh Unit Lead for each eligible task in ledger priority order,
up to capacity. Use the [Ready Frontier](../../spec-first-workflow.md#terms)
and read only the ready packets' mutable owners and exclusive locks to assign
disjoint writes. Bound concurrent compiling Rust worktrees (typically 2–3)
and reserve capacity for integration, review and recovery.

Choose the checkout and account for descendants under [Delegation
Interface](../../agent-harness.md#delegation-interface). Serialize tasks that
need fixtures unavailable in isolation. Only the Orchestrator advances the
integration branch. Record each Lead's locator in `Execution`.

Send [Unit Implementation](implementation/unit.md), ledger and packet locations,
integration branch and tip, checkout mode, and relevant landed prerequisite,
shared-lock and same-crate commits or bounded diffs. The Lead returns an
[Implementation Result](../interfaces/acceptance-result-v1.md#implementation-result).

### 3. Acceptance Transition

Integrate current returns serially. For an interrupted landing, stale or
duplicate return, or changed assignment, apply [Orchestrator Recovery](implementation/orchestrator-recovery.md).

1. Check the assignment's delta against granted owners and locks. Reject any Lead
   change to the task bundle, including a committed change in an isolated
   candidate. Distinguish the Orchestrator's own ledger updates. In a shared checkout commit only verified paths. For an
   isolated candidate fast-forward the integration branch. If it cannot
   fast-forward, send the new tip to the same Lead for the [pre-return
   audit](implementation/unit.md#3-pre-return-self-audit-and-return).
2. Mark the task integrated and record its receipt under Ledger Runtime State.
3. Release locks, clear `Execution`, and retire the Lead and its worktree
   subject to [Cleanup](../shared/cleanup.md).
4. Refill the frontier immediately while unrelated Leads continue.

For scope expansion, grant disjoint paths before editing. If another writer
holds them, wait for landing and refresh the requester's tip before granting
the locks. For mutual contention, missing inputs or upstream gaps, preserve
partial work, release unused locks and apply Parent-Owned Recovery. Clear
`Execution` once the writer stops. Keep unintegrated tasks unchecked.

Proceed to Step 4 when all code tasks are integrated and writers have joined,
including when an external effect is pending. Apply [Authority](../../../AGENTS.md#authority)
before any external write or landing on an auto-deploy branch. Close as blocked only when
required work remains and recovery cannot advance it.

### 4. Final Validation And Repair Routing

Assign [Final Validation](implementation/final-validation.md) to one fresh
Delivery Owner who implemented none of the units. Send the ledger, accepted
bundle, integration branch, Base and fixed candidate identity/diff. At dispatch
and return verify HEAD, index, working tree and consumed bundle inputs still
match the fixed validation boundary. Preserve unexpected edits, establish their ownership and
restore the agreed boundary before re-running invalidated validation.

On `NeedsRepair` dispatch a fresh [Repair Lead](implementation/repair.md) as
sole writer in the shared checkout (isolated if required by the adapter).
Send the verbatim [Repair Request](../interfaces/acceptance-result-v1.md#repair-request),
branch, Base, current tip, affected packets, prior decisions and combined
writable scope. Apply the adapter's capability escalation for a missed invariant.

- On `Implemented`, use Step 3's scope check, landing and retirement procedure
  without changing task checkboxes. Resume the same Delivery Owner with the
  new candidate, repair-only diff, scope delta and repair decisions.
- On complete refutation, resume the Delivery Owner with the proof and close
  the Repair Lead without a landing.
- Route scope, input and upstream blockers as in Step 3. For a Delivery Owner
  blocker, resolve its named action under Parent-Owned Recovery and resume
  validation when possible.

### 5. Close

Record the terminal receipt. On `Accepted`, mark the ledger done and apply
[Cleanup](../shared/cleanup.md). If cleanup changes tracked documentation,
run `make docs-check` and `make typos` and commit the scoped change in commit
mode. Report impact, material limitations, pending external checks and
follow-ups. On terminal `Blocked`, preserve useful work and record
`status: blocked` and `Blocked stop:` with the exact missing input or
authority and its owner.
