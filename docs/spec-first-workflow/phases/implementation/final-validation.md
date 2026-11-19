# Final Validation

Use as Delivery Owner once every planned unit is `Implemented` and assembled.
Prove the fixed candidate against the accepted outcome and return one
[Completion Result](../../interfaces/acceptance-result-v1.md#completion-result)
— or one batched [Repair
Request](../../interfaces/acceptance-result-v1.md#repair-request) for in-scope
defects.

**Read-only in a ledger:** keep candidate files untouched during ledger Final
Validation; inspect code/diffs, run checks, and dispatch review, routing all
code, test, or generated-file edits through `NeedsRepair` (only a root-local
inline Lead edits directly).

## Steps

### 1. Pin Candidate And Run Fast Mechanical Baseline

- Pin the assembled candidate (`<Base>..HEAD`, or `git diff <Base>` in
  `no-commit` mode, plus `git status`), confirming zero active writers and no
  stray untracked files.
- Read `tasks.md` (`Completion`, `Global constraints`, task `decisions` and
  `unverified`), each packet's `Final validation`, and `git diff --stat`.
- Select one non-overlapping verification plan from [Validation
  budget](../../../../AGENTS.md#validation-budget), reconciling any packet
  `Additional proof` with its authoritative source (load [Validation
  Routing](../../../validation-routing.md) for mixed/specialized surfaces and
  [Evidence Contract](../../shared/evidence-contract.md) for reuse, scope, or
  unavailable infrastructure).
- Run fast mechanical checks (`fmt-check`, `lint`, `build`, relevant tests,
  generated-contract checks, required variants). **Proof bar:** count only
  executed checks with **non-zero** test counts observing the claimed boundary;
  never reuse pre-assembly Lead results across seams touched by another unit;
  record unavailable *optional* infrastructure as a limitation rather than
  fabricating green or building ad-hoc stacks.

If build or lint fails such that tests cannot run, go straight to Step 4;
otherwise finish test execution and continue to Step 2 so check failures and
trace gaps batch into one `Repair Request`.

### 2. Trace Obligations Bidirectionally

- Read **primary sources**, not only Lead summaries: `intent.md`, `spec.md`,
  `design/`, `rollout.md` (when present), packets, and the candidate diff.
- **Forward trace:** map every accepted obligation, active `Observable claim`
  (treating `superseded at assembly by T<contract>` as closed by `T<contract>`
  and `Completion`), preserved `G<n>`, and `unverified` entry to a current
  passing check on affected paths (running any locally runnable `unverified`
  test now). Verify removals make old behavior unreachable and renames preserve
  behavior. Inspect every cross-unit `Provides -> Depends on` seam and compare
  task `decisions` for silent mismatches.
- **Backward trace:** map every material behavior change in the diff back to an
  accepted obligation (catching scope creep, leftover superseded code, or
  unintended side effects).

Complete this step only when every obligation, seam, `unverified` entry, and
material diff hunk maps to a passing check, an unavailable *optional* gap, or a
concrete defect. If any required check failed or any defect or missing required
coverage survived Steps 1–2, go to Step 4 — never dispatch review on a failing
candidate.

### 3. Evaluate Review Trigger And Freeze Candidate

Evaluate [Review Trigger](../../shared/review.md#trigger) against concrete diff
signals (`Cargo.toml`, new traits/modules/config keys/fallbacks, protected
domains, and task count):

- Select **2-lens [Complementary
  Panel](../implementation-review.md#complementary-panel)** for multi-task
  ledgers (≥2 planned code tasks) or high-risk changes adding structure; one
  fresh 4-axis `reviewer-agent` for other Full reviews; **Structure-only** when
  only structural additions fire (naming every triggering addition in the
  brief); or **Direct completion** for single-task/inline candidates matching
  no trigger (recording `review: not_required (<checked signals: none matched>)`
  and jumping to Step 5).
- Keep the candidate frozen while `reviewer-agent` runs (tier per harness
  adapter). **Reviewer brief:**
  - mode (`Full — 4 axes`, `Full — Lens A (Axes 1+4)` / `Lens B (Axes 2+3)`,
    `Structure-Only` + triggering additions, or `Delta Recheck` + anchored
    findings and repair range)
  - pinned diff command (`git diff <Base>..HEAD` or `git diff <Base>`) and
    commit list
  - accepted sources present (`intent.md`, `spec.md`, `design/`, `rollout.md`,
    `tasks.md`, packets) and current check receipts
- On 2-lens return, keep both reports verbatim by axis without reranking or
  masking either lens's `FAIL` (use `adjudicator-agent` only for an
  evidence-backed conflict between lenses). On `FAIL` or `NEEDS_PARENT`, go to
  Step 4; on `PASS`, go to Step 5.

### 4. Verify Findings, Classify, And Route Repairs

**Verify before routing:** check every review finding against its cited code
anchor and trace before acting; dismiss unanchored or factually refuted claims
with their refuting anchor (or escalate contested claims via
`adjudicator-agent`).

Batch surviving failures and verified findings:

| Classification | Action |
| --- | --- |
| `TASK_DEFECT` or `INTEGRATION_DEFECT` | Stop active readers and release validation locks (retaining reviewer identity when the adapter permits delta-recheck continuation). In a ledger, return **one batched** [Repair Request](../../interfaces/acceptance-result-v1.md#repair-request) (`verdict: NeedsRepair`) naming all surviving findings, owning tasks (both seam tasks for `INTEGRATION_DEFECT`), and `retained_evidence`. Root-local Leads repair under [Repair Assignment](repair.md). |
| `environment` (optional) | Record the material gap in `evidence` without holding acceptance. |
| `environment` (required), `UPSTREAM_GAP`, or `NEEDS_PARENT` | Run a missing local check directly if inside authority (keeping candidate read-only); otherwise apply [Evidence Contract](../../shared/evidence-contract.md#required-and-optional-proof) and return `Blocked` naming claim and owner. |

**Repair-round cap:** allow at most **2 repair rounds** per anchored defect
class (per [Review](../../shared/review.md#lifecycle)); if unresolved or
refuted, escalate via [Parent-Owned
Recovery](../../shared/transition.md#parent-owned-recovery).

**On resume after repair:** re-pin the candidate, inspect the repair range
(`<old-HEAD>..<new-HEAD>`) and repair `decisions`, and rerun only failed,
invalidated, and not-yet-run required checks. If review has not run yet, go to
Step 3; otherwise request a bounded **delta recheck** passing repair range,
closed findings, repair `decisions`, and fresh receipts (or fresh review /
delta-scoped fresh reviewer under [Review](../../shared/review.md#lifecycle)).

### 5. Emit Completion Result

Return [Completion
Result](../../interfaces/acceptance-result-v1.md#completion-result) only when
every gate holds:

- [ ] Every active `Observable claim` (with `superseded at assembly by
  T<contract>` closed via `T<contract>`), `Completion` criterion, and locally
  runnable `unverified` entry has passing proof at its required boundary (only
  unavailable *optional* proof may remain a named gap; missing *required* proof
  blocks `Accepted`).
- [ ] Every material behavior change in the candidate diff serves an accepted
  obligation.
- [ ] All blocking check and verified review findings are closed and any
  selected review returned `PASS` (or `not_required` recorded with checked
  signals).
- [ ] Any explicitly requested external outcome (CI, deployment, runtime
  observation) is directly observed (otherwise retain local evidence while
  keeping Completion open or returning `Blocked` if unreachable).

## Conditional Branches

- **Runtime or fixture scenarios:** when explicitly required, run the smallest
  end-to-end scenario covering setup, behavior, observation, and bounded
  cleanup first, reusing valid prepared environments.
- **Long checks and resource waits:** record a checkpoint in execution state
  and inspect logs, processes, or lock owners in the background so delay causes
  stay visible.
