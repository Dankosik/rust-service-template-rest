# Specification Review

Read-only review adapter for a fixed Definition candidate (`spec.md` at `status:
draft` or an inline already-satisfied no-delta package). Return [Review Result
V1](../interfaces/review-result-v1.md) with anchored findings, classification
(`PHASE_DEFECT`, `UPSTREAM_GAP`, or `CONCERN`), and smallest owner
(`Specification`, `Intake`, or external policy owner). Report findings without
rewriting `spec.md` or inventing product decisions.

For an already-satisfied no-delta package, jump to [Already-Satisfied
No-Delta](#already-satisfied-no-delta).

## Execution Order, Scope And Primary Sources

Run only the checks assigned in your brief, respecting the cold-start boundary
(never open uncited codebase files before completing Phase A):

- **Single-reviewer:** Phase A (`1`) → Phase B (`2`, `3`) → Phase C (`4`, `5`,
  `6`).
- **Axis 1 — Truth & Scope:** Phase B (`2`, `3`) only. Independently verify
  baseline claims and blast radius across uncited primary sources—`api/`, code,
  migrations, config, `scripts/template/profiles.json`, tests, `docs/decisions/`,
  [Domain Vocabulary](../../repo-architecture.md#domain-vocabulary), and
  [Production Contract](../../production-contract.md)—never treating prose as
  evidence; record inspected primary locators in `evidence_boundary`.
- **Axis 2 — Precision & Purity:** Phase A (`1`, cold) → Phase C (`4`, `5`,
  `6`). Inspect `intent.md`, `spec.md`, and **explicitly cited links** (`api/`,
  `research/*.md`, `docs/decisions/`, cited code anchors) to verify link
  integrity, contract interactions, and mechanism leaks—without duplicating
  Axis 1's uncited codebase blast-radius scan. Record inspected artifacts and
  cited anchors in `evidence_boundary`.

Stop a broken path at its earliest defect while continuing independent paths.

**Universal witness bar:** Every `FAIL` requires a concrete witness (exact
quote/anchor + failure scenario or primary contradiction + external
consequence). Never block `PASS` on downstream technical choices, equivalent
phrasing with no external divergence, or speculative scenarios outside
`intent.md` and affected surfaces.

## Checks

### Phase A — Cold-Start *(run first, on `intent.md`, `spec.md`, and cited links only)*

1. **Cold-start & falsifier quality.** Walk `intent.md`, `spec.md`, and cited
   links with zero chat history. `FAIL` (witness: exact unanswered question or
   broken anchor + consequence) on missing product context a cold-start
   downstream consumer (`Technical Design` or `Planning`) cannot answer, a
   broken anchor, residual `Blocked decisions` in a `draft`, or any `Falsifier`
   violating [Specification V1](../interfaces/spec-v1.md#field-rules)
   (tautological, vague, wrong layer, or prescribing tests/proof gates).

### Phase B — Truth, Authority & Blast Radius

2. **Intent fidelity, authority & decision discipline.** Map every material
   `intent.md` item (`Success signal`, delegated limits, `Open questions`) to
   `spec.md` (`R<n>`, `G<n>`, `Non-goals`, or `Downstream handoff`). `FAIL`
   (witness: exact `intent.md` quote + missing/conflicting anchor + external
   consequence) on a dropped item, weakened constraint, unauthorized breaking
   change, smuggled decision in `Non-goals`/`Falsifier`, delegated choice
   exceeding limits or lacking a steelman alternative/reason, assumption lacking
   an objective `Reopen if`, or an undelegated requester- or policy-owned fork
   disguised as a `Bounded assumption` (`UPSTREAM_GAP`, `reopen_owner: Intake`
   or external policy owner).
3. **Source truth & Probe #4 (blast radius and existing state).** Independently
   inspect primary sources and callers for every touched surface against
   [Behavioral Probes #4](specification/behavioral-probes.md). `FAIL` (witness:
   primary `file:line`/symbol + contradicted/omitted claim) if a baseline claim
   contradicts code, `intent.md` rests on a verified false premise
   (`UPSTREAM_GAP`, `reopen_owner: Intake`), any affected surface, consumer,
   optional profile, persisted/in-flight state, or compatibility/authorization
   guarantee is missing from `Preserved guarantees`, or a cross-task term or
   service-wide promise conflicts with `Domain Vocabulary`/`Production Contract`
   or omits `Durable contract update`.

### Phase C — Behavioral Tightness & Mechanism Purity

Witnesses: Checks 4–5 give **starting state**, **trigger**, **observable
result(s)**, and **exact anchor**; Check 6 gives **leaked clause**, **valid
alternative satisfying `intent.md` and cited contracts without it**, and **proof
no external observer can distinguish them**.

4. **Malicious compliance & Probe #3 promise boundaries.** Construct a literal
   implementation satisfying every `R<n>` and `G<n>` together that still breaks
   an observable outcome or `Success signal` in `intent.md` (excluding
   non-behavioral goals in `Downstream handoff`). Test every relied-on response,
   status, event, or ack for *false-positive success* and *unobservable
   completion*.
5. **Two-implementations divergence (Probes #1, #2, #3, and #5).** Probe
   [Behavioral Probes](specification/behavioral-probes.md) **#1, #2, #3, and #5**
   (leaving Probe #4 to Check 3) for two implementations both satisfying
   `spec.md` yet yielding **materially different external outcomes**
   (status/code, persisted state, side effect, error recoverability,
   security/privacy exposure, compatibility, or success signal) without an
   explicit permitted invariant (`Open to Technical Design:`). Check vocabulary
   consistency, missing table cells, and [Research](research.md) quantity labels
   on numeric bounds.
6. **Mechanism leak & deletion test.** Apply the deletion test to every inferred
   clause. `FAIL` on leaked mechanisms, crate/file placement, task slicing,
   invented unfixed internal/operator identifier spellings left open by
   `intent.md` and `api/` (owned by Technical Design's [Pinned shared
   identifiers](../interfaces/design-v1.md)), or
   clauses excluding a contract-valid design without protecting an accepted
   external guarantee.

## Already-Satisfied No-Delta

Independently verify every functional, operational, quantitative, and technical
`intent.md` obligation and `Success signal` against primary sources (`api/`,
code, migrations, config, `scripts/template/profiles.json`, callers), recording
inspected locators in `evidence_boundary`. Return **`PASS | FAIL` only** (never
`CONCERNS`):

- `PASS`: primary evidence proves zero delta remains across all affected
  surfaces and callers.
- `FAIL`: `UPSTREAM_GAP` (`reopen_owner: Intake`) on a verified false premise,
  or `PHASE_DEFECT` (`reopen_owner: Specification`) if any delta (including
  behavior-preserving or quantitative work) remains.

## Verdict *(Draft Specification)*

- `PASS`: every assigned check clears with required inspection recorded in
  `evidence_boundary`.
- `CONCERNS`: strictly a bounded downstream proof or external risk over closed
  behavior—never behavioral ambiguity (`FAIL`) or ordinary Technical Design work
  (`PASS`). Return `CONCERN` with **risk, downstream owner, and closing action**.
- `FAIL`: any surviving witnessed check; return `PHASE_DEFECT` or `UPSTREAM_GAP`
  with exact anchor and smallest owner (`Specification`, `Intake`, or external
  policy owner).
