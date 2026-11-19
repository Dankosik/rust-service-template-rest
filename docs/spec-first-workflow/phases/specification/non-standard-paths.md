# Non-Standard Specification Paths

Load from [Specification](../specification.md) only when triggered.

## Reopen Or Resume From Blocked

Read existing `specs/<task>/spec.md` alongside `intent.md`. Reconcile only
invalidated clauses under [Artifacts](../../shared/artifacts.md), keep surviving
`R<n>`/`G<n>` IDs stable (retire deleted IDs instead of renumbering), re-verify
any prior `Carried concerns` (dropping resolved or superseded items), and drop
every resolved `B<n>` entry before either writing `status: draft` in step 3 or
updating `Blocked decisions` for newly uncovered child forks. Continue
[Specification](../specification.md) from step 1.

## Wrong Premise In Intent

Primary sources contradict a baseline premise of `intent.md`. If
`specs/<task>/spec.md` already exists (reopen or existing `draft`), set
`status: blocked`, record the verified discrepancy in `Blocked decisions`, and
return `result: specs/<task>/spec.md`; otherwise return the discrepancy inline
without creating a stub file. Emit [Transition Result
V1](../../shared/transition.md#transition-result-v1) (`status: blocked`,
`owner: Definition`, `review: none`, `reopen_owner: Intake`).

## Already-Satisfied No-Delta

Use only when primary sources prove zero functional, operational, quantitative,
or technical work remains.

1. Skip drafting `spec.md`. Build an inline **Primary No-Delta Evidence** list
   mapping every `intent.md` obligation and `Success signal` to verified primary
   locators (`file:line`/symbol, `api/`, migration, config, callers).
2. Submit to 1 fresh `reviewer-agent` running [Specification
   Review](../specification-review.md#already-satisfied-no-delta) under
   [Review](../../shared/review.md).
3. Handle the verdict (`PASS | FAIL` only):
   - **`PASS`** *(or zero surviving blockers after `adjudicator-agent` closure
     under [Review Lifecycle](../../shared/review.md#lifecycle))*: remove any
     superseded `specs/<task>/spec.md` and `specs/<task>/workflow-plan.md`
     under [Artifacts](../../shared/artifacts.md), and return [Transition Result
     V1](../../shared/transition.md#transition-result-v1) (`status: ready`,
     `owner: Definition`, `result: <evidence>`, `next_owner: none`).
   - **`FAIL`:** verify citations and handle any refuted finding under [Review
     Lifecycle](../../shared/review.md#lifecycle). On confirmed
     `reopen_owner: Specification`, enter [Specification](../specification.md)
     step 2 (fresh review on the resulting draft); on confirmed `reopen_owner:
     Intake`, follow [Wrong Premise In Intent](#wrong-premise-in-intent).

## Behavior-Preserving Change

After [Specification](../specification.md) step 1 primary-source grounding
confirms functional behavior stays unchanged while an operational, quantitative,
or internal technical goal remains, run [Specification](../specification.md)
steps 2–6: put quantitative/operational targets in `Behavior delta`, or write
`None — behavior-preserving change` when only `G<n>` and a `Downstream handoff`
(`Technical Design` or `Planning`) apply. Evaluate step 4 adversarial dry-runs
strictly over observable outcomes (`R<n>` + `G<n>`), excluding non-behavioral
goals routed to `Downstream handoff`.

## Blocked Frontier

When step 2, step 4, or a confirmed step 5 `UPSTREAM_GAP` surfaces an
undelegated requester- or policy-owned fork: finish all independent branches of
step 2 to collect the full **independent frontier**, frame each root `B<n>` in
requester behavior and consequences (worded so `"yes"` accepts the
recommendation when supported), and attach any already-known dependent child
fork under its parent `B<n>` as `Follow-up if <option>` (for Intake to hold back
from Round 1 and ask immediately only if the requester chooses that branch).
Save resolved rules and `Blocked decisions` in `spec.md` at `status: blocked`,
and return [Transition Result
V1](../../shared/transition.md#transition-result-v1) (`status: blocked`,
`owner: Definition`, `result: specs/<task>/spec.md`, `review: none`,
`reopen_owner: Intake` or external policy owner) without review.
