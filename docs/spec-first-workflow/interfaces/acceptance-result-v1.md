# Acceptance Result V1

Wire schemas for Implementation results and Orchestrator-owned ledger runtime
state. Unit and Repair Leads return `Implementation Result`; only the Delivery
Owner returns `Repair Request` or `Completion Result`.

## Implementation Result

```text
unit: <T<ID> or fixed implementation scope>
verdict: Implemented | Blocked
candidate: <commit/branch, bounded diff, or none if blocked>
provides: <implemented code or contract; no acceptance claim>
decisions: <only diff-independent facts siblings/downstream/Delivery Owner need: new/changed public symbols & signatures, error variants/codes, defaults, reusable test helpers, packet deviations, repair root causes/refutations; omit narration, or none>
unverified: <unchecked assumptions, unrun tests with exact reason, or none>
next_owner: <next implementation owner, or exact missing input and its owner>
```

- Return `Implemented` only after passing the [Pre-Return
  Self-Audit](../phases/implementation/unit.md#3-pre-return-self-audit-and-return)
  (single owner of the `Implemented` bar).
- After mid-task steering changes consumed inputs, identify the steering update
  the result answers (including on `Blocked`).
- `Blocked` names the exact missing input and owner (retaining partial work—as
  a WIP commit on the worktree branch when stopping an isolated Lead),
  `Ledger Orchestrator — scope expansion: <paths>` (live worktree preserved
  without a WIP commit), or `Delivery Owner — refuted findings` from [Repair
  Assignment](../phases/implementation/repair.md).

## Repair Request

Returned by the Delivery Owner during ledger [Final
Validation](../phases/implementation/final-validation.md#4-verify-findings-classify-and-route-repairs)
to batch all verified in-scope defects from one pass into a single fresh repair
Lead:

```text
unit: Completion
verdict: NeedsRepair
candidate: <fixed assembled candidate where failure occurred>
findings:
  - id: <F1>
    classification: TASK_DEFECT | INTEGRATION_DEFECT
    failed_claim: <anchored accepted claim, check, or verified review finding>
    anchor: <path::Symbol, hunk, or failing command>
    evidence: <exact failing output or verified counterexample>
    owning_tasks: <T<ID> or seam pair T<ID> + T<ID>>
retained_evidence: <passing checks unaffected by this repair, or none>
prior_attempt: <Round 1 candidate, attempted hypothesis, and recheck failure; omit on Round 1>
```

## Completion Result

Returned once every planned code task is `Implemented` and assembled (or for a
standalone single-unit delivery):

```text
unit: Completion
verdict: Accepted | Blocked
candidate: <fixed assembled candidate>
evidence: <local validation results, observed requested external outcomes, and material optional gaps>
review: <Review Result V1 or not_required (<checked trigger signals>)>
invalidated_receipts: <evidence invalidated by repair; omit when none>
next_owner: <exact remaining action and owner, or none>
```

- Return `Accepted` only when every gate in [Emit Completion
  Result](../phases/implementation/final-validation.md#5-emit-completion-result)
  holds; otherwise return `Blocked` as specified there.

## Ledger Runtime State

Orchestrator-owned fields appended to `specs/<task>/tasks.md` during
[Implementation](../phases/implementation.md#orchestrator-loop); Planning preserves integrated state on [reopen](../phases/planning.md#steps).

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
- Replace one receipt in place per task and one for `Completion` (Git keeps
  history; write non-empty `decisions` or `unverified` as indented sub-bullets
  under the task so primary details stay untruncated):

```text
Implemented: <unit>; verification: pending final validation; candidate: <commit or bounded diff>
  - decisions: <carried verbatim from Lead's result; omit when none>
  - unverified: <carried verbatim from Lead's result; omit when none>
Accepted: Completion; evidence: <claim-matched result>; candidate: <identity>
Blocked: <unit>; unverified: <claim>; evidence: <narrower result>; next owner: <owner and condition>; candidate: <identity or none>
```

Mark `status: done` only after final `Accepted` proves `Completion`.
