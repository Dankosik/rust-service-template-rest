# Repair Assignment

Use as a fresh Acceptance-Unit Lead (`ACCEPTANCE_UNIT_LEAD`) dispatched on a
Final Validation [Repair
Request](../../interfaces/acceptance-result-v1.md#repair-request), or as a
root-local inline Lead repairing its own Final Validation finding.

## Steps

### 1. Ground And Reproduce (`red`)

- Read every finding in `Repair Request`, affected packets, prior `decisions`,
  and — on **Round 2** — the brief's Round 1 commit SHA (or repaired paths and
  summary in `no-commit` / inline mode), failed hypothesis, and recheck
  evidence.
- For Rust repairs, load [Rust
  Coder](../../../../.agents/skills/rust-coder/SKILL.md), [Rust
  Testing](../../../../.agents/skills/rust-testing/SKILL.md) when changing
  tests, [Rust Debugging](../../../../.agents/skills/rust-debugging/SKILL.md)
  whenever root cause is uncertain, async/flaky, or this is **Round 2** (state
  2–3 ranked **falsifiable hypotheses** before editing), and only other
  `.agents/skills/rust-*` methods matching an open decision.
- Verify each finding against primary code (including surrounding callers) and
  reproduce every confirmed failure on the assembled candidate before editing.
- **Refutation bar:** refute a finding only when primary code or a direct check
  disproves the exact counterexample path — a passing existing test never
  refutes a finding unless it exercises that counterexample. Edit nothing for a
  refuted finding and record the refuting anchor and proof in `decisions`. If
  every finding is refuted, return `Blocked` with `next_owner: Delivery Owner —
  refuted findings` (a root-local inline Lead records the refuting anchor and
  returns directly to [Final Validation](final-validation.md#4-verify-findings-classify-and-route-repairs)).

Complete this step only when every finding is reproduced (`red`) or refuted
with primary-source proof.

### 2. Fix At The Root Cause And Lock Regression Proof

- Treat suggested review remedies as evidence, not mandates. Fix each confirmed
  finding at its causal root inside granted writable scope (reconciling both
  sides of any `INTEGRATION_DEFECT` seam) while preserving
  [Engineering](../../../../AGENTS.md#engineering) and [Rust Change
  Surface](../../../../AGENTS.md#rust-change-surface).
- **Regression guard:** for every confirmed behavioral or seam finding, lock
  down a minimal seam-level regression test observed `red` before the fix and
  `green` after (or record why no seam exists — itself a structural finding).

### 3. Verify (`green`) And Return

Confirm every reproduced check and changed crate's test suite (or proving
consumer crate when a touched crate holds zero tests) passes with **non-zero**
executed tests, pass [Pre-Return
Self-Audit](unit.md#3-pre-return-self-audit-and-return) on the repaired surface
(leaving scoped edits uncommitted in the shared checkout, or committing on a
worktree branch only if isolated), and emit [Implementation
Result](../../interfaces/acceptance-result-v1.md#implementation-result)
recording in `decisions`: (1) each repaired root cause, (2) any refuted finding
with its anchor and proof, and (3) the exact invalidated check surface. (A
root-local inline Lead returns directly to [Final
Validation](final-validation.md#4-verify-findings-classify-and-route-repairs).)

For scope expansion or an exposed `UPSTREAM_GAP`, follow [Unit
Escalation](unit.md#escalation).
