# Task Ledger V1

`specs/<task>/tasks.md` holds execution-changing state: the task graph index,
global completion target, and runtime receipts. Detailed sources, boundaries,
locks, and validation claims live in each task's [Task Packet
V1](task-packet-v1.md).

## Authoring Schema (Planning-owned)

```markdown
# Goal
status: draft | ready | blocked | done
Completion: <falsifiable observable condition for the entire assembled request, plus any explicitly requested external outcome>
Global constraints: <active execution instructions, shared non-obvious invariants, or carried review CONCERNS; omit when none>

## Tasks

- [ ] T<ID>: <one-line observable outcome>
  - Depends on: <T<ID> [stage], <gate> [effect:<action>], or none>
  - Provides: <short handle consumed downstream or by Completion>
  - Packet: tasks/<ID>-<slug>.md
```

- Order tasks **topologically** (initial Ready Frontier first, every blocker
  before its dependents).
- Start at `status: draft`; flip to `status: ready` only after readiness review
  permits handoff.
- **Dependency stage syntax:** unannotated (`T1`) gates coding on `T1`'s
  integrated `Implemented` code (omit the edge when a contract is already
  settled in Design and needs no code from `T1`); `[acceptance]` gates only
  final validation; `[effect:<action>]` gates only a named external action
  (e.g. deploy, release). Keep IDs and tags identical in each packet's `Depends
  on`.
- `Global constraints` links accepted owners instead of copying specs and is
  updated in place when guidance changes.
- Planning stops here; Runtime State below is Orchestrator-owned.

## Runtime State (Orchestrator-owned)

Managed under [Implementation](../phases/implementation.md#orchestrator-loop);
Planning omits these fields.

- `Base: <commit SHA>` (header, pinned before the first dispatch so worktrees,
  Final Validation, and Review share one exact diff baseline; add `Commit mode:
  no-commit` only when the user explicitly forbade local commits).
- `Blocked stop: <unavailable required input and owner>` (header, only when
  blocked).
- Routing fields per task (not acceptance evidence): `Execution: <native task or
  agent locator>` (set on dispatch, clear when writers stop) and `Resume:
  <remaining action and indispensable verified context>` (only when native state
  cannot recover after interruption; delete on resume).
- Checkboxes (`[x]`) record integrated `Implemented` code only; `Blocked` stays
  unchecked, and a checked task makes no acceptance claim while final validation
  or later gates remain open.
- Replace one [Acceptance Result V1](acceptance-result-v1.md) in place per task
  and one for `Completion` (Git keeps history; write non-empty `decisions` or
  `unverified` as indented sub-bullets under the task so primary details stay
  untruncated):

```text
Implemented: <unit>; verification: pending final validation; candidate: <commit or bounded diff>
  - decisions: <carried verbatim from Lead's result; omit when none>
  - unverified: <carried verbatim from Lead's result; omit when none>
Accepted: Completion; evidence: <claim-matched result>; candidate: <identity>
Blocked: <unit>; unverified: <claim>; evidence: <narrower result>; next owner: <owner and condition>; candidate: <identity or none>
```

Mark `status: done` only after final `Accepted` proves `Completion`.
