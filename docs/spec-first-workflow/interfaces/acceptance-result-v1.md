# Acceptance Result V1

The unit Lead reports code production. Only the Delivery Owner reports final
acceptance or requests final-validation repairs; a ledger task has no
acceptance verdict.

## Implementation Result

```text
unit: <T<ID> or fixed implementation scope>
verdict: Implemented | Blocked
candidate: <bounded diff, tree, commit, patch identity, or none if blocked>
provides: <implemented code or contract; no acceptance claim>
decisions: <choices the packet left open and how each was closed, deviations from the packet, or none>
unverified: <assumptions coded without a check, unit tests not yet run and why, or none>
next_owner: <next implementation owner, or exact missing input and its owner>
```

Return `Implemented` when the unit meets this bar:

- The packet's `Outcome` and `Observable claim` are coded and wired within its
  `Boundary`, with the tests, generated sources, and cleanup required and every
  placeholder body replaced.
- The changed Rust crates pass targeted type-check, lint, and formatting
  against the current candidate.
- Each test the unit added or changed executed with a non-zero pass count via a
  focused selector. A test requiring heavy or unavailable infrastructure is
  named in `unverified` instead.
- Every sub-lane the Lead started has joined or stopped.

[Build In Tight Vertical Slices](../phases/implementation/unit.md#2-build-in-tight-vertical-slices)
owns coding feedback. For a ledger, the Orchestrator applies [Acceptance
Transition](../phases/implementation.md#3-acceptance-transition) before
unlocking tasks that require integrated code. After mid-task steering changes
consumed inputs, identify the steering update the result answers, including
when returning `Blocked`. `Blocked` retains partial work and names the missing
implementation input. Neither verdict starts final validation while another
code task remains unfinished.

## Repair Request

Use only when the Delivery Owner finds a repairable in-scope failure during
[Final Validation](../phases/implementation/final-validation.md) of a ledger
candidate. Do not use `Blocked` for ordinary internal repairs.

```text
unit: Completion
verdict: NeedsRepair
candidate: <fixed assembled candidate where failure occurred>
classification: product | test_oracle | integration_defect | structure_defect
failed_claim: <anchored accepted claim, check, or review finding>
evidence: <exact failing command/output or review anchor>
owning_tasks: <T<ID> or seam pair T<ID> + T<ID>>
retained_evidence: <passing checks unaffected by this repair, or none>
```

The Orchestrator dispatches a fresh Lead in [Repair
Assignment](../phases/implementation/unit.md#repair-assignment) mode,
integrates the repair serially, and resumes the Delivery Owner with the updated
candidate identity and repair delta.

## Completion Result

Use only after every planned code task is `Implemented` and assembled. A
standalone single-task delivery also uses `Completion`; an individual task
inside a ledger cannot use this branch.

```text
unit: Completion
verdict: Accepted | Blocked
candidate: <fixed assembled candidate>
evidence: <local validation results, observed requested external outcomes, and material gaps>
review: <final review result or not_required under shared Review>
invalidated_receipts: <evidence invalidated by repair; omit when none>
next_owner: <exact remaining action and owner, or none>
```

Return `Accepted` once [Final Validation](../phases/implementation/final-validation.md)
passes for the changed surface and explicit additions, blocking findings are
resolved, and any selected review passes. If the request also includes an
external outcome, such as green CI or a deployment, its stated success
condition must be directly observed before returning `Accepted`. Report
material optional gaps in `evidence` without holding the verdict.

Retain passing local results for `evidence` while a requested external outcome
is pending. Continue its authorized execution, observation, or in-scope repair
and keep Completion open. Return `Blocked` only when no authorized progress
can obtain the required outcome or proof. Record the completed scope in
`evidence` and the missing input, capability, or authority in `next_owner`.

During orchestrated execution these results are input to the Orchestrator,
which records canonical state without repeating implementation or validation.
A root-local Lead may record its own result.
