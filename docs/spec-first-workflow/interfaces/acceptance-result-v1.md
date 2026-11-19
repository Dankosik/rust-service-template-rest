# Acceptance Result V1

Wire schemas for Implementation results. Unit and Repair Leads return
`Implementation Result`; only the Delivery Owner returns `Repair Request` or
`Completion Result`.

## Implementation Result

```text
unit: <T<ID> or fixed implementation scope>
verdict: Implemented | Blocked
candidate: <commit/branch, bounded diff, or none if blocked>
provides: <implemented code or contract; no acceptance claim>
decisions: <choices downstream/siblings/Delivery Owner may depend on, packet deviations, repair root causes/refutations, or none>
unverified: <unchecked assumptions, unrun tests with exact reason, or none>
next_owner: <next implementation owner, or exact missing input and its owner>
```

- Return `Implemented` only after passing the [Pre-Return
  Self-Audit](../phases/implementation/unit.md#3-pre-return-self-audit-and-return)
  (single owner of the `Implemented` bar).
- After mid-task steering changes consumed inputs, identify the steering update
  the result answers (including on `Blocked`).
- `Blocked` retains partial work and names the exact missing input and owner
  (or `Delivery Owner — refuted findings` from [Repair
  Assignment](../phases/implementation/repair.md)).

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
  holds.
- Retain passing local results in `evidence` while an explicitly requested
  external outcome is pending; return `Blocked` only when no authorized
  progress can obtain the required outcome or proof, recording completed scope
  in `evidence` and the exact gap and owner in `next_owner`.
