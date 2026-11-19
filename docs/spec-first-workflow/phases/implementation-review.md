# Implementation Review

Use when shared [Review](../shared/review.md) routes a fixed implementation
candidate (assembled ledger, single-unit delivery, or standalone review). Stay
read-only and review directly without repairing, re-scoping, or accepting.

Pin the candidate diff (`<Base>..HEAD` or supplied diff command), confirm the
ref resolves and the diff is non-empty (returning `REVIEW_HANDOFF_INVALID`
otherwise), and **read touched production code before its tests**. Consume
accepted sources (`intent.md`, `spec.md`, `design/`, `rollout.md`, `tasks.md`,
packets) and check receipts; run a bounded existing diagnostic only for a
concrete falsification question under [Evidence
Contract](../shared/evidence-contract.md#required-and-optional-proof).

## Adversarial Falsifiers

Evaluate every assigned axis (all four in single-reviewer mode, or your pair in
[Complementary Panel](#complementary-panel)) and report under explicit per-axis
headings (`none` plus one line naming what was attacked when an axis passes) so
one passing axis never masks another's defect. Skip anything deterministic
tooling (`rustc`, `clippy`, `rustfmt`, `quality/architecture.json`, `jscpd`)
already enforces.

1. **Outcome, Spec & Scope Parity.** Start from accepted sources and
   `Completion`. Construct a concrete caller or operator scenario where every
   executed test stays green yet an accepted obligation, preserved `G<n>`,
   withdrawn removal path, or `Completion` criterion fails. Trace every
   material diff hunk back to an accepted obligation; flag unrequested behavior
   or silent scope drift.
2. **Real-Path Defects & Cross-Unit Seams.** Attack touched production code
   with four probes:
   - *Precedence & rejection:* two rules or errors firing together; state
     mutation or info leak before rejection.
   - *Partial failure & cancellation:* future dropped mid-`.await`, halfway
     write failure, timeout, held lock/permit, non-idempotent retry, or
     irreversible migration.
   - *Silent degradation:* `let _ =`, `.ok()`, catch-all arms, or defaults
     turning real failures into silent success.
   - *Cross-unit seams:* compare exact types, error codes, units, defaults, and
     recorded task `decisions` across both sides of every `Provides -> Depends
     on` seam (`INTEGRATION_DEFECT`).
3. **Structure & Single Ownership.** Apply `rust-structural-quality`'s
   **deletion test** and [Engineering](../../../AGENTS.md#engineering). Flag
   code reimplementing `std`, a locked `Cargo.lock` crate, or an authoritative
   tool with matching semantics; splitting one fact across hand-synced
   carriers; or adding an unjustified crate, dependency/feature, layer, trait,
   config key, or fallback/shim. **Repo standards override generic taste:**
   suppress objections where `AGENTS.md` or `docs/decisions/` endorses the
   pattern. Name the violated constraint and exact item to delete, collapse,
   bind, or delegate.
4. **Test & Proof Falsifiability.** Apply the **Mutation Mental Test** to every
   added or edited test: mentally invert or delete the single production branch
   it claims to guard — would it actually go **red**? Flag **tautological**
   assertions (recomputing expected values like the code), tests mocking the
   claimed boundary or asserting only `is_ok()` / internal steps, sleeps or
   retries masking flakes, duplicate same-layer suites, and deleted or weakened
   safety, durability, or sole regression guards.

## Blocking Bar And Verdict

Return [Review Result V1](../interfaces/review-result-v1.md) (`PASS | FAIL |
NEEDS_PARENT`, or `REVIEW_HANDOFF_INVALID`). Keep each finding compact and
structured (anchor, violated claim, trace/counterexample, smallest owner — no
essays):

| Disposition | Bar |
| --- | --- |
| Blocking (`TASK_DEFECT`, `INTEGRATION_DEFECT`, `UPSTREAM_GAP`) | Cites (1) falsified accepted claim or constraint, (2) anchor (`path::Symbol` / hunk), (3) reproducible trace, counterexample, or check, and (4) smallest owner. Real in-scope defects from optional checks block. |
| Non-blocking (`FOLLOW_UP`, never blocks `PASS`) | Style preferences, equivalent single-owner structure, naming polish, speculative future risks, untouched historical test debt, and missing optional integration/provider/container evidence. |

## Complementary Panel

For multi-task ledgers (≥2 planned code tasks) or high-risk changes adding
structure, two fresh `reviewer-agent` actors run in parallel on the frozen
candidate:

- **Lens A — Spec, Scope & Proof:** Falsifiers **1 and 4** exclusively.
- **Lens B — Runtime Defects, Seams & Structure:** Falsifiers **2 and 3**
  exclusively.

**Synthesis (Delivery Owner):** `PASS` requires both lenses `PASS`; any `FAIL`
or `NEEDS_PARENT` holds, reported verbatim per axis without reranking. Resolve
an evidence-backed conflict between lenses via `adjudicator-agent`.

## Delta Recheck

Falsify only the brief's anchored findings, the repair range
(`<old-HEAD>..<new-HEAD>`), invalidated proof, and structure added by the
repair against [Engineering](../../../AGENTS.md#engineering). Keep unaffected
closed reasoning intact.

## Structure-Only Review

Run **Falsifier 3** exclusively on each structural addition named in the brief;
keep behavior and proof outside scope. Classify
[Engineering](../../../AGENTS.md#engineering) violations as `TASK_DEFECT`
naming the item to delete, collapse, bind, or delegate; classify other
observations as `FOLLOW_UP`.
