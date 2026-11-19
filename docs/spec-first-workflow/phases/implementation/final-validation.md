# Final Validation

As Delivery Owner prove the fixed assembled candidate. In a ledger this
assignment is read-only. Route candidate edits through a Repair Lead.
[Execution Modes](modes.md) supplies the inputs for inline and no-commit runs.

### 1. Pin Candidate, Select Review Mode, And Run Mechanical Baseline

Pin the brief's diff and candidate identity. Confirm writers have joined and
identify any unexpected working-tree changes before using receipts. Read
Completion, global constraints, packet final-validation claims, decisions,
unverified entries and the diff stat. Select the disposition under [Review
Trigger](../../shared/review.md#trigger).

Select the matching [Validation budget](../../../../AGENTS.md#validation-budget)
checks plus source-mandated additional proof. Apply [Execution
Evidence](../../shared/evidence-contract.md#execution-evidence) for receipt
reuse and invalidation, and [Validation Routing](../../../validation-routing.md)
for generated contracts or mixed surfaces. Attribute any docs-check failure to
its actual path before excluding an execution-only untracked bundle issue.
If a failed prerequisite prevents further checks, route it through Step 4 and
retain the unfinished checks.

### 2. Verify Proof Completeness (And Diff Trace When Unreviewed)

Map each active observable claim, preserved guarantee and local Completion
criterion to the specific executed test or command output observing its seam.
Resolve superseded claims through the named contract task and Completion.
Check cross-unit decisions and shared seams, verify withdrawn paths, and
reconcile every unverified entry. Run outstanding required local checks and
locally runnable deferred tests. Record unavailable optional proof at its
actual scope.

For direct completion or structure-only review, trace each material diff hunk
to the accepted inputs and spot-check changed tests for tautologies. In full
review the independent reviewers own this diff audit. Batch mechanical defects
and proof gaps in Step 4. Begin initial review only after required local proof
is complete.

### 3. Freeze Candidate And Run Selected Review

Re-evaluate the trigger on the fixed candidate after pre-review repair.
Record direct completion with the checked trigger signals. Otherwise apply
[Review Lifecycle](../../shared/review.md#lifecycle) and dispatch the selected
[Implementation Review](../implementation-review.md) with its mode, pinned
diff, accepted sources and check receipts. Apply the adapter's panel threshold.
Route findings through Step 4. For an invalid handoff correct the boundary
and redispatch without consuming a repair round. If a required diff is
genuinely empty, return the missing decision to its owner.

### 4. Verify Findings, Classify, And Route Repairs

Verify each cited finding against the primary anchor and surrounding callers.
Record the exact refutation for a dismissed claim. Preserve panel findings by
axis and use [Review Route](../../shared/review.md#route) for a surviving
material conflict. Classify findings under [Review Result V1](../../interfaces/review-result-v1.md).

- Batch confirmed task and integration defects into one [Repair
  Request](../../interfaces/acceptance-result-v1.md#repair-request) per pass.
  Stop readers and release validation locks before handing off. Include both
  owning units for a broken seam.
- For unavailable proof, apply [Required and Optional Proof](../../shared/evidence-contract.md#required-and-optional-proof).
  Return a required missing input, upstream gap or unresolved parent decision
  as `Blocked` with its owner and narrower evidence.
- On repair or refutation, verify the repair delta and each refuting anchor.
  Keep surviving defects open. Re-pin any changed candidate, reconcile
  failed, invalidated and unfinished checks in Step 1, and refresh affected
  proof mappings in Step 2. Complete any pending initial review in Step 3.
  After previously completed review apply Review Lifecycle for the bounded
  delta recheck, expanded full review and repair limit. Apply that limit to
  unresolved defects even across candidate revisions in the same repair cycle.

### 5. Emit Completion Result

Return [Completion Result](../../interfaces/acceptance-result-v1.md#completion-result)
as `Accepted` once Steps 1–4 close on the current candidate, including the
required review disposition and any verified refutations or adjudication.
Every explicitly requested external outcome also needs direct evidence and
the action's existing authority. While it is pending, return `Blocked` with
verified local scope and the exact external gate and owner. Unrequested CI or
runtime checks and material optional gaps stay scoped notes.
