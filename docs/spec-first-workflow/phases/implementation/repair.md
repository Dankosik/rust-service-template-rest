# Repair Assignment

Use as a fresh Repair Lead (`ACCEPTANCE_UNIT_LEAD`) dispatched on a Final
Validation [Repair
Request](../../interfaces/acceptance-result-v1.md#repair-request). For
`no-commit` or inline-unit overrides, follow [Execution Modes](modes.md).

## Steps

### 1. Ground And Confirm Each Finding

- Read every finding in `Repair Request`, affected packets, prior `decisions`,
  and — on **Round 2** — `prior_attempt` (Round 1 candidate, attempted
  hypothesis, and recheck failure).
- For Rust repairs, load [Rust
  Coder](../../../../.agents/skills/rust-coder/SKILL.md), [Rust
  Testing](../../../../.agents/skills/rust-testing/SKILL.md) when changing
  tests, [Rust Debugging](../../../../.agents/skills/rust-debugging/SKILL.md)
  when root cause is uncertain, async/flaky, or this is **Round 2**, and only
  other `.agents/skills/rust-*` skills matching an open decision.
- **Uncertain cause or Round 2:** before editing, state 2–3 ranked falsifiable
  hypotheses as `If <X> is the cause, then <changing/observing Y> produces <Z>`.
- Confirm each finding against live code and surrounding callers:

| Finding type | Confirmation bar |
| --- | --- |
| Failing check or existing test | Run the exact failing command on the assembled candidate and observe `red`. |
| Behavioral, runtime, or seam counterexample (Axis 1–2) | Verify the path in live code/callers, write a minimal seam-level regression test reproducing it, and observe `red` before editing production code (or record why no seam exists — itself a structural finding). |
| Structural or diff-hygiene violation (Axis 3) | Verify callers and invariants against [Engineering](../../../../AGENTS.md#engineering) and [Rust Change Surface](../../../../AGENTS.md#rust-change-surface); confirm existing seam tests start `green` (`green -> green`). |
| Weak or tautological test (Axis 4) | Confirm the named surviving mutant against live code; prepare a seam test that fails on the broken branch and passes on live behavior. |

- **Refutation bar:** refute a finding only when primary code or a direct check
  disproves the exact counterexample path — a passing existing test never
  refutes unless it exercises that counterexample. Edit nothing for a refuted
  finding and record the refuting anchor and proof in `decisions`. If **every**
  finding is refuted, return `Blocked` (`next_owner: Delivery Owner — refuted
  findings`); on partial refutation, record refuted items in `decisions` and
  fix the confirmed ones.

Done when every finding clears its confirmation bar or is refuted with
primary-source proof.

### 2. Fix At The Root Cause Without Band-Aids

- Treat suggested review remedies as evidence, not mandates. Fix each confirmed
  finding at its causal root inside granted writable scope (reconciling both
  sides of any `INTEGRATION_DEFECT` seam) while preserving
  [Engineering](../../../../AGENTS.md#engineering) and [Rust Change
  Surface](../../../../AGENTS.md#rust-change-surface).
- **Anti-band-aid bar:** `Delta Recheck` falsifies repair structure — a quick
  fix using an unjustified shim, fallback, config flag, `.clone()`/`Arc`
  workaround, or new dependency still fails.

### 3. Verify (`green`) And Return

Confirm every reproduced check, new/strengthened regression test, and changed
crate's test suite (or proving consumer crate when a touched crate holds zero
tests) passes with **non-zero** counts. Pass [Pre-Return
Self-Audit](unit.md#3-pre-return-self-audit-and-return) on the repaired surface
(leaving scoped edits uncommitted in the shared checkout, or committing on a
worktree branch only if isolated), and emit [Implementation
Result](../../interfaces/acceptance-result-v1.md#implementation-result)
recording in `decisions`: (1) each repaired root cause (and confirmed Round 2
hypothesis), (2) any refuted finding with anchor and proof, and (3) exact
invalidated check surface.

For scope expansion, an ungranted Cargo crate/feature, or an exposed
`UPSTREAM_GAP`, follow [Unit Escalation](unit.md#escalation).
