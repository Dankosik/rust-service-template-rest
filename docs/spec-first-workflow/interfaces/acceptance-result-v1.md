# Acceptance Result V1

Compact return format for `ACCEPTANCE_UNIT_LEAD` (unit implementation and repair
passes) and Orchestrator-owned runtime state in `specs/<task>/tasks.md`.

## Implementation Result

```text
unit: <T<ID> or repair round>
verdict: Implemented | Blocked
candidate: <commit/branch, bounded diff, or none if blocked>
provides: <implemented contract; no acceptance claim>
decisions: <only diff-independent facts downstream needs: new/changed public symbols, error codes, defaults, test helpers, packet deviations, repair root causes/refutations; or none>
unverified: <unchecked assumptions or unrun tests with exact reason, or none>
next_owner: <Orchestrator, Orchestrator — scope expansion: <paths>, Orchestrator — refuted findings, or exact missing input and owner>
```

- Return `Implemented` only after passing [Unit
  Implementation](../phases/implementation/unit.md#steps) Step 3.
- On `Blocked`, preserve partial work (as a WIP commit in an isolated worktree;
  keep the live worktree untouched without a WIP commit for scope expansion).

## Ledger Runtime State

Appended to `specs/<task>/tasks.md` strictly by the Ledger Orchestrator during
[Implementation](../phases/implementation.md#orchestrator-loop) (preserved on
Planning reopen):

- Header fields: `Base: <commit SHA>` (pinned before first dispatch so
  worktrees, verification, and review share one diff baseline) and optional
  `Commit mode: no-commit` when the user forbade local commits.
- Per-task routing: `Execution: <native agent locator>` (set on dispatch, clear
  when the writer stops) and optional `Resume: <remaining action>` only when
  native state cannot recover after interruption.
- Checkboxes (`[x]`) mark integrated `Implemented` code only; `Blocked` stays
  unchecked. Replace one receipt in place per task and one final `Completion`
  receipt:

```text
Implemented: <unit>; candidate: <commit or bounded diff>
  - decisions: <carried verbatim; omit when none>
  - unverified: <carried verbatim; omit when none>
Accepted: Completion; evidence: <checks + review verdict>; candidate: <identity>
Blocked: <unit>; unverified: <claim>; next owner: <owner and condition>; candidate: <identity or none>
```

Flip `status: done` only after final verification and required review pass.
