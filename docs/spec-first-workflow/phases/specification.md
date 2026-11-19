# Specification

Definition owns what users, callers, operators and downstream systems can
observe. [Phase Selection](../../spec-first-workflow.md#phase-selection)
determines whether a behavioral contract needs authoring or reopening.

## Steps

1. **Ground the request.** Require a ready `specs/<task>/intent.md`. If it is
   missing or unready, create no spec, leave any existing spec untouched and return a
   [Blocked Handoff](specification/non-standard-paths.md#blocked-handoff)
   naming the missing input and reopening Intake. On an existing
   spec, follow [Reopen Or Resume From Blocked](specification/non-standard-paths.md#reopen-or-resume-from-blocked).
   Read active coordination and prior research when present. Check Intent's
   baseline against affected code, callers, schemas, migrations, config,
   profiles and tests. Consult [Domain Vocabulary](../../repo-architecture.md#domain-vocabulary),
   decision records and the [Production Contract](../../production-contract.md)
   for applicable policy. Ground statically by default. Delegate wide
   multi-crate inventories under [Read-Only Delegation](../shared/read-only-delegation.md)
   and verify decision-critical anchors directly. Use supporting
   [Research](research.md) when evidence or feasibility could change behavior. Leave solution discovery
   to Technical Design unless feasibility changes an observable obligation.

   A contradicted baseline premise or infeasible hard binding takes [Wrong Premise In Intent](specification/non-standard-paths.md#wrong-premise-in-intent).
   If all obligations are already met, take [Already-Satisfied No-Delta](specification/non-standard-paths.md#already-satisfied-no-delta).
   If functional behavior stays fixed but work remains, use
   [Behavior-Preserving Change](specification/non-standard-paths.md#behavior-preserving-change).

2. **Settle behavioral forks.** Use the canonical vocabulary and distinguish
   new or overloaded terms. Walk [Behavioral Probes](specification/behavioral-probes.md).
   For an unresolved external-observer question, load the matching
   `rust-api-contract`, `rust-security`, `rust-config`, `rust-reliability`,
   `rust-observability` or `rust-performance` skill.
   Resolve every material [Intent obligation](../interfaces/intent.md#field-rules),
   including the observable goal behind a suggested mechanism. Keep research
   unknowns and freshness conditions in `Reopen if`. Investigate conflicting
   evidence under [Conflict Or Freshness](research-branches.md#conflict-or-freshness).

   | Source of a decision | Disposition |
   | --- | --- |
   | Applicable contract, code, standard or accepted decision | Cite its primary locator. Revisit a rejected decision only on its reopen condition or an explicit Intent mandate. Flag changes to shared vocabulary, service promises or decision records for a durable contract update. |
   | A range of outcomes is acceptable | Pin the invariant and leave the permitted variation to Technical Design under [Field Rules](../interfaces/spec-v1.md#field-rules). |
   | Delegated behavioral choice | Decide within the delegated limits and record the decision. |
   | Strong repository convention or applicable standard supports a reversible assumption | Record the assumption only if it preserves requester meaning, caller commitments, persisted-data fate and trust boundaries. |
   | Internal goal or non-behavioral preference | Name its downstream owner in the handoff. |
   | Required behavioral authority is unresolved | Take the [Blocked Frontier](specification/non-standard-paths.md#blocked-frontier). |

3. **Render the draft.** Write the closed decisions to `specs/<task>/spec.md`
   under [Specification V1](../interfaces/spec-v1.md).

4. **Preflight the on-disk draft.** Trace each material Intent obligation and
   research disposition to a clause. Dry-run the hardest one or two interacting
   or unhappy paths against the rules, guarantees and table cells. Repair any
   path that satisfies the text yet defeats the success signal or allows
   materially different outcomes outside a stated invariant. Check each
   falsifier and inferred restriction against [Field Rules](../interfaces/spec-v1.md#field-rules).
   Route newly exposed forks through step 2.

5. **Review the fixed draft.** Dispatch the fresh reviewer(s) selected by
   [Specification Review](specification-review.md) with the inputs and checks
   assigned there. Use [Review Lifecycle](../shared/review.md#lifecycle) for
   verification, rechecks and adjudication. If any upstream gap is confirmed,
   take [Upstream Gap After Review](specification/non-standard-paths.md#upstream-gap-after-review)
   before editing. Otherwise repair confirmed local defects
   and repeat the affected preflight. A grounding repair that changes rules,
   guarantees or scope can invalidate the other axis, as can a contract repair
   that changes behavior, baseline or affected surfaces. Review every failed
   or invalidated axis afresh unless the bounded delta criteria hold on that
   axis and the repair leaves the other axis valid.

6. **Hand off.** When review permits movement, finalize the spec under
   [Field Rules](../interfaces/spec-v1.md#field-rules) and return its reviewed
   [Transition Result V1](../shared/transition.md#transition-result-v1)
   (`owner: Definition`, `result: specs/<task>/spec.md`). Select the next
   owner under [Phase Selection](../../spec-first-workflow.md#phase-selection).
