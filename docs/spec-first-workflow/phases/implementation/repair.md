# Repair Assignment

Use as a fresh Acceptance-Unit Lead (`ACCEPTANCE_UNIT_LEAD`) dispatched on a
Final Validation [Repair
Request](../../interfaces/acceptance-result-v1.md#repair-request), or as a
root-local inline Lead repairing its own Final Validation finding.

## Steps

### 1. Ground And Reproduce (`red`)

- Read every finding in the `Repair Request`, affected packets, and prior task
  `decisions`.
- Verify each finding against primary code and reproduce every product, test,
  or seam failure on the assembled candidate before editing.
- **Refuted finding:** if primary code or a direct check proves the claimed
  invariant already holds, edit nothing for that finding and record the
  refuting anchor and output in `decisions`. If every finding is refuted,
  return `Blocked` with `next_owner: Delivery Owner — refuted findings` instead
  of inventing a code change.

Complete this step only when every finding is reproduced (`red`) or refuted
with primary-source proof.

### 2. Fix At The Root Cause

Treat suggested review remedies as evidence, not mandates. Fix each confirmed
finding at its shared root inside the granted writable scope (reconciling both
sides of any `INTEGRATION_DEFECT` seam) while preserving
[Engineering](../../../../AGENTS.md#engineering): keep guards intact and
avoid unjustified waivers, crates, layers, config keys, or fallbacks.

### 3. Verify (`green`) And Return

Confirm every reproduced check passes with **non-zero** executed tests, pass
the [Pre-Return Self-Audit](unit.md#3-pre-return-self-audit-and-return) on the
repaired surface, and emit [Implementation
Result](../../interfaces/acceptance-result-v1.md#implementation-result)
recording in `decisions`: (1) each repaired root cause, (2) any refuted finding
with its anchor and proof, and (3) the exact invalidated check surface.
