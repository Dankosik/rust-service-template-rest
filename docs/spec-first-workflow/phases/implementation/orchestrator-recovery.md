# Orchestrator Recovery

## Stale, Late, Or Duplicate Returns

Match each return to the active task, Lead, accepted inputs and current tree
before recording state or releasing locks. Ignore completed duplicates, leave
an active blocked recovery with its owner, and preserve useful superseded
edits for the current owner.

## Interrupted Integration

Compare Git, native agent state and ledger receipts against the ordered
[Acceptance Transition](../implementation.md#3-acceptance-transition).
Resume at the first unfinished step. In no-commit mode use the recorded
snapshots and landed deltas to distinguish a completed landing from a partial one.

## Mid-Task Steering And Contract Invalidation

Apply [Resume](../../shared/resume.md) to reconcile changed inputs, notify
affected Leads and preserve independent work. For a new write overlap, pause
the conflicting writers and resolve the grant before either resumes.
