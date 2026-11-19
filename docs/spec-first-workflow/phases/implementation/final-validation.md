# Final Validation

Use as Delivery Owner once every planned unit is `Implemented` and assembled.
Prove the fixed candidate against the accepted outcome and return one
[Completion Result](../../interfaces/acceptance-result-v1.md#completion-result)
— or one batched [Repair
Request](../../interfaces/acceptance-result-v1.md#repair-request) for in-scope
defects.

**Read-only in a ledger:** keep candidate files untouched; inspect code/diffs,
run checks, and dispatch review, routing all code, test, or generated-file
edits through `NeedsRepair` (only a root-local inline Lead edits directly).

## Steps

### 1. Pin Candidate, Select Review Mode, And Run Mechanical Baseline

- Pin `<Base>..HEAD` (or `git diff <Base>` in `no-commit` mode) and `git
  status`, confirming zero active writers and no stray untracked files
  (`specs/<task>/` stays untracked until [Close](../implementation.md#5-close)).
- Read `tasks.md` (`Completion`, `Global constraints`, task `decisions` and
  `unverified`), each packet's `Final validation`, and `git diff --stat`.
- **Select review mode early** under [Review
  Trigger](../../shared/review.md#trigger) so Step 2 stays inside your `smart
  zone` without duplicating Full Review's diff audit.
- Select one non-overlapping plan from [Validation
  budget](../../../../AGENTS.md#validation-budget); on mixed surfaces, run
  read-only `BASE_REF=<Base> make plan` (ignoring only the `links` portion of
  `make docs-check` when triggered solely by untracked `specs/<task>/`, while
  preserving `DOCS_CHECK_PART=image-inputs` and tracked-doc checks) and load
  [Validation Routing](../../../validation-routing.md) when policy rules apply.
  Load [Evidence Contract](../../shared/evidence-contract.md) only for reuse,
  additional required proof, or unavailable infrastructure.
- Run mechanical checks (`fmt-check`, `lint`, `build`, relevant tests,
  generated-contract and surface checks). **Proof bar:** count only executed
  checks with **non-zero** test counts observing the claimed boundary; never
  reuse pre-assembly Lead results across seams touched by another unit; record
  unavailable *optional* infrastructure as a limitation without fabricating
  green or provisioning ad-hoc stacks.

If build or lint blocks tests, go straight to Step 4; otherwise finish tests
and continue to Step 2 so check failures and proof gaps batch into one `Repair
Request`.

### 2. Verify Proof Completeness (And Diff Trace When Unreviewed)

- **Always (proof completeness):** map every active `Observable claim`
  (treating `superseded at assembly by T<contract>` as closed by `T<contract>`
  and `Completion`), `Completion` criterion, preserved `G<n>`, and `unverified`
  entry to a **current passing executed check** with non-zero count (running
  locally runnable `unverified` tests now). Verify removals make old behavior
  unreachable and compare task `decisions` for contradictions across shared
  seams or locks.
- **When Full Review will run:** stop Step 2 here — leave hunk-by-hunk semantic
  diff auditing to fresh `reviewer-agent`(s) in Step 3 so you keep your `smart
  zone` lean and stay neutral when verifying citations in Step 4.
- **When Direct Completion or Structure-Only Review applies:** also read
  primary sources (`intent.md`, `spec.md`, `design/`, `rollout.md`) and the
  candidate diff to trace every material hunk back to an accepted obligation,
  verify cross-unit seams, and spot-check added tests against tautologies.

Complete this step only when every obligation, `unverified` entry, and (when
unreviewed) material hunk maps to a passing check, an unavailable *optional*
gap, or a concrete defect. If any required check failed or any defect or
missing required coverage survived Steps 1–2, go to Step 4 — never dispatch
review on a failing candidate.

### 3. Freeze Candidate And Run Selected Review

- Re-evaluate [Review Trigger](../../shared/review.md#trigger) if a repair
  landed before initial review. If **Direct completion** applies, record
  `review: not_required (<checked trigger signals: none matched>)` and go to
  Step 5.
- Otherwise freeze the candidate and dispatch `reviewer-agent` (tier per
  harness adapter) under [Implementation Review](../implementation-review.md)
  with:
  - mode (`Full — 4 axes`, `Full — Lens A (Axes 1+4)` / `Lens B (Axes 2+3)`,
    `Structure-Only` + triggering additions, or `Delta Recheck` per
    [Delta Recheck](../implementation-review.md#delta-recheck))
  - pinned diff command (`git diff <Base>..HEAD`, `git diff <Base>`, or the
    bounded repair diff for `Delta Recheck`) and commit list
  - present accepted sources (`intent.md`, `spec.md`, `design/`, `rollout.md`,
    `tasks.md`, packets) and current check receipts
- On 2-lens return, report both verbatim by axis without reranking or masking
  any `FAIL`, `NEEDS_PARENT`, or `REVIEW_HANDOFF_INVALID` (dispatch
  `adjudicator-agent` only for an evidence-backed conflict between lenses).
- Route the verdict: on `REVIEW_HANDOFF_INVALID`, repair the brief or diff
  boundary and re-dispatch without consuming a repair round (or return
  `Blocked` if the candidate diff is genuinely empty); on `FAIL` or
  `NEEDS_PARENT`, go to Step 4; on `PASS`, go to Step 5.

### 4. Verify Findings, Classify, And Route Repairs

**Verify before routing:** read the cited code anchor and surrounding callers
for every review finding before acting; dismiss unanchored or factually refuted
claims with their refuting anchor (or escalate contested claims via
`adjudicator-agent`). If every blocking review finding is dismissed with
primary-source proof (or resolved with zero surviving blockers via
`adjudicator-agent`), record those refutation/adjudication anchors in `review`
and proceed directly to Step 5 without editing code.

Batch surviving failures and verified findings under [Review Result
V1](../../interfaces/review-result-v1.md):

| Classification | Action |
| --- | --- |
| `TASK_DEFECT` or `INTEGRATION_DEFECT` | Stop active readers and release validation locks. In a ledger, return **one batched** [Repair Request](../../interfaces/acceptance-result-v1.md#repair-request) (`verdict: NeedsRepair`) naming all surviving findings, owning tasks (both seam tasks for `INTEGRATION_DEFECT`), and `retained_evidence`. Root-local Leads repair under [Repair Assignment](repair.md). |
| `environment` (optional) | Record the material gap in `evidence` without holding acceptance. |
| `environment` (required), `UPSTREAM_GAP`, or `NEEDS_PARENT` | Run a missing local check directly if inside authority (read-only); otherwise apply [Evidence Contract](../../shared/evidence-contract.md#required-and-optional-proof) and return `Blocked` naming claim and owner. |

Follow the **2-repair-round cap** and recheck escalation rules in [Review
Lifecycle](../../shared/review.md#lifecycle); escalate via [Parent-Owned
Recovery](../../shared/transition.md#parent-owned-recovery) if unresolved.

**On resume after repair:**

- **Repair Lead `Blocked` (`refuted findings`):** inspect the refuting anchor
  directly; if verified and zero blockers survive, record the refutation anchor
  in `review` and go to Step 5 (or Step 2/3 if initial validation/review has
  not completed); otherwise escalate via `adjudicator-agent` or Parent-Owned
  Recovery.
- **Repair landed:** re-pin candidate, inspect the repair diff
  (`<old-HEAD>..<new-HEAD>`, or `git diff <Base> -- <repaired paths>` in
  `no-commit` / inline mode) and repair `decisions`, and follow in order:
  1. Rerun failed, invalidated, and not-yet-run required checks; if any fail,
     stay in Step 4 and batch the failure (respecting the 2-round cap).
  2. If Step 2 has not completed yet or the repair invalidated proof/seams,
     complete Step 2's proof/trace check first (staying in Step 4 if any gap
     survives).
  3. Route review: if initial review has not run yet, go to Step 3; otherwise
     dispatch one fresh `reviewer-agent` in [Delta
     Recheck](../implementation-review.md#delta-recheck) mode — or a fresh
     **full** review via Step 3 if the repair expanded scope, interfaces, or
     risk surface under [Review Lifecycle](../../shared/review.md#lifecycle) —
     routing its verdict identically to Step 3 (`PASS` -> Step 5,
     `FAIL`/`NEEDS_PARENT` -> Step 4, `REVIEW_HANDOFF_INVALID` -> fix handoff
     and re-dispatch).

### 5. Emit Completion Result

Return [Completion
Result](../../interfaces/acceptance-result-v1.md#completion-result) (`Accepted`)
only when every gate holds:

- [ ] Every active `Observable claim` (with `superseded at assembly by
  T<contract>` closed via `T<contract>`), `Completion` criterion, and locally
  runnable `unverified` entry has passing proof at its required boundary (only
  unavailable *optional* proof may remain a named gap).
- [ ] Every material behavior change in the candidate diff serves an accepted
  obligation.
- [ ] All blocking check and verified review findings are closed and any
  selected review returned `PASS` (or had all blocking findings dismissed with
  primary-source refutation/adjudication, or `not_required` with checked
  signals).
- [ ] Any explicitly requested external outcome (CI, deployment, runtime
  observation) is directly observed; if unreachable or awaiting external
  authorization, return `Blocked` with passing local results in `evidence` and
  the exact external gate and owner in `next_owner` (never invent a third
  verdict).
