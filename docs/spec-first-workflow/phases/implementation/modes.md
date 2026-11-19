# Execution Modes

## No-Commit Mode

When the user forbids local commits, record `Commit mode: no-commit` and
serialize all writers in the integration worktree. Use `git diff <Base>` for
the assembled change. Mark newly created, verified candidate files with
`git add -N -- <paths>` so this diff includes them. Exclude the task bundle.

Before each unit, repair, validation or validation resume, pin the current
candidate. Retain HEAD, index state and a recoverable snapshot of the content,
paths and file modes of the scoped worktree changes, including new files and
deletions. Keep temporary snapshots private and outside the candidate; exclude
credentials, ignored fixtures and the task bundle. Track newly appearing
untracked paths before including their contents. Preserve snapshots through
their last consumer.

On every return verify HEAD and index against their pinned state. Only the
Orchestrator marks verified new files intent-to-add, after checking the
writer's delta. Preserve and reconcile unexpected commits or staging before
consuming a result. Compare working-tree content before and after each writer,
including paths changed by earlier units. Verify the actual delta against this writer's grant. Record that delta
in place of a landing commit. Give downstream Leads the relevant bounded diffs.
Pass the before/after repair delta to the Delivery Owner and reviewer.
Identify a prior attempt by snapshot and repair summary. For a read-only
assignment, any candidate or index change invalidates its return. Preserve
unexpected edits and recover the pinned candidate before continuing. Apply
[Cleanup](../../shared/cleanup.md) to snapshots and leave tracked closeout
edits uncommitted.

## Inline Unit Mode

For a fixed unit without a ledger, the root-local Lead pins Base before editing
and uses the accepted inline scope and obligations wherever the ledger method
would read a packet, ledger or Completion criteria. Apply [Unit
Implementation](unit.md), then [Final Validation](final-validation.md) on the
fixed change. The same Lead owns both steps without dispatch or result handoff.
For uncommitted changes the root-local Lead performs the snapshot, scope
check and intent-to-add steps above.

Decide disjoint scope and dependency changes directly within existing authority.
Reopen invalid behavior, architecture or a zero-delta assignment under
[Transition](../../shared/transition.md). On a confirmed defect, perform
[Repair Assignment](repair.md) directly. Resume validation with the repair
delta or refuting evidence. Keep the selected independent review and recheck
boundaries.
