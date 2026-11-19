# Specification Review

Review one fixed Definition candidate under [Review Lifecycle](../shared/review.md#lifecycle).
For an inline evidence package use [Already-Satisfied No-Delta](#already-satisfied-no-delta).

## Execution Order, Scope And Primary Sources

**Definition owner.** For a draft spec covering multiple surfaces, breaking or external compatibility,
authorization, money, data integrity or concurrent state transitions, dispatch
two fresh reviewers in parallel, one per axis. Otherwise one fresh reviewer
covers checks 1–6 in order. Use fresh context without parent history. Give each
reviewer the fixed spec, ready Intent, assigned checks and method path. Include
task research for the reviewer covering Axis 1. Keep panel reports distinct.
Every assigned axis must clear on the same candidate under the shared lifecycle.

**Reviewer.** Run only the checks assigned in the brief.

- **Axis 1 — Grounding and fidelity (2, 3).** Inspect primary code, callers,
  schemas, migrations, config, profiles and tests, including uncited interactions
  across all [Behavioral Probes](specification/behavioral-probes.md). Check
  accepted policy against its owning document.
- **Axis 2 — Cold-start and contract tightness (1, 4–6).** Use Intent, the
  spec and cited sources. Complete check 1 before uncited inspection, including
  in a single-reviewer run. In check 6 consult an uncited named seam only to
  establish whether an identifier is fixed.

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

### Phase A — Cold-Start

1. **Cold-start & falsifier quality.** Check whether a downstream reader can act on the brief. Fail on:
   - missing product context a cold-start consumer (`Technical Design` or
     `Planning`) cannot answer without chat archaeology;
   - broken anchor or residual `Blocked decisions` in a `draft`;
   - any `Falsifier` violating [Specification
     V1](../interfaces/spec-v1.md#field-rules) (tautological, vague, wrong
     layer, or prescribing tests/proof gates).

### Phase B — Grounding, Fidelity & Codebase Reality

2. **Intent fidelity, authority and decisions.** Independently trace every
   material obligation, scenario and binding under [Intent Field Rules](../interfaces/intent.md#field-rules)
   and every live research unknown, conflict or refresh trigger to its disposition.
   Check authority for changed behavior, assumptions and delegated choices, including
   options hidden in non-goals or falsifiers. Verify decisions against
   [Specification V1](../interfaces/spec-v1.md#field-rules).
3. **Primary-source truth, codebase interactions & Probe #5 (blast radius /
   existing state).** Independently inspect primary sources and callers across
   the blast radius against [Behavioral
   Probes](specification/behavioral-probes.md). `FAIL` on:
   - a false spec baseline (`PHASE_DEFECT`), or a verified false premise in
     the accepted Intent (`UPSTREAM_GAP`, reopen Intake);
   - uncited codebase interaction missed by `spec.md` across Probes #1–#6, or
     generic filler in `Preserved guarantees`;
   - reversing/adopting a rejected `docs/decisions/` choice without meeting its
     reopen condition or an explicit `intent.md` mandate, or omitting a required
     `Durable contract update` (`Domain Vocabulary`, `Production Contract`, or
     `docs/decisions/`).

### Phase C — Behavioral Tightness & Mechanism Purity

Witnesses: Checks 4–5 give **starting state**, **trigger**, **observable
result(s)**, and **exact anchor**; Check 6 gives **leaked clause**, **valid
alternative satisfying `intent.md` and cited contracts without it**, and **the authority that permits the alternative's observable outcome**.

4. **Malicious compliance & Probe #3 promise boundaries.** Construct a literal
   implementation satisfying every `R<n>` and `G<n>` together that still breaks
   an observable outcome or `Success signal` in `intent.md` (excluding
   non-behavioral goals in `Downstream handoff`). Attack every relied-on
   response, status, event, or ack for *false-positive success* and
   *unobservable completion*.
5. **Two-implementations divergence.** Apply probes 1–4 and 6 and the spec's
   stated existing-state and compatibility rules. Can two implementations obey
   the contract yet yield materially different external outcomes outside an
   explicit permitted variation? Check table coverage, terminology, quantity
   labels and breaking-change fields against [Specification V1](../interfaces/spec-v1.md#field-rules).
6. **Mechanism leak.** For each inferred restriction, try to exhibit a design
   that satisfies the accepted guarantees and bindings while violating only that
   clause. Check identifier authority under [Field Rules](../interfaces/spec-v1.md#field-rules).

## Already-Satisfied No-Delta

Confirm `intent.md` is at `status: ready` and independently verify every
functional, operational, quantitative, and technical `intent.md` obligation and
`Success signal` against Axis 1's primary sources and callers, recording
inspected locators in `evidence_boundary`. Use the no-delta verdicts in
[Review Result V1](../interfaces/review-result-v1.md).
Pass only when primary evidence proves every obligation. Classify a missing
accepted input or false Intent premise as an upstream gap. Remaining work or
insufficient evidence for the candidate's zero-delta claim is a phase defect.

## Blocking Bar And Verdict *(Draft Specification)*

| Disposition | Bar |
| --- | --- |
| Blocking (`FAIL`) | Surviving witnessed check with anchor, classification (`PHASE_DEFECT` or `UPSTREAM_GAP`), and smallest owner (`Specification`, `Intake`, or external policy/evidence owner). |
| Bounded (`CONCERNS`) | `CONCERN` per [Review Result V1](../interfaces/review-result-v1.md) over closed behavior—never behavioral ambiguity (`FAIL`). |
| Non-blocking (never blocks `PASS`) | Downstream Technical Design choices, equivalent phrasing with no external divergence, or speculative scenarios outside `intent.md` and affected surfaces. |

Return `PASS` when every assigned check clears with per-check proof of attack
and required inspection in `evidence_boundary`; otherwise return `CONCERNS` or
`FAIL` per the table above.
