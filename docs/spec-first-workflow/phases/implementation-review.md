# Implementation Review

Use when shared [Review](../shared/review.md) routes a fixed implementation
candidate (single-unit final delivery, assembled ledger candidate, or explicit
standalone review). This adapter owns implementation falsifiers and verdicts.
Stay read-only and perform the review directly without repairing, re-scoping,
or accepting.

Pin the candidate diff (`<Base>..HEAD` or supplied fixed identity), confirm it
is non-empty, and **read touched production code before its tests**. Consume
accepted sources (`intent.md`, `spec.md`, `design/`, `rollout.md`, `tasks.md`,
packets), current proof receipts, and irreproducible external evidence. Reuse
valid results; run an existing bounded falsifier only for a concrete diagnostic
question within [Evidence
Contract](../shared/evidence-contract.md#required-and-optional-proof). When
long or external required checks are still running on the fixed candidate,
inspect code and traces now, report discovered defects promptly, and wait for
those results before returning `PASS`.

## Adversarial Falsifiers

Evaluate **all four axes** and report surviving findings grouped by axis so a
clean pass on one axis never masks a defect on another. Continue across
independent paths so one review pass surfaces all material defects.

1. **Outcome, Spec & Scope Parity.** Start from the accepted sources and
   `Completion`. Construct a concrete caller or operator scenario where every
   executed test stays green yet an accepted obligation, preserved guarantee,
   withdrawn removal path, or `Completion` criterion fails. Trace every
   material diff hunk back to an accepted obligation; flag unrequested behavior
   changes or silent scope drift.
2. **Real-Path Defects & Cross-Unit Seams.** Attack touched production code on
   concrete boundary, rejection, error-precedence, partial-write,
   cancellation/drop, concurrency, authorization, money, data-integrity, and
   migration-reversibility paths. On multi-task candidates, inspect every
   `Provides -> Depends on` seam, assembly point, and recorded task `decisions`
   entry for cross-unit mismatches (`INTEGRATION_DEFECT`) and deferred
   unit-local invariants.
3. **Structure & Single Ownership.** Apply `rust-structural-quality`'s deletion
   test and [Engineering](../../../AGENTS.md#engineering). Flag code that
   reimplements a mechanism already provided with matching boundary semantics
   by `std`, a declared crate, or an authoritative tool; splits one fact across
   carriers callers must keep aligned by hand; or adds an unjustified crate,
   dependency, layer, trait, config key, or retained fallback/shim. Name the
   violated constraint and exact item to delete, collapse, bind, or delegate.
4. **Test & Proof Falsifiability.** On candidate-added or edited tests and
   claimed proof, test whether each guard would actually go **red** when the
   claimed behavior breaks. Flag **tautological** assertions (recomputing
   expected values the way the code does), tests that mock away the boundary
   under claim or mirror internal steps rather than observable contracts,
   retries or wall-clock sleeps masking nondeterminism, copy-pasted suites
   duplicating an already-covered path at the same layer, and deleted or
   weakened independent safety, durability, or sole regression guards.

## Blocking Bar And Verdict

Return [Review Result V1](../interfaces/review-result-v1.md) (`PASS | FAIL |
NEEDS_PARENT`, or `REVIEW_HANDOFF_INVALID` when the fixed delivery boundary is
missing):

| Disposition | Bar |
| --- | --- |
| Blocking (`TASK_DEFECT`, `INTEGRATION_DEFECT`, `UPSTREAM_GAP`) | Cites (1) the exact accepted claim or constraint falsified, (2) candidate anchor (`path::Symbol` / hunk), (3) a reproducible trace, counterexample, or mechanical check, and (4) the smallest repair or reopen owner. An actual in-scope defect found by an optional check blocks. |
| Non-blocking (`FOLLOW_UP`, never blocks `PASS`) | Style preferences, equivalent alternative organizations preserving single ownership, naming polish, speculative future risks, untouched historical test debt, and missing optional integration, provider, or container evidence. |

## Delta Recheck

When Review selects a bounded delta recheck, falsify only the anchored
findings, their invalidated proof, and any structure the repair added against
[Engineering](../../../AGENTS.md#engineering). Preserve unaffected closed
reasoning.

## Structure-Only Review

Use when shared Review triggers review only for added structure. Run **Falsifier
3** exclusively on each addition the trigger names; behavior and proof stay
outside this boundary. Classify violations of
[Engineering](../../../AGENTS.md#engineering) as `TASK_DEFECT` naming the item
to delete, collapse, bind to its source owner, or replace with `std` or a
maintained crate; classify other observations as `FOLLOW_UP`.

## Integrated Candidate

Multi-task final delivery runs all four Adversarial Falsifiers across the
complete assembled candidate. Implemented units have no prior review verdict to
reuse; reuse only still-current proof and do not reopen unaffected closed
findings. Route each surviving finding to the smallest affected task owner — or
both seam task owners for `INTEGRATION_DEFECT` — keeping repair and invalidated
checks inside Final Validation without recreating per-task acceptance or review
cycles. Reopen Planning only when repair changes accepted behavior or
ledger-wide scope rather than a local seam between implemented units.
