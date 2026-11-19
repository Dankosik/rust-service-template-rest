# Specification Review

Use when [Review](../shared/review.md) routes a fixed Definition result
(`specs/<task>/spec.md` at `status: draft` or already-satisfied no-delta
evidence) or on an explicit standalone specification review. Stay read-only:
return anchored findings with their smallest owner in [Review Result
V1](../interfaces/review-result-v1.md) without rewriting `spec.md` or inventing
product decisions.

Treat `spec.md` baseline prose and `intent.md` current-state claims as claims
to verify against primary sources—committed contracts (`api/`), code,
migrations, config, tests, `docs/decisions/`, and [Domain
Vocabulary](../../repo-architecture.md#domain-vocabulary)—never as evidence.

## Review Topology

- **Already-satisfied no-delta result (always 1 fresh `reviewer-agent`):**
  independently verify every functional and non-functional obligation and
  `Success signal` in `intent.md` against primary code, committed contracts
  (`api/`), migrations, config, and callers, recording inspected primary
  locators in `evidence_boundary`, then jump to [Verdict](#verdict).
- **Draft `spec.md` — default (1 fresh `reviewer-agent`):** run Tier 1 then
  Tier 2 across all six falsifiers for local or single-surface specs, recording
  all inspected primary files, symbols, schemas, migrations, and caller
  searches in `evidence_boundary`.
- **Draft `spec.md` — two-axis parallel review (2 fresh `reviewer-agent` lanes,
  never forking parent history):** use when the spec spans multiple interacting
  surfaces, changes a breaking/external compatibility boundary, or governs
  authorization, money, data integrity, or concurrent state transitions:
  - **Axis 1 — Authority, Source Truth, Blast Radius & Cold-Start:** Falsifiers
    **1, 2 (including Seam 4), and 6**; records all inspected codebase, schema,
    migration, and caller primary sources in `evidence_boundary`.
  - **Axis 2 — Adversarial Behavior, Seams 1–3 & 5, and Mechanism Leaks:**
    Falsifiers **3, 4 (Seams 1–3 and 5 only), and 5**; records the `intent.md`,
    `spec.md`, and cited contract/decision primary anchors checked in
    `evidence_boundary` without duplicating Axis 1's codebase blast-radius
    scan.

Keep both axis reports distinct without masking findings; review passes only
when both axes clear on the same candidate. After repair, re-run only the
failed or invalidated axis under [Review](../shared/review.md#lifecycle),
retaining an unaffected axis's `PASS`; use `adjudicator-agent` under
[Review](../shared/review.md#route) only for one surviving evidence-backed
inter-axis conflict.

## Adversarial Falsifiers

Run every falsifier assigned to your axis (Tier 1 before Tier 2 in
single-reviewer mode). Stop a broken path at its earliest defect while
continuing independent paths so one pass surfaces all material findings.

### Tier 1 — Authority, Source Truth & Blast Radius

1. **Intent fidelity, authority & assumption discipline.** Map every material
   item in `intent.md` (`Success signal`, delegated limits, `Open questions`)
   to `spec.md` (`R<n>`, `G<n>`, `Non-goals`, or `Downstream handoff`). `FAIL`
   on a dropped item, weakened constraint, unauthorized breaking change,
   delegated choice exceeding limits or lacking its rejected alternative and
   reason, smuggled decision in `Non-goals`/`Falsifier`, assumption lacking an
   objective `Reopen if`, or an undelegated requester- or policy-owned fork
   disguised as a `Bounded assumption` (`reopen_owner: Intake` or external
   policy owner).
2. **Source truth & Seam 4 (blast radius and existing state).** Independently
   inspect primary sources and callers for every touched surface against
   [Behavioral Seams #4](specification.md#behavioral-seams). `FAIL` if a
   baseline claim contradicts code, `intent.md` rests on a false premise
   (`reopen_owner: Intake`), any affected surface, consumer, persisted/in-flight
   state, or compatibility/authorization requirement under Seam 4 is missing
   from `Preserved guarantees`, or a cross-task term / service-wide promise
   conflicts with [Domain
   Vocabulary](../../repo-architecture.md#domain-vocabulary) / [Production
   Contract](../../production-contract.md) or omits its `Durable contract
   update`.

### Tier 2 — Behavioral Tightness, Mechanism Leaks & Cold-Start

Witnesses required: for falsifiers 3–4, give **starting state**, **trigger**,
**observable result** (or two diverging results), and **exact anchor**; for
falsifier 5, give the **leaked clause**, a **valid alternative satisfying
`intent.md` and cited contracts without it**, and **proof no external observer
can distinguish them**.

3. **Malicious compliance & Seam 3 promise boundaries.** Construct a literal
   implementation satisfying every `R<n>` and `G<n>` (evaluating interacting
   rules together) that still misses an observable outcome or `Success signal`
   in `intent.md` (excluding non-behavioral technical goals routed to
   `Downstream handoff`). Test every relied-on signal, status, event, or ack
   for Seam 3's *false-positive success* and *unobservable completion*.
4. **Two-implementations divergence (Seams 1, 2, 3, and 5).** Probe [Behavioral
   Seams](specification.md#behavioral-seams) **#1, #2, #3, and #5** (leaving
   Seam 4 to Falsifier 2) for a concrete scenario where two implementations
   both satisfy `spec.md` yet yield **materially different external outcomes**
   (status/code, persisted state, side effect, error recoverability,
   security/privacy exposure, compatibility, or success signal) without a
   permitted invariant. Check vocabulary consistency and source labels on
   numeric bounds.
5. **Mechanism leak & deletion test.** Apply the deletion test to every
   inferred clause. `FAIL` on leaked mechanisms, crate/file placement, task
   slicing, or clauses excluding a contract-valid design without protecting an
   accepted external guarantee.
6. **Cold-start & falsifier boundary.** Walk `intent.md`, `spec.md`, and cited
   links with zero chat history. `FAIL` on missing product context needed to
   understand rules, a broken anchor, residual `Blocked decisions` in a
   `draft`, or any `Falsifier` violating [Specification
   V1](../interfaces/spec-v1.md#field-rules) (vague, wrong layer, or
   prescribing test implementation / unmandated proof gates).

## Verdict

- **Draft specification:**
  - `PASS`: every assigned falsifier clears with the axis's required inspection
    recorded in `evidence_boundary`. Never block `PASS` on downstream choices
    (Technical Design, Planning, Implementation), interchangeable phrasing or
    formatting where no material external outcome diverges, or speculative
    scenarios outside `intent.md` and affected surfaces.
  - `CONCERNS`: strictly a bounded downstream proof or external risk over
    closed behavior (such as an external contract clause needing a Technical
    Design probe, or an optional live verification gap)—never behavioral
    ambiguity (`FAIL`) or ordinary Technical Design work (`PASS`). Name
    **risk, downstream owner, and closing action** so Definition persists it in
    `Carried concerns` before setting `status: ready`.
  - `FAIL`: any surviving falsifier; anchor each finding to `R<n>`, `G<n>`,
    `intent.md`, or primary source and name the smallest owner (`Specification`,
    `Intake`, or external policy owner).
- **Already-satisfied no-delta result:** `PASS` requires independently verified
  primary evidence in `evidence_boundary` proving every functional and
  non-functional `intent.md` obligation already holds across affected surfaces
  and callers; `FAIL` with `reopen_owner: Intake` on a false premise, or
  `Specification` if any delta (including behavior-preserving or quantitative
  work) remains.
