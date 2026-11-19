# Non-Standard Specification Paths

Load from [Specification](../specification.md) only when triggered.

## Reopen Or Resume From Blocked

Keep the existing spec untouched while grounding. Reconcile invalidated
clauses under [Artifacts](../../shared/artifacts.md) and apply [Field
Rules](../../interfaces/spec-v1.md#field-rules) for IDs and prior concerns.
Remove resolved blockers. If a dependent fork is now open, take the
[Blocked Frontier](#blocked-frontier). Once the frontier clears, resume drafting.

## Upstream Gap After Review

Preserve the review result on the blocked handoff. For a missing or unready
Intent, apply the input rule in [Specification step 1](../specification.md#steps)
and return the gap inline before any repair. With a ready Intent, repair
independent local defects, then use [Wrong Premise In Intent](#wrong-premise-in-intent)
for a verified contradiction or [Blocked Frontier](#blocked-frontier) for a
missing decision or external authority.

## Wrong Premise In Intent

When primary evidence contradicts an Intent baseline or conflicts with a hard
binding, reopen Intake with the discrepancy. Mark an existing spec blocked
and record the evidence in `Blocked decisions`. With no spec, return the
discrepancy inline. Follow [Blocked Handoff](#blocked-handoff).

## Already-Satisfied No-Delta

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
     Lifecycle](../../shared/review.md#lifecycle))*: retire any superseded
     `specs/<task>/spec.md` and close coordination under
     [Artifacts](../../shared/artifacts.md), then return [Transition Result
     V1](../../shared/transition.md#transition-result-v1) (`status: ready`,
     `owner: Definition`, `result: <evidence>`, `review: <Review Result V1
     locator or inline result>`, `next_owner: none`).
   - **`FAIL`:** verify citations and handle any refuted finding under [Review
     Lifecycle](../../shared/review.md#lifecycle). On confirmed
     `reopen_owner: Specification`, enter [Specification](../specification.md)
     to ground any missing evidence before resubmitting. If work remains,
     enter step 2 and review the resulting draft. For a confirmed upstream gap,
     follow [Upstream Gap After Review](#upstream-gap-after-review).

## Behavior-Preserving Change

Keep concrete unchanged guarantees in `G<n>`. Record operational or quantitative
changes as rules. For internal work alone, use the [behavior-preserving
marker](../../interfaces/spec-v1.md#field-rules) and hand off the technical goal.
Continue the normal authoring and review path over the observable contract.

## Blocked Frontier

For unresolved behavioral decisions, finish independent branches. Save closed
rules and the independent root blockers in a blocked spec under [Specification
V1](../../interfaces/spec-v1.md). Include dependent child forks with their
triggering option. Reopen Intake for requester meaning, or name the external
policy or evidence owner for its missing input. Return a [Blocked Handoff](#blocked-handoff).

## Blocked Handoff

Return [Transition Result V1](../../shared/transition.md#transition-result-v1)
with `status: blocked`, `owner: Definition`, the blocked spec path or inline
input gap, the review result if reviewed, the smallest reopen owner and
`next_owner: none`. Resume once the named input arrives.
