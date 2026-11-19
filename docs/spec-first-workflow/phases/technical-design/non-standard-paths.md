# Non-Standard Technical Design Paths

Load from [Technical Design](../technical-design.md) only when triggered.

## Reopen Or Resume From Blocked Or Ready

Inspect only what changed across `intent.md`, `spec.md`, existing
`specs/<task>/design/design.md`, and `specs/<task>/rollout.md` under
[Artifacts](../../shared/artifacts.md). Preserve surviving `D<n>`/`S<n>` IDs
and keep any retained `rollout.md` status in lockstep (`draft` before review)
per [Design V1](../../interfaces/design-v1.md#field-rules), re-verify prior
`Carried concerns`, and mark superseded siblings for deletion in step 3 (or
[No-Open-Decisions Exit](#no-open-decisions-exit)). Continue [Technical
Design](../technical-design.md) from step 1.

## Behavior-Preserving Change Or Skipped Definition

- **Behavior-preserving change:** when `spec.md` has `Behavior delta: None —
  behavior-preserving change`, run steps 1–6 over `Preserved guarantees`
  (`G<n>`) and `Technical Design` `Downstream handoff` items (deepening seams,
  ownership, or cleanup still carries real open forks).
- **Skipped Definition (fixed external contract):** when [Phase
  Selection](../../../spec-first-workflow.md#phase-selection) skipped Definition
  because a fixed external contract left zero behavioral forks, verify
  `intent.md` is `status: ready` and use `intent.md` and that cited contract as
  the obligation baseline.

## No-Open-Decisions Exit

Triggered from [Technical Design](../technical-design.md#steps) step 1 when zero
open forks, zero unpinned `Open to Technical Design:` identifiers, and no
non-trivial `rollout.md` apply:

1. Create no new `design.md`. Submit inline **forced-owner evidence** to 1
   fresh `reviewer-agent` running [Technical Design
   Review](../technical-design-review.md#no-open-decisions-result) under
   [Review](../../shared/review.md): `Obligation -> forced owner -> primary
   locator -> proving surface` table plus `Verified anchors` (for inspected
   non-obvious locked crates), `Assumptions and reopen triggers` (for genuinely
   uncached crates), `Shared mutated surfaces`, `Superseded paths and generated
   contracts`, `Durable doc updates`, `Planning hooks`, and unresolved `spec.md`
   `Carried concerns` when applicable.
2. On `PASS` or `CONCERNS`: delete stale `specs/<task>/design/*.md` or
   `rollout.md`, emit [Transition Result
   V1](../../shared/transition.md#transition-result-v1) (`status: ready`,
   `owner: Technical Design`, `result: <reviewed forced-owner evidence +
   carried concerns>`, `review: <locator or inline result>`, `movement_evidence:
   <zero open forks verified>`, `reopen_owner: none`, `next_owner: Planning`),
   and stop.
3. On `FAIL`: handle findings under [Review
   Lifecycle](../../shared/review.md#lifecycle); use a bounded delta recheck
   when only handoff metadata was incomplete; on confirmed `PHASE_DEFECT`
   exposing an open fork (`reopen_owner: Technical Design`), enter steps 2–6
   with a **fresh** review on the drafted design; on confirmed `UPSTREAM_GAP`,
   take [Blocked Exit](#blocked-exit).

## Blocked Exit

Use on non-`ready` `intent.md` (`reopen_owner: Intake`) or non-`ready`
`spec.md` (outside a skipped-Definition contract), missing
or contradictory product behavior, tripped `spec.md` `Reopen if`, falsified
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
