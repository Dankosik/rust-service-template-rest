# T1 — Every admitted attempt retains bounded, observable lifecycle custody

Outcome:
Pending-handler destruction can no longer silently abandon attempt custody;
unexpected supervisor retirement reaches the existing engine/process failure
path, while admission and completion membership remain bounded and observable.

Consumes:
- [Specification](../spec.md), R1 and R4 ownership observation — required behavior and preserved outcome precedence.
- [System design](../design/system.md#r1-attempt-containment-and-failure-custody) and [R4](../design/system.md#r4-fixed-workload-budget-and-observation) — single-task containment, armed guard and two label-free owner gauges.
- [Ownership](../design/ownership.md#files) — existing attempt, claim, engine and conditional worker readers.

Provides:
- Hardened existing attempt lifecycle and actual-owner metrics for T2's measurement binding; changed runtime paths form T2's explicit baseline-to-candidate adoption patch.
- Narrow regressions at current owner-local and jobs/process proof surfaces, retaining adequate existing cases.

Boundary:
Implement the selected safe handler owner and pre-submission guard in existing
custody; retain tracker, scheduling, fencing, budgets, cancellation precedence,
sticky failure semantics and sanitized diagnostics. Include both new ownership
gauges here because their correct transitions share this lifecycle owner.
Repair worker failure readers only for a demonstrated gap. No new executor,
retry policy, task registry, settings, dependencies or schema. No proactive
preparation/first-poll policy change. Reference and guide work belongs to T2.

Mutable owners:
- `crates/infra-jobs`: attempt/engine/claim implementation, local tests and associated Rust API documentation only.
- `crates/jobs-worker/src/bootstrap.rs` or `src/shutdown.rs` only for a source-confirmed existing failure-reader gap and its scoped tests.
- `test/tests/jobs/execution.rs` and `test/tests/jobs/process.rs` for missing R1 regressions; no fixture manifest or shared scenario driver ownership.

Exclusive locks:
- The production attempt/engine/claim custody and worker failure-reader surfaces above; no shared manifest, migration chain or guide mutation.

Final validation:
- Claim: R1's known, cancellation and uncertainty outcomes retain their accepted distinctions; handler panic containment differs from supervisor failure; ownership retires without an orphan or unbounded result collection.
- Checks: Matching build/tests and required real-PG/process proof under the ledger's single final boundary. Reuse existing fenced/unknown/worker evidence where unchanged; Implementation selects concrete missing cases and records commands. Final independent review covers lifecycle/concurrency invariants.
- Observable: Sanitized handler destruction retains timeout/force disposition, ready/success precedence survives, unexpected supervisor loss becomes the existing primary/degraded failure, valid subsequent work proceeds after contained panic, and both actual ownership gauges return to idle within admitted bounds.

Reopen if:
An unavailable mechanism, deadline/precedence change, new public capability or
unowned failure reader is necessary: Technical Design, then Specification only
if meaning changes. A concrete neighboring defect is evidence, not permission
for a general runtime refactor.
