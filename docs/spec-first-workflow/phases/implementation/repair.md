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
- For Rust repairs apply [Rust Change Surface](../../../../AGENTS.md#rust-change-surface) and load `rust-debugging` for an uncertain or async/flaky cause or a second attempt.
- **Uncertain cause or Round 2:** before editing, state 2–3 ranked falsifiable
  hypotheses as `If <X> is the cause, then <changing/observing Y> produces <Z>`.
- Confirm each finding against live code and surrounding callers:

| Finding type | Confirmation bar |
| --- | --- |
| Failing check or existing test | Run the exact failing command on the assembled candidate and observe `red`. |
| Behavioral, runtime, or seam counterexample (Axis 1–2) | Verify the path in live code/callers, reuse or write a minimal seam-level regression test reproducing it, and observe `red` before editing production code (or record why no seam exists — itself a structural finding). |
| Structural or diff-hygiene violation (Axis 3) | Verify callers and invariants against [Engineering](../../../../AGENTS.md#engineering) and [Rust Change Surface](../../../../AGENTS.md#rust-change-surface); confirm existing seam tests start `green` (`green -> green`). |
| Weak or tautological test (Axis 4) | Confirm the named surviving mutant against live code; prepare a seam test that fails on the broken branch and passes on live behavior. |

- **Refutation bar:** refute a finding only when primary code or a direct check
  disproves the exact counterexample path — a passing existing test never
  refutes unless it exercises that counterexample. Edit nothing for a refuted
  finding and record the refuting anchor and proof in `decisions`. If **every**
  finding is refuted, return `Blocked` (`next_owner: Delivery Owner — refuted
  findings`); on partial refutation, record refuted items in `decisions` and
  fix the confirmed ones.

### 2. Repair Confirmed Defects

Fix each confirmed root cause inside the grant. For an integration defect reconcile both sides of the seam. Evaluate proposed remedies under [Review Lifecycle](../../shared/review.md#lifecycle).

### 3. Verify (`green`) And Return

Rerun the reproducer or strengthened test and apply the [Pre-Return Self-Audit](unit.md#3-pre-return-self-audit-and-return) to the repaired surface. Record in the result's `decisions`: (1) each repaired root cause (and confirmed Round 2
hypothesis), (2) any refuted finding with anchor and proof, and (3) exact
invalidated check surface.

For scope expansion, an ungranted Cargo crate/feature, or an exposed
`UPSTREAM_GAP`, follow [Unit Escalation](unit.md#escalation).
