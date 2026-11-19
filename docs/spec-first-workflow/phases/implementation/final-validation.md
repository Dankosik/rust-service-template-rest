# Final Validation

Use as Delivery Owner once every planned unit is `Implemented`, the candidate
is assembled, and all writers have joined. One Delivery Owner proves the
assembled candidate against the accepted outcome and reports one [Completion
Result](../../interfaces/acceptance-result-v1.md#completion-result).

## Obligations

Read the ledger's Completion and Global constraints, every packet's Final
validation section, and each task's recorded `decisions` and `unverified`
entries. A root-local Lead reads the same fields from its inline unit.

Start from the accepted outcome. Trace each obligation for this candidate to
its observable result on the affected paths, including behavior those paths
must preserve. Then trace each material behavior change back to the accepted
obligation it serves. For a removal, check that the old behavior is unreachable
on the paths from which it was withdrawn. For a requirement rename alone, check
that its promised behavior still holds. Close each `unverified` entry with a
check, or carry it as a named gap.

## Checks

Select the checks for the changed surface from the [Validation
budget](../../../../AGENTS.md#validation-budget) and reconcile any task
additions with their authoritative source. Match each claim to current
evidence at the required boundary. One result can cover several claims. Consult
[Validation Routing](../../../validation-routing.md) for mixed or specialized
surfaces and the [Evidence Contract](../../shared/evidence-contract.md) for
reuse, claim scope, or unavailable infrastructure. Run fast checks first.
Include variants, generated clients, or intermediate versions when the selected
checks or accepted scope require them.

## Review

Apply the [review triggers](../../shared/review.md#trigger) to the assembled
candidate to select full review, structure-only review, or direct completion.
The Delivery Owner validates behavior on every route. Keep the candidate fixed
during validation and any selected review.

## Repair

Classify each failure as product, test/oracle, or environment.

- **Product or test/oracle**: Repair required-check failures and in-scope
  product or test failures at the root they share. In a ledger, send the
  Orchestrator a repair request naming the falsified claim, its evidence, and
  the owning task, or both seam tasks for an integration defect; the owning
  Lead repairs. A root-local Lead repairs directly. Join or stop readers and
  release validation locks before edits begin.
- **Environment**: Record the material gap when an optional check's
  environment is unavailable. The [Evidence
  Contract](../../shared/evidence-contract.md#required-and-optional-proof)
  governs a required check that cannot run.

Once a repair is integrated, run the failed, invalidated, and outstanding
required checks. Apply [Parent-Owned
Recovery](../../shared/transition.md#parent-owned-recovery) when repeated
attempts add no discriminating evidence.

## Done

Report the Completion Result for the requested scope once all of these hold,
carrying forward the local validation evidence:

- Every `Observable claim` in every packet and the ledger's Completion has a
  current result at its required boundary, or a named gap.
- Every material behavior change in the candidate serves an accepted
  obligation.
- Blocking findings are closed and any selected review has returned.

## Conditional Branches

- **Runtime or fixture scenarios**: When explicitly required, run the smallest
  end-to-end scenario covering setup, behavior, observation, and bounded cleanup
  first, reusing valid prepared environments.
- **Isolated or remote execution**: Resolve candidate or image digests,
  checksums, and host paths from task state or its manifest before consuming
  results, updating replacement locators and invalidating only affected proof
  after repair.
- **Long checks and resource waits**: Record an observation checkpoint in
  existing execution state and inspect logs, processes, or lock owners via
  background execution so concrete delay causes are visible in place.
