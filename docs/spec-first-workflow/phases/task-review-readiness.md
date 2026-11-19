# Task Review / Readiness

Use when [Review](../shared/review.md) routes a fixed Planning result (a draft
ledger or no-implementation disposition) or on an explicit standalone readiness
review. Run statically over repository code, targeted `Cargo.lock` sources, and
accepted artifacts — no builds, tests, or services. Stay read-only and return
[Review Result V1](../interfaces/review-result-v1.md) with classification
(`PHASE_DEFECT`, `UPSTREAM_GAP`, or `CONCERN`) and smallest owner.

For a no-implementation result (1 fresh `reviewer-agent`), jump directly to
[No-Implementation Result](#no-implementation-result).

## Axes *(Draft Ledger)*

- **Default (1 fresh `reviewer-agent`):** 1–2 task ledgers with `≤2` mutated
  crates and no `rollout.md` or wide refactor — run all six falsifiers.
- **Two-axis parallel review (2 fresh `reviewer-agent` lanes):** ledgers with
  `≥3` tasks, `rollout.md`, wide refactor (`expand`/`migrate`/`contract`), or
  `≥3` mutated crates. Run strictly the assigned axis; review clears only when
  **both** axes clear (`PASS` or `CONCERNS`) without masking findings across
  axes (route one surviving evidence-backed conflict to `adjudicator-agent` per
  [Review](../shared/review.md#route)):
  - **Axis 1 — Obligations, Slicing & Falsifiability:** falsifiers **1, 2, 6**.
  - **Axis 2 — DAG, Source Truth, Locks & Cold-Start Isolation:** falsifiers
    **3, 4, 5**.

## Evidence Boundary And Reporting

Treat upstream prose, packet `Sources`, and Planning summaries as claims to
verify against primary source. Run `make ledger-check TASK=<task>` (`python3
scripts/verify/check_ledger.py specs/<task>`) first (or verify its output on
the current candidate) so mechanical parity, DAG, orphan, and upstream-status
defects fail fast.

Report under explicit numbered headings for **every assigned falsifier** (when
clear, write `none — attacked <witness, symbol, or edge>`), supplying a
concrete witness (`task/edge`, failure scenario, exact anchor) for every
finding. Record the assigned reconstructions in `evidence_boundary`:

- **Axis 1 / Full:** independent map of [Planning Step 1](planning.md#steps)
  obligations + `Completion`; per-task base-commit falsifying `path::Symbol`
  (or shape exception).
- **Axis 2 / Full:** `check_ledger.py` result + independently checked Ready
  Frontier, concurrent pairs, and shared/exhaustive surfaces; concrete
  workspace symbols, `--all-targets` caller searches, and `Cargo.lock` sources
  inspected (plus uncached crates); every `T_a -> T_b` edge checked for
  producer isolation.

## Adversarial Falsifiers

Stop a broken path at its earliest defect while continuing independent paths.

1. **Coverage, `Completion`, scope creep & deletion.**
   - Map `intent.md`, `spec.md`, `design/`, and `rollout.md` independently
     against [Planning Step 1](planning.md#steps): `FAIL` if every task can
     satisfy its claims yet any accepted `R<n>`, `G<n>`, `D<n>`, `S<n>`,
     `Downstream handoff`, `Superseded paths and generated contracts`,
     `Durable contract update` / `Durable doc updates`, rollout gate,
     `Planning hooks` risk (without a valid declined-hook rationale), or
     `owner: Planning` `Carried concern` remains unclosed.
   - `FAIL` on bare whole-file `spec.md`/`design.md` `Sources` in multi-task
     ledgers, tautological/incomplete `Completion`, scope creep, or redundant
     tasks failing the **deletion test**.
2. **Slicing, shape & granularity.**
   - Verify against [Task Shapes And
     Granularity](planning.md#task-shapes-and-granularity) and [Conditional
     Planning Branches](planning/conditional-branches.md).
   - `FAIL` on mixed independent outcomes, horizontal layers (including
     cleanup/docs tails or `shared-contract` without `≥2` lock-disjoint
     parallel consumers), false parallelism, **smart-zone overload (God Task
     spanning heterogeneous risk/gate or multi-subsystem boundaries)**,
     unisolated primary integration or blast-radius risk, un-gated
     `integration-first` dependents, or collapsed deploy-separated `rollout.md`
     phases.
   - *Short-circuit:* in single-reviewer mode, skip cold-start polishing on
     malformed packets if slicing fails, while still reporting upstream gaps or
     source contradictions found in the same pass.
3. **DAG, Rust compile edges & concurrent collisions.**
   - Confirm `check_ledger.py` passes and every edge follows [Task Ledger
     V1](../interfaces/task-ledger-v1.md#authoring-schema-planning-owned).
   - Independently audit every concurrently eligible pair; `FAIL` on undeclared
     collisions across `Mutable owners`, [Task Packet
     V1](../interfaces/task-packet-v1.md#field-rules)'s four `Exclusive locks`
     categories (including parent `lib.rs`/`mod.rs`), exhaustive public
     workspace enums/structs, or unpinned shared identifiers.
4. **Primary-source truth & `--all-targets` blast radius.**
   - Inspect workspace code, `quality/architecture.json`,
     `scripts/template/profiles.json`, manifests, generated contracts,
     migrations, superseded paths, and targeted `Cargo.lock` sources for
     **non-obvious, unanchored, or slicing-critical** external APIs.
   - Confirm every broken production or test (`--all-targets`) caller sits
     inside the mutating task (or additive `expand -> migrate -> contract`),
     architecture/profile updates land with their slice, and new consumers
     compile behind `Depends on`. `FAIL` on missing anchors or source
     contradictions (uncached crate in `Reopen if` is `CONCERN`).
5. **Zero-history cold-start, producer isolation & propagation.**
   - Walk every task from `tasks.md`, packets, cited `Sources`, and primary
     code alone (zero chat history).
   - `FAIL` if two fresh Leads can build incompatible interfaces, names,
     placements, or edge behavior, if a producer packet alone omits any
     contract a dependent assumes, or if active `workflow-plan.md` rules,
     Implementation/Delivery Owner `Carried concerns`, or upstream
     `Assumptions and reopen triggers` were dropped.
6. **Base-commit falsifiability & proof boundary.**
   - Check each `Outcome`, `Observable claim`, and `Completion` against live
     code, [Task Packet V1](../interfaces/task-packet-v1.md#field-rules), and
     [Conditional Claim Rules](planning/conditional-branches.md#claim-rules).
   - `FAIL` if a claim holds on the base commit outside its shape rule, passes
     on a stub or partial wiring, requires unlanded sibling/downstream code,
     omits its seam, or prescribes tests, fixtures, proving layers, commands,
     or unanchored `Additional proof`.

## Delta Recheck

Falsify only the brief's anchored findings, edited packets/edges,
`check_ledger.py` parity/waves, and invalidated locks or producer contracts.
Require fresh review on reslice or changed outcomes/boundaries
([Review](../shared/review.md#lifecycle)).

## No-Implementation Result

Independently verify primary-source evidence proving every accepted obligation
already holds or an authorized scope exit applies, with owners and stages named
for any outstanding proof or external effects. Return **`PASS | FAIL` only**:

- `PASS`: primary evidence recorded in `evidence_boundary` proves zero
  implementation delta remains.
- `FAIL`: `PHASE_DEFECT` (`reopen_owner: Planning`) when implementation delta
  remains, or `UPSTREAM_GAP` (smallest upstream `reopen_owner`) on a missing
  decision or false premise.

## Blocking Bar And Verdict *(Draft Ledger)*

| Disposition | Bar |
| --- | --- |
| Blocking (`FAIL`) | Surviving falsifier classified as `PHASE_DEFECT` (`reopen_owner: Planning`) or `UPSTREAM_GAP` (`Technical Design`, `Specification`, `Intake`, or external authority), anchored to task, `R<n>`/`G<n>`/`D<n>`/`S<n>`, or primary source with concrete witness. |
| Bounded (`CONCERNS`) | `CONCERN` for a bounded later risk consistent with readiness (e.g., an uncached crate in `Reopen if`); name risk, affected task, and target home (`Global constraints` or `Reopen if` — never `Final validation`). |
| Non-blocking (never blocks `PASS`) | Absent test cases/commands, optional infrastructure, routine Implementation choices, semantic `Mutable owners` without file lists, a 1-task ledger fitting one smart zone, or equivalent valid decompositions. |

`PASS` when every assigned falsifier clears with its required
`evidence_boundary` reconstructions recorded.
