# Non-Standard Technical Design Paths

Load from [Technical Design](../technical-design.md) only when triggered.

## Reopen Or Resume From Blocked Or Ready

Inspect changes to the accepted baseline, design, rollout and their primary
anchors under [Artifacts](../../shared/artifacts.md). Recheck decisions and
concerns affected by those changes, apply [Design V1 lifecycle and ID
stability](../../interfaces/design-v1.md#field-rules), and mark superseded siblings for deletion in step 3 (or
[No-Open-Decisions Exit](#no-open-decisions-exit)). Continue [Technical
Design](../technical-design.md) from step 1.

## Behavior-Preserving Change Or Skipped Definition

- **Behavior-preserving change:** when `spec.md` has `Behavior delta: None —
  behavior-preserving change`, apply the normal decision-frontier routing to `Preserved guarantees`
  (`G<n>`) and `Technical Design` `Downstream handoff` items (deepening seams,
  ownership, or cleanup still carries real open forks).
- **Skipped Definition (fixed external contract):** when [Phase
  Selection](../../../spec-first-workflow.md#phase-selection) skipped Definition
  because a fixed external contract left zero behavioral forks, verify
  `intent.md` is `status: ready` and use `intent.md` and that cited contract as
  the obligation baseline.

## No-Open-Decisions Exit

Enter only from the forced-obligations route in [Technical Design](../technical-design.md#steps).

1. Create no new `design.md`. Submit inline **forced-owner evidence** for [Technical Design
   Review](../technical-design-review.md#no-open-decisions-result) under
   [Review](../../shared/review.md): `Obligation -> forced owner -> primary
   locator -> proving surface` table plus applicable [Design V1](../../interfaces/design-v1.md)
   grounding and planning handoff fields, bounded assumptions and reopen
   triggers, and unresolved upstream concerns.
2. On `PASS` or `CONCERNS`: delete stale `specs/<task>/design/*.md` or
   `rollout.md`, emit [Transition Result
   V1](../../shared/transition.md#transition-result-v1) (`status: ready`,
   `owner: Technical Design`, `result: <reviewed forced-owner evidence +
   upstream and review concerns>`, `review: <locator or inline result>`, `movement_evidence:
   <zero open forks verified>`, `reopen_owner: none`, `next_owner: Planning`),
   and stop.
3. On `FAIL`: handle findings under [Review
   Lifecycle](../../shared/review.md#lifecycle); use a bounded delta recheck
   when only handoff metadata was incomplete; on confirmed `PHASE_DEFECT`
   exposing an open fork or a required non-trivial rollout, enter steps 2–6
   with a **fresh** review on the drafted design; on confirmed `UPSTREAM_GAP`,
   take [Blocked Exit](#blocked-exit).

## Blocked Exit

Use on a missing or unready required baseline input (Intake for intent,
Specification for the behavioral specification, external authority for its
contract), missing or contradictory product behavior, tripped `spec.md` `Reopen if`, falsified
baseline/requirement, missing external authority, or a surviving user-owned
trade-off. Collect all independent blockers in one pass—formatting `B<n>` per
[Design V1](../../interfaces/design-v1.md)—then emit [Transition Result
V1](../../shared/transition.md#transition-result-v1) (`status: blocked`,
`owner: Technical Design`, `review: <locator or inline result, or none when
unreviewed>`, `movement_evidence: none`, `reopen_owner: Specification | Intake
| <external authority>`, `next_owner: none`):

- **Existing or worthwhile `design/design.md`:** save `design/design.md` at
  `status: blocked` with `Blocked decisions`, flip any persisted `rollout.md`
  to `status: blocked`, and set `result: specs/<task>/design/design.md`.
- **No partial work to preserve:** flip any persisted `rollout.md` to `status:
  blocked` and carry collected blockers inline in `result:`.
