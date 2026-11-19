# Specification Review

Use when [Review](../shared/review.md) routes a fixed Definition result
(`specs/<task>/spec.md` at `status: draft` or no-delta evidence) or on an
explicit standalone specification review. Stay read-only: verify claims against
primary repository sources and return anchored findings with their smallest
owner in [Review Result V1](../interfaces/review-result-v1.md) without
rewriting `spec.md` or inventing product decisions.

For a no-delta result, jump directly to [Verdict](#verdict).

## Adversarial Falsifiers

Run every falsifier below against `intent.md`, `spec.md`
([Specification V1](../interfaces/spec-v1.md)), cited `research/*.md`, and
primary repository sources (`api/openapi.yaml`, code, migrations, config,
tests, `docs/decisions/`). Stop a broken path at its earliest defect while
continuing independent paths so one pass surfaces all material findings.

For scenario findings (falsifiers 3–5), supply the concrete witness: **starting
state**, **trigger**, **observable result** (or two diverging results), the
**exact anchor** in `intent.md`, `spec.md` (`R<n>` / `G<n>`), or primary
source, and how it breaks the named promise or excludes a contract-permitted
result.

1. **Intent fidelity & authority.** Map every material item in `intent.md`
   (including delegated limits and `Open questions`) to `spec.md`. `FAIL` on a
   dropped item, weakened constraint, unauthorized breaking change, delegated
   choice exceeding its limits, or undelegated requester-owned decision
   (`reopen_owner: Intake`). Check `Non-goals`, `Decisions, assumptions, and
   reopen triggers`, and `Falsifier` entries for smuggled behavioral decisions
   or assumptions lacking an objective `Reopen if` trigger.
2. **Source truth & blast radius.** Independently read primary code, schemas,
   config, migrations, and callers for every touched surface—do not trust prose
   about current behavior. `FAIL` if a baseline claim contradicts actual code,
   a materially affected surface or supported consumer guarantee is missing
   from `Preserved guarantees`, or a changed guarantee lacks authorization,
   former-caller behavior, or mixed-version compatibility expectations where
   relevant.
3. **Malicious compliance & promise boundaries.** Construct a literal
   implementation passing every `R<n>` that still misses `intent.md`'s outcome
   or `Success signal`, evaluating interacting rules together. On any signal,
   status, event, or ack an actor relies on, test for *false-positive success*
   and *unobservable completion*.
4. **Two-implementations divergence.** Construct a concrete scenario (unhappy
   paths, invalid/repeated transitions, idempotency/replay, partial failure,
   absence/bounds, rule precedence) where two implementations both satisfy
   `spec.md` yet yield different observable results without a stated invariant
   permitting both. Check domain vocabulary consistency across rules.
5. **Mechanism leak & deletion test.** Construct a valid outcome or
   implementation satisfying `intent.md` and cited constraints yet excluded by
   an unauthorized clause in `spec.md`. `FAIL` on leaked mechanisms, crate/file
   placement, task slicing, or inferred rules that fail the deletion test.
6. **Cold-start & proof-boundary discipline.** Walk `intent.md`, `spec.md`, and
   cited links with zero chat history. `FAIL` if Design or Planning must guess
   product meaning, a cited primary anchor is broken, a rule lacks a concrete
   observable `Falsifier`, or `spec.md` prescribes tests, fixtures, proving
   layers, commands, or unmandated live gates
   ([Evidence Contract](../shared/evidence-contract.md#required-and-optional-proof)).

## Verdict

- **Draft specification:**
  - `PASS`: all six falsifiers clear with no surviving material divergence or
    authority gap. Never block `PASS` on mechanism choices owned by Technical
    Design, task slicing owned by Planning, test implementation owned by
    Implementation, phrasing preferences where no two implementations can
    diverge, or speculative scenarios outside `intent.md` and affected
    surfaces.
  - `CONCERNS`: only a bounded later proof or risk obligation over accepted
    behavior; name it so Definition persists it in `Carried concerns` before
    setting `status: ready`.
  - `FAIL`: any surviving falsifier; anchor each finding to `R<n>`, `G<n>`,
    `intent.md`, or primary source and name the smallest owner (`Specification`,
    `Intake`, or external policy owner).
- **No-delta result:** `PASS` requires primary evidence proving every accepted
  obligation already holds without behavioral change; `FAIL` with
  `reopen_owner: Intake` if `intent.md` rested on a false premise, or
  `Specification` if an unaddressed delta remains.
