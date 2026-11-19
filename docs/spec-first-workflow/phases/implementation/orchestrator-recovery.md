# Orchestrator Recovery

Load only when Ledger Orchestration hits a late or duplicate return,
interrupted integration, or mid-task steering and contract invalidation.

## Stale, Late, Or Duplicate Returns

Use the dispatch or steering record to tie every incoming return to the active
task, Lead identity, and accepted inputs, and check any returned candidate
against the tree. Only a current return may change ledger state, release locks,
or unlock dependents.

- **Duplicate `Implemented` after integration completed:** no action required.
- **Duplicate `Blocked`:** leave the active recovery with its current owner.
- **Obsolete or superseded return:** do not overwrite current ledger state;
  preserve useful edits from the obsolete candidate for the current owner.

## Interrupted Integration

Serial integration executes four checkpoints in order:

1. Verify scope/locks and land the candidate onto the integration branch as a
   clean local commit (or confirm the shared-checkout diff in `no-commit`
   mode).
2. Mark `[x]` and record the `Implemented` receipt (`decisions`, `unverified`)
   in `tasks.md`.
3. Release locks, clear `Execution`, and retire the Lead and its worktree.
4. Refill the Ready Frontier.

If interrupted mid-integration, inspect Git state, native agent state, and
`tasks.md`, and resume at the first unfinished checkpoint.

## Mid-Task Steering And Contract Invalidation

When reconciled user guidance, a write overlap, or an invalidated contract
changes inputs for active work:

1. Update the owning artifacts (`spec.md`, `design/`, `tasks.md`
   `Global constraints`, and affected packets) in place under
   [Artifacts](../../shared/artifacts.md).
2. Pause only conflicting writers or invalidated consumers while unrelated
   tasks continue.
3. Notify each affected live Lead one-to-one under [Agent
   Harness](../../../agent-harness.md#context-and-lifetime):
   - **Continue the live Lead** when the delta stays inside its unchanged
     outcome, capability tier, and current or parent-expanded disjoint scope.
   - **Replace with a fresh Lead** (stopping the old Lead and preserving useful
     partial edits first) when the delta changes outcome, reassigns or narrows
     writable scope, or raises required capability.
