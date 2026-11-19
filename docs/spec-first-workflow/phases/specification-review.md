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

Run only the checks assigned in your brief. Complete Phase A strictly from
`intent.md`, `spec.md`, and explicitly cited links before opening any uncited
codebase file:

- **Single-reviewer:** Phase A (`1`, cold) → Phase B (`2`, `3`) → Phase C (`4`,
  `5`, `6`).
- **Axis 1 — Grounding, Fidelity & Codebase Reality (`2`, `3`):** inspect
  `intent.md`, `spec.md`, `research/*.md`, and uncited primary sources (`api/`,
  code, migrations, config, `scripts/template/profiles.json`, tests,
  `docs/decisions/`, [Domain
  Vocabulary](../../repo-architecture.md#domain-vocabulary), [Production
  Contract](../../production-contract.md))—never treating prose as evidence.
  Own all uncited-codebase truth across Probes #1–#6; record inspected locators
  in `evidence_boundary`.
- **Axis 2 — Cold-Start, Contract Tightness & Purity (`1` cold → `4`, `5`,
  `6`):** complete Phase A strictly from `intent.md`, `spec.md`, and cited
  links, then run Phase C over those same sources—opening a named repo seam in
  Check 6 *only* when needed to verify whether a candidate identifier is already
  fixed in `api/`/code vs invented. Record inspected artifacts and anchors in
  `evidence_boundary`.

## Proof Of Attack And Witness Bar

Report under **explicit per-check headings (`Check 1`..`Check 6`, assigned
checks only)**:

- **Clearing check:** write `none — attacked <strongest loophole, divergence, or
  primary anchor tested>; closed by <exact spec.md clause/table or primary
  locator>` so passing checks prove adversarial execution rather than passive
  skimming.
- **Failing check:** supply a concrete witness (`exact quote/anchor`, `failure
  scenario or primary contradiction`, `external consequence`, `classification`,
  `smallest owner`). Stop a broken path at its earliest defect while continuing
  independent paths.

## Checks

### Phase A — Cold-Start *(complete before opening uncited codebase files)*

1. **Cold-start & falsifier quality.** Walk `intent.md`, `spec.md`, and cited
   links with zero chat history. `FAIL` on:
   - missing product context a cold-start consumer (`Technical Design` or
     `Planning`) cannot answer without chat archaeology;
   - broken anchor or residual `Blocked decisions` in a `draft`;
   - any `Falsifier` violating [Specification
     V1](../interfaces/spec-v1.md#field-rules) (tautological, vague, wrong
     layer, or prescribing tests/proof gates).

### Phase B — Grounding, Fidelity & Codebase Reality

2. **Intent fidelity, authority & decision discipline.** Independently map
   material `intent.md` items (`Success signal`, delegated limits, `Open
   questions`) and `research/*.md` unknowns/conflicts/refresh triggers to
   `spec.md`. `FAIL` on:
   - dropped item, weakened constraint, unauthorized breaking change, or
     decision smuggled into `Non-goals`/`Falsifier`;
   - `Delegated decision` exceeding limits or lacking a steelman alternative,
     reason, or objective `Reopen if`;
   - `Bounded assumption` lacking an objective `Reopen if` or disguising an
     undelegated one-way fork (`UPSTREAM_GAP`, `reopen_owner: Intake` or
     external policy owner);
   - unresolved `research/*.md` behavioral authority conflict not blocked or
     returned to its evidence owner ([Conflict Or
     Freshness](research-branches.md#conflict-or-freshness)), or bounded
     unknown / `valid as of` refresh trigger dropped or treated as settled fact.
3. **Primary-source truth, codebase interactions & Probe #5 (blast radius /
   existing state).** Independently inspect primary sources and callers across
   the blast radius against [Behavioral
   Probes](specification/behavioral-probes.md). `FAIL` on:
   - baseline claim contradicting primary code, or verified false premise in
     `intent.md` (`UPSTREAM_GAP`, `reopen_owner: Intake`);
   - uncited codebase interaction missed by `spec.md` across Probes #1–#6
     (caller, consumer, optional profile, persisted/in-flight state,
     mixed-version overlap, middleware precedence, race, replay/promise trap,
     authz/trust boundary, or startup/degrade rule), or generic filler in
     `Preserved guarantees`;
   - reversing/adopting a rejected `docs/decisions/` choice without meeting its
     reopen condition or explicit user request, or omitting a required `Durable
     contract update` (`Domain Vocabulary`, `Production Contract`, or
     `docs/decisions/`).

### Phase C — Behavioral Tightness & Mechanism Purity

Witnesses: Checks 4–5 give **starting state**, **trigger**, **observable
result(s)**, and **exact anchor**; Check 6 gives **leaked clause**, **valid
alternative satisfying `intent.md` and cited contracts without it**, and **proof
no external observer can distinguish them**.

4. **Malicious compliance & Probe #3 promise boundaries.** Construct a literal
   implementation satisfying every `R<n>` and `G<n>` together that still breaks
   an observable outcome or `Success signal` in `intent.md` (excluding
   non-behavioral goals in `Downstream handoff`). Attack every relied-on
   response, status, event, or ack for *false-positive success* and
   *unobservable completion*.
5. **Two-implementations divergence (Probes #1, #2, #3, #4, and #6).** Probe
   [Behavioral Probes](specification/behavioral-probes.md) **#1, #2, #3, #4, and
   #6**—plus any **stated** `spec.md` existing-state, compatibility, or
   breaking-change rules (`Former-caller outcome:`, `Mixed-version overlap:`),
   leaving uncited codebase blast-radius scanning to Check 3—for two
   implementations both satisfying `spec.md` yet yielding **materially different
   external outcomes** (status/code, persisted state, side effect, error
   recoverability, authz/privacy exposure, compatibility, or success signal)
   without an explicit `Open to Technical Design:` invariant. Check vocabulary
   consistency (`Terminology` + `not <confused sense>`), missing table cells or
   unanchored table rules outside `R<n>`, and [Research](research.md) quantity
   labels on numeric bounds.
6. **Mechanism leak & deletion test.** Apply the deletion test to every inferred
   clause. `FAIL` on leaked mechanisms, crate/file placement, task slicing,
   invented unfixed internal/operator identifier spellings left open by
   `intent.md` and `api/` (owned by Technical Design's [Pinned shared
   identifiers](../interfaces/design-v1.md)), breaking changes misplaced inside
   `Preserved guarantees`, or clauses excluding a contract-valid design without
   protecting an accepted external guarantee.

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

## Blocking Bar And Verdict *(Draft Specification)*

| Disposition | Bar |
| --- | --- |
| Blocking (`FAIL`) | Surviving witnessed check with anchor, classification (`PHASE_DEFECT` or `UPSTREAM_GAP`), and smallest owner (`Specification`, `Intake`, or external policy owner). |
| Bounded (`CONCERNS`) | Strictly a bounded downstream proof or external risk over closed behavior—never behavioral ambiguity (`FAIL`); return `CONCERN` with **risk, downstream owner, and closing action**. |
| Non-blocking (never blocks `PASS`) | Downstream Technical Design choices, equivalent phrasing with no external divergence, or speculative scenarios outside `intent.md` and affected surfaces. |

Return `PASS` when every assigned check clears with per-check proof of attack
and required inspection in `evidence_boundary`; otherwise return `CONCERNS` or
`FAIL` per the table above.
