# Final Validation

Use as Delivery Owner once every planned unit is `Implemented`, the candidate
is assembled, and all writers have joined. Prove the fixed assembled candidate
against the accepted outcome and report one [Completion
Result](../../interfaces/acceptance-result-v1.md#completion-result) — or a
[Repair Request](../../interfaces/acceptance-result-v1.md#repair-request) when
an in-scope defect requires repair.

## Steps

### 1. Pin Candidate And Trace Obligations Bidirectionally

- Pin the exact assembled candidate (`<Base>..HEAD` from `tasks.md` plus
  `git status`), confirming zero active writers and no stray untracked
  artifacts.
- Read **primary sources**, not only Lead summaries: `tasks.md` (`Completion`,
  `Global constraints`, every task's `decisions` and `unverified`), every
  packet's `Final validation` section, and `spec.md`, `design/`, and
  `rollout.md` when present. (A root-local Lead reads the same fields from its
  inline unit.)
- **Forward trace:** trace every accepted obligation, `Observable claim`,
  preserved guarantee, and `unverified` entry to a concrete observable result
  on the affected paths. For a removal, verify the old behavior is unreachable
  on withdrawn paths; for a rename alone, verify its promised behavior still
  holds. Inspect every cross-unit `Provides -> Depends on` seam and compare
  task `decisions` entries for silent cross-unit mismatches.
- **Backward trace:** inspect the actual `<Base>..HEAD` diff and trace every
  material behavior change back to an accepted obligation (catching scope
  creep, leftover superseded code, or accidental side effects).

Complete this step only when every accepted obligation, cross-unit seam,
`unverified` entry, and material diff hunk maps to a valid existing check, a
planned check, a named gap, or a defect.

### 2. Run Non-Overlapping Local Checks

- Select one non-overlapping verification plan for the changed surface from
  [Validation budget](../../../../AGENTS.md#validation-budget) and reconcile
  any packet `Additional proof` with its authoritative source. Load
  [Validation Routing](../../../validation-routing.md) for mixed or specialized
  surfaces and [Evidence Contract](../../shared/evidence-contract.md) for
  reuse, claim scope, additional proof, or unavailable infrastructure.
- Run fast mechanical checks first (`fmt-check`, `lint`, `build`, relevant
  tests, generated-contract checks). Include variants, generated clients, or
  intermediate versions when selected checks or accepted scope require them.
- **Proof bar:** count only executed checks with **non-zero** test counts that
  observe the claimed boundary; never reuse a pre-assembly Lead result across a
  seam or dependency touched by another unit; record unavailable optional
  infrastructure as a named limitation rather than fabricating green or
  building ad-hoc stacks.

If any required local check fails, go directly to Step 4 — never dispatch an
independent review on a failing build or test candidate.

### 3. Evaluate Review Trigger And Freeze Candidate

- Apply [Review Trigger](../../shared/review.md#trigger) to the fixed, locally
  passing candidate: **Full final review**, **Structure-only review**, or
  **Direct completion**.
- Keep the candidate frozen while `reviewer-agent` runs, selecting capability
  tier per the active harness adapter for high-risk or structure-only reviews.
  Pass sparse **primary-source** pointers: pinned `<Base>..HEAD` candidate
  identity, authoritative accepted sources (`spec.md`, `design/`, `tasks.md`,
  packets), and current check receipts.
- On `FAIL` or `NEEDS_PARENT`, go to Step 4. On `PASS` (or Direct completion),
  go to Step 5.

### 4. Classify Failures And Route Repairs

Classify every check or review failure before acting:

| Classification | Action |
| --- | --- |
| `product`, `test_oracle`, `integration_defect`, or `structure_defect` | Ensure no active reader runs and release validation locks (retaining the reviewer identity when a bounded delta recheck may follow). In a ledger, return a [Repair Request](../../interfaces/acceptance-result-v1.md#repair-request) (`verdict: NeedsRepair`) naming the pinned `candidate`, anchored falsified claim/finding, classification, evidence, smallest owning task(s) (both seam tasks for `INTEGRATION_DEFECT`), and `retained_evidence` unaffected by the repair. A root-local Lead repairs directly under [Repair Assignment](unit.md#repair-assignment). |
| `environment` (optional) | Record the material gap in `evidence` without holding acceptance. |
| `environment` (required), `UPSTREAM_GAP`, or `NEEDS_PARENT` | Close a missing check directly if inside Delivery Owner authority; otherwise apply [Evidence Contract](../../shared/evidence-contract.md#required-and-optional-proof) and return `Blocked` naming the unverified claim and owner. |

**On resume after a repair integrates:** re-pin the updated candidate, run only
failed, invalidated, and not-yet-run required checks, and request a bounded
**delta recheck** from the reviewer (or fresh review if the repair widened
interfaces or risk under [Review](../../shared/review.md#lifecycle)). Apply
[Parent-Owned Recovery](../../shared/transition.md#parent-owned-recovery) when
repeated attempts add no discriminating evidence.

### 5. Emit Completion Result

Return [Completion
Result](../../interfaces/acceptance-result-v1.md#completion-result) only when
every gate holds, carrying forward local validation evidence:

- [ ] Every `Observable claim`, `Completion` criterion, and `unverified` entry
  has current proof at its required boundary, or a named gap.
- [ ] Every material behavior change in `<Base>..HEAD` serves an accepted
  obligation.
- [ ] All blocking check and review findings are closed and any selected review
  returned `PASS` (or `not_required` recorded).
- [ ] Any explicitly requested external outcome (CI, deployment, runtime
  observation) is directly observed (or local results are retained in
  `evidence` while keeping Completion open / returning `Blocked` if unreachable).

## Conditional Branches

- **Runtime or fixture scenarios:** when explicitly required, run the smallest
  end-to-end scenario covering setup, behavior, observation, and bounded
  cleanup first, reusing valid prepared environments.
- **Isolated or remote execution:** resolve candidate or image digests,
  checksums, and host paths from task state or its manifest before consuming
  results, updating replacement locators and invalidating only affected proof
  after repair.
- **Long checks and resource waits:** record an observation checkpoint in
  existing execution state and inspect logs, processes, or lock owners via
  background execution so concrete delay causes are visible in place.
