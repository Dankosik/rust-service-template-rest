# Task Ledger V1

`specs/<task>/tasks.md` holds execution-changing state: the task graph index,
global completion target, and runtime receipts. Detailed `Provides`, sources,
boundaries, locks, and validation claims live in each task's [Task Packet
V1](task-packet-v1.md).

## Authoring Schema (Planning-owned)

```markdown
# Goal: <short request outcome>
status: draft | ready | blocked | done
Blocked stop: <missing input/decision and owner; omit unless blocked>
Completion: <falsifiable observable condition for the assembled request, plus any requested external outcome>
Global constraints: <active rules, cross-task pinned shared identifiers, shared invariants, or carried CONCERNS; omit when none>

## Tasks

- [ ] T<ID>: <one-line observable outcome> [<prefactor | shared-contract | integration-first | expand | migrate | contract; omit for vertical slice>]
  - Depends on: <T<ID> [stage], <gate> [effect:<action>], or none>
  - Packet: tasks/<ID>-<slug>.md
```

- Order tasks **topologically** (blockers before dependents) and by **dispatch
  priority** within each frontier (risk-retirement, `prefactor`,
  `shared-contract`, and critical-path blockers first).
- Start at `status: draft`; set `status: ready` after readiness review permits
  handoff, or `status: blocked` with `Blocked stop:` when preserving partial
  work alongside an upstream `blocked` handoff.
- **Stage syntax:**
  - Unannotated (`T1`) gates coding on `T1`'s integrated `Implemented` code —
    required whenever a task compiles against any Rust type, trait, function,
    config field, exhaustive enum/struct change, or SQLx schema from `T1`, or
    an accepted sequencing constraint (such as an `integration-first` risk
    gate) orders `T1` first. Omit a coding edge only when the task compiles and
    unit-tests standalone against a wire, env, or already-landed contract (use
    `T1 [acceptance]` if assembly still requires both).
  - `[acceptance]` gates final validation; `[effect:<action>]` gates a named
    external action (`deploy`, `release`).
- Keep IDs, non-default shape tags, and stage tags identical in each packet.
- `Global constraints` links accepted owners and pins cross-task shared
  identifiers once; update in place when guidance changes.
- Planning stops here; Runtime State below is Orchestrator-owned.

## Runtime State (Orchestrator-owned)

Managed under [Implementation](../phases/implementation.md#orchestrator-loop);
Planning omits these fields.

- `Base: <commit SHA>` (header, pinned before the first dispatch so worktrees,
  Final Validation, and Review share one exact diff baseline; add `Commit mode:
  no-commit` only when the user explicitly forbade local commits under
  [Implementation](../phases/implementation.md#1-verify-controls-and-pin-baseline)).
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
