# Non-Standard Specification Paths

Load from [Specification](../specification.md) only when triggered.

## Reopen Or Resume From Blocked

Read existing `specs/<task>/spec.md` alongside `intent.md`, keeping `spec.md`
untouched through step 1 grounding so early exits do not leave a half-edited
draft. Reconcile only invalidated clauses under
[Artifacts](../../shared/artifacts.md), preserve ID stability and re-verify
prior `Carried concerns` per [Specification
V1](../../interfaces/spec-v1.md#field-rules), and drop every resolved `B<n>`
(removing `## Blocked decisions` completely before writing `status: draft` in
step 3, or updating it for newly uncovered child forks). Continue
[Specification](../specification.md) from step 1.

## Wrong Premise In Intent

Primary sources contradict a baseline premise of `intent.md`. If
`specs/<task>/spec.md` already exists (reopen or existing `draft`), set
`status: blocked`, record the verified discrepancy in `Blocked decisions`, and
return `result: specs/<task>/spec.md`; otherwise return the discrepancy inline
without creating a stub file. Emit [Transition Result
V1](../../shared/transition.md#transition-result-v1) (`status: blocked`,
`owner: Definition`, `review: <review result when triggered from review,
otherwise none>`, `reopen_owner: Intake`, `next_owner: none`).

## Already-Satisfied No-Delta

Use only when primary sources prove zero functional, operational, quantitative,
or technical work remains.

1. Skip drafting `spec.md`. Build an inline **Primary No-Delta Evidence** list
   mapping every `intent.md` obligation and `Success signal` to verified primary
   locators (`file:line`/symbol, `api/`, migration, config, callers).
2. Submit to 1 fresh `reviewer-agent` running [Specification
   Review](../specification-review.md#already-satisfied-no-delta) under [Review
   Lifecycle](../../shared/review.md#lifecycle), passing strictly `intent.md`,
   the inline Primary No-Delta Evidence, `research/*.md` when present, and the
   method path.
3. Handle the verdict:
   - **`PASS`** *(or review closure under [Review
     Lifecycle](../../shared/review.md#lifecycle))*: remove any superseded
     `specs/<task>/spec.md` and `specs/<task>/workflow-plan.md` under
     [Artifacts](../../shared/artifacts.md), and return [Transition Result
     V1](../../shared/transition.md#transition-result-v1) (`status: ready`,
     `owner: Definition`, `result: <evidence>`, `review: <Review Result V1
     locator or inline result>`, `next_owner: none`).
   - **`FAIL`:** verify citations and handle any refuted finding under [Review
     Lifecycle](../../shared/review.md#lifecycle). On confirmed
     `reopen_owner: Specification`, enter [Specification](../specification.md)
     step 2 (fresh review on the resulting draft); on confirmed `reopen_owner:
     Intake`, follow [Wrong Premise In Intent](#wrong-premise-in-intent).

## Behavior-Preserving Change

After [Specification](../specification.md) step 1 confirms this mode, run steps
2–6: record any quantitative/operational targets in `Behavior delta` (otherwise
use [Specification V1](../../interfaces/spec-v1.md#field-rules)'s
behavior-preserving marker alongside `G<n>` and `Downstream handoff`). Evaluate
step 4 adversarial dry-runs strictly over observable outcomes (`R<n>` + `G<n>`),
excluding non-behavioral goals routed to `Downstream handoff`.

## Blocked Frontier

When step 2, step 4, or a confirmed step 5 `UPSTREAM_GAP` surfaces an
undelegated requester- or policy-owned fork or an unresolved behavioral
authority conflict: finish all independent branches of step 2 to collect the
full **independent frontier**, record the root frontier and any known dependent
child forks in `Blocked decisions` per [Specification
V1](../../interfaces/spec-v1.md), save resolved rules and `Blocked decisions` in
`spec.md` at `status: blocked`, and return [Transition Result
V1](../../shared/transition.md#transition-result-v1) (`status: blocked`,
`owner: Definition`, `result: specs/<task>/spec.md`, `review: <review result
when triggered from step 5, otherwise none>`, `reopen_owner: Intake` or
external policy/evidence owner, `next_owner: none`) without another review.
