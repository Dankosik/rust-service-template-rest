# Final Validation

Use as Delivery Owner (`ACCEPTANCE_UNIT_LEAD`) once every planned unit is
`Implemented` and assembled. Prove the fixed candidate against the accepted
outcome and return one [Completion
Result](../../interfaces/acceptance-result-v1.md#completion-result) — or one
batched [Repair Request](../../interfaces/acceptance-result-v1.md#repair-request)
for in-scope defects.

**Read-only in a ledger:** leave candidate files untouched; inspect diffs, run
checks, and dispatch review, routing every code, test, or generated-file edit
through `NeedsRepair`. For `no-commit` or inline-unit overrides, follow
[Execution Modes](modes.md).

## Steps

### 1. Pin Candidate, Select Review Mode, And Run Mechanical Baseline

- Pin `<Base>..HEAD` (or brief's diff command) and `git status`, confirming
  zero active writers and no stray untracked files (`specs/<task>/` stays
  untracked until [Close](../implementation.md#5-close)).
- Read `tasks.md` (`Completion`, `Global constraints`, task `decisions`, and
  `unverified`), each packet's `Final validation`, and `git diff --stat`.
- **Select review mode early** under [Review
  Trigger](../../shared/review.md#trigger) so Step 2 stays inside your smart
  zone without duplicating Full Review's diff audit.
- Pick one non-overlapping plan from [Validation
  budget](../../../../AGENTS.md#validation-budget); on mixed surfaces, run
  read-only `BASE_REF=<Base> make plan` (ignoring `docs-check` links only when
  triggered solely by untracked `specs/<task>/`) and load [Validation
  Routing](../../../validation-routing.md) when policy rules apply. Load
  [Evidence Contract](../../shared/evidence-contract.md) only for reuse,
  additional required proof, or unavailable infrastructure.
- Run mechanical checks (`fmt-check`, `lint`, `build`, relevant tests,
  generated-contract and surface checks). Run assembled checks across every
  seam touched by multiple units, count only executed checks with **non-zero**
  test counts observing the claimed boundary, and record unavailable *optional*
  infrastructure as a limitation without provisioning ad-hoc stacks.

If build or lint blocks tests, go straight to Step 4; otherwise finish tests
and continue to Step 2 so check failures and proof gaps batch into one `Repair
Request`.

### 2. Verify Proof Completeness (And Diff Trace When Unreviewed)

- **Always (specific proof mapping):** map every active `Observable claim`
  (`superseded at assembly by T<contract>` closes via `T<contract>` and
  `Completion`), `Completion` criterion, preserved `G<n>`, and `unverified`
  entry to the **specific executed test name(s) or command output** observing
  its claimed seam (a blanket suite total like `make test: N passed` without
  naming covering tests is not proof). Run locally runnable `unverified` tests
  now, verify removals make old behavior unreachable, and compare task
  `decisions` for contradictions across shared seams or locks.
- **When Full Review will run:** stop Step 2 here — leave hunk-by-hunk semantic
  diff auditing to fresh `reviewer-agent`(s) in Step 3 so you keep your smart
  zone lean and stay neutral when verifying citations in Step 4.
- **When Direct Completion or Structure-Only Review applies:** also read
  accepted sources (`intent.md`, `spec.md`, `design/`, `rollout.md`) and the
  candidate diff to trace every material hunk back to an accepted obligation,
  verify cross-unit seams, and spot-check added tests against tautologies.

Done when every obligation, `unverified` entry, and (when unreviewed) material
hunk maps to a named passing check, an unavailable *optional* gap, or a
concrete defect. Fix check failures and missing required proof first; dispatch
review only on a green, proof-complete candidate.

### 3. Freeze Candidate And Run Selected Review

- Re-evaluate [Review Trigger](../../shared/review.md#trigger) if a repair
  landed before initial review. If **Direct completion** applies, record
  `review: not_required (<checked trigger signals: none matched>)` and go to
  Step 5.
- Otherwise freeze the candidate and dispatch `reviewer-agent` (tier per
  harness adapter) under [Implementation Review](../implementation-review.md)
  with mode, pinned diff command and commit list, accepted bundle paths
  (`specs/<task>/`), and current check receipts — omitting builder or Delivery
  Owner narrative justifications or self-review claims.

| Review Return | Action |
| --- | --- |
| `PASS` (both lenses `PASS` in a 2-lens panel) | Go to Step 5. |
| `FAIL` or `NEEDS_PARENT` | Hold 2-lens reports verbatim by axis without reranking (dispatch `adjudicator-agent` only for an evidence-backed conflict between lenses). Go to Step 4. |
| `REVIEW_HANDOFF_INVALID` | Repair brief or diff boundary and re-dispatch without consuming a repair round (or return `Blocked` if candidate diff is genuinely empty). |

### 4. Verify Findings, Classify, And Route Repairs

**Verify before routing:** read the cited code anchor and surrounding callers
for every review finding before acting; dismiss unanchored or factually refuted
claims with their refuting anchor (or escalate contested claims via
`adjudicator-agent`). If zero blocking findings survive, record those anchors
in `review` and go directly to Step 5 without editing code.

Batch surviving failures and verified findings under [Review Result
V1](../../interfaces/review-result-v1.md):

| Classification | Action |
| --- | --- |
| `TASK_DEFECT` or `INTEGRATION_DEFECT` | Stop active readers, release validation locks, and return **one batched** [Repair Request](../../interfaces/acceptance-result-v1.md#repair-request) (`verdict: NeedsRepair`) naming all surviving findings, owning tasks (both seam tasks for `INTEGRATION_DEFECT`), `retained_evidence`, and `prior_attempt` on Round 2. |
| `environment` (optional) | Record the material gap in `evidence` without holding acceptance. |
| `environment` (required), `UPSTREAM_GAP`, or `NEEDS_PARENT` | Run a missing local check directly if inside authority (read-only); otherwise apply [Evidence Contract](../../shared/evidence-contract.md#required-and-optional-proof) and return `Blocked` naming claim and owner. |

Follow the **2-repair-round cap** and escalation rules in [Review
Lifecycle](../../shared/review.md#lifecycle); escalate via [Parent-Owned
Recovery](../../shared/transition.md#parent-owned-recovery) if unresolved.

**On resume after repair:**

- **All findings refuted (`Blocked: refuted findings`):** inspect the refuting
  anchor directly; if verified with zero surviving blockers, record the anchor
  in `review` and continue (Step 5, or Step 2/3 if initial validation or review
  has not run); otherwise escalate via `adjudicator-agent` or Parent-Owned
  Recovery.
- **Repair landed (`Implemented`):** re-pin candidate, inspect the repair diff
  (`<old-HEAD>..<new-HEAD>`) and repair `decisions`, and follow in order:
  1. **Resolve any partial refutations in repair `decisions`:** inspect each
     refuting anchor directly — dismiss verified refutations (removing them
     from open findings passed to `Delta Recheck`), stay in Step 4 and batch
     for Round 2 (within the 2-round cap) if a refutation fails and the defect
     still holds, or escalate genuinely contested claims via `adjudicator-agent`.
  2. **Rerun checks:** rerun failed, invalidated, and not-yet-run required
     checks; if any fail, stay in Step 4 and batch within the 2-round cap.
  3. **Refresh proof mapping:** if Step 2 has not completed or the repair
     invalidated proof or seams, complete Step 2's proof check first (staying
     in Step 4 if any gap survives).
  4. **Route review:** if initial review has not run, go to Step 3; otherwise
     dispatch one fresh `reviewer-agent` in [Delta
     Recheck](../implementation-review.md#delta-recheck) mode (passing repair
     diff, repaired/open findings, verified partial-refutation anchors, and
     repair `decisions`) — or a fresh **full** review via Step 3 if the repair
     expanded scope, interfaces, or risk surface under [Review
     Lifecycle](../../shared/review.md#lifecycle) — routing per Step 3's table.

### 5. Emit Completion Result

Return [Completion
Result](../../interfaces/acceptance-result-v1.md#completion-result) (`Accepted`)
only when every gate holds:

- [ ] Every active `Observable claim` (`superseded at assembly by T<contract>`
  closed via `T<contract>`), `Completion` criterion, and locally runnable
  `unverified` entry maps to a named passing executed check at its required
  boundary (only unavailable *optional* proof remains a named gap).
- [ ] Every material behavior change in the candidate diff serves an accepted
  obligation.
- [ ] All blocking check and verified review findings are closed and any
  selected review returned `PASS` (or all blocking findings were dismissed with
  primary-source refutation/adjudication, or `not_required` with checked
  signals).
- [ ] Every external outcome explicitly requested in `Completion` (CI,
  deployment, runtime observation) is directly observed. Unrequested CI/`main`
  checks remain pending notes under `Accepted`; if a requested external outcome
  is unreachable or awaits authorization, return `Blocked` with passing local
  results in `evidence` and the exact external gate and owner in `next_owner`
  (never invent a third verdict).
