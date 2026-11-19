# Task Review / Readiness

Use when [Review](../shared/review.md) routes a fixed Planning result (a draft
ledger or no-implementation disposition) or on an explicit standalone readiness
review. Run statically over repository and locked-dependency sources—no builds,
tests, or services. Stay read-only: report anchored findings and their smallest
owner without reslicing, rewriting packets, or inventing missing decisions.

For a no-implementation result, jump directly to [Verdict](#verdict).

## Adversarial Falsifiers

Run every falsifier below against the draft ledger and its packets. Stop any
broken path at its earliest missing choice or source contradiction while
continuing independent paths so one review pass surfaces all material defects.

1. **Coverage & deletion.** Construct a scenario where every task meets its
   `Outcome` and `Observable claim` yet an accepted obligation in `intent.md`,
   `spec.md`, `design/`, or `rollout.md` (or a preserved consumer guarantee)
   still fails. Conversely, test whether deleting or merging any task leaves all
   obligations and prerequisites satisfied.
2. **Slicing & granularity.** Verify each packet fits one valid shape (vertical
   slice / tracer bullet, authorized prefactoring / shared seam, integration-first
   slice, or expand–migrate–contract) and one fresh Lead session. `FAIL` if a
   packet mixes independent outcomes or cuts a single feature into a horizontal
   layer that cannot be consumed alone. If slicing fails, skip detailed dry-runs
   of malformed packets, but still report upstream gaps or source contradictions
   found in the same pass.
3. **Base-commit falsifiability (anti-vacuity).** Check each `Outcome` and
   `Observable claim` against current code. `FAIL` if it already holds on the
   base commit, cannot distinguish accepted behavior from a plausible incomplete
   change, requires another task's unlanded code to become meaningful, or
   restates the prompt without a concrete observable delta (or removal/prefactor
   invariant per [Task Packet V1](../interfaces/task-packet-v1.md#field-rules)).
4. **Cold-start execution & source truth.** Walk every task from `tasks.md`, its
   packet, cited `Sources`, and current code alone—zero chat history. Confirm
   every cited anchor exists, every consumed symbol, config key, and
   `Cargo.lock`-locked crate API matches actual source (reusing shared checks),
   and a fresh Lead needs no guesswork on behavior, mechanism, or placement.
   Check canonical handoff parity: `tasks.md` and packets match on IDs, `Depends
   on` stage tags, and `Provides`; no orphan packets exist; active instructions
   from `workflow-plan.md` (when present) appear in `Global constraints`.
5. **DAG, blast radius & lock collisions.** Verify an acyclic path where all
   tasks reach `Implemented` before final validation and every dependency uses
   its earliest consuming stage. Falsify each changed contract against its
   **blast radius** (callers, consumers, generated schemas, migrations, removed
   paths): every companion sits inside the unit, behind a valid intermediate
   dependency, or is proved unchanged. Independently reconstruct the Ready
   Frontier (including tasks sharing a free lock) and all concurrently eligible
   task pairs; `FAIL` on frontier mismatch or undeclared collisions across
   `Mutable owners` or shared `Exclusive locks`.
6. **Boundary & proof discipline.** Confirm no packet invents behavior or
   architecture, prescribes test cases, fixtures, proving layers, or commands,
   or adds `Additional proof` without an accepted source anchor.

## Verdict

- **Draft ledger:**
  - `PASS`: all six falsifiers clear, at least one task is on the Ready
    Frontier, the plan's frontier matches the tasks the reviewer can start
    individually from source-verified inputs without chat history, and shared
    locks are declared accurately. Absent test cases/commands or optional
    integration infrastructure never block readiness.
  - `CONCERNS`: only a bounded later risk consistent with readiness (including
    an uncached crate check noted in `Reopen if`); name the risk, task, and
    where Planning must carry it before setting `status: ready`.
  - `FAIL`: any surviving falsifier; anchor each finding and name the smallest
    repair or upstream reopen owner.
- **No-implementation result:** `PASS` requires current evidence that every
  obligation is already met or an authorized scope exit, with owners named for
  any outstanding proof or external effects; otherwise `FAIL`.
