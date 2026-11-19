# Task Review / Readiness

Use when [Review](../shared/review.md) routes a fixed Planning result (a draft
ledger or no-implementation disposition) or on an explicit standalone readiness
review. Run statically over repository code, targeted `Cargo.lock` sources, and
accepted artifacts—no builds, tests, or services. Stay read-only: return
anchored findings grouped by falsifier (1–6) with their smallest owner in
[Review Result V1](../interfaces/review-result-v1.md) without reslicing,
rewriting packets, or inventing decisions.

For a no-implementation result, jump directly to [Verdict](#verdict).

Treat upstream prose and packet `Sources` as claims to verify against primary
source, never as evidence. Record all four reconstructions in
`evidence_boundary`:

- Obligation & `Completion` map checked (`R<n>`, `G<n>`, `Downstream handoff`,
  rollout gates, durable contract/decision docs, `Planning hooks`);
- Independently reconstructed initial Ready Frontier, concurrently eligible
  pairs, and shared/exhaustive surfaces checked;
- Concrete workspace symbols, caller searches, and non-obvious `Cargo.lock`
  crate sources inspected (plus uncached crates);
- Cold-start / producer-isolation and base-commit falsifiability checks run.

## Adversarial Falsifiers

Run Tier 1 before Tier 2. Stop any broken path at its earliest defect while
continuing independent paths so one pass surfaces all material findings. Supply
a concrete witness (task/edge, failure scenario, exact anchor) for scenario
findings.

### Tier 1 — Graph, Slicing & Concurrency

1. **Coverage, `Completion`, scope creep & deletion.**
   - Verify `spec.md`, plus `design/design.md` and `rollout.md` when present,
     are `status: ready`, then map `intent.md`, `spec.md`, `design/`, and
     `rollout.md` independently: `FAIL` if every task can satisfy its `Outcome`
     and `Observable claim` yet an accepted `R<n>`, `G<n>`, `Downstream
     handoff`, rollout gate, `Carried concerns`, `Planning hooks` risk, or
     required durable contract or decision-doc update still fails.
   - Check `tasks.md`'s `Completion`: `FAIL` if tautological or missing
     cross-slice invariants or explicitly requested external outcomes.
   - Trace each task's `Outcome` and `Boundary -> In scope` to an accepted
     obligation (`FAIL` on scope creep or invented behavior/architecture), and
     test whether deleting or merging any task preserves all obligations,
     smart-zone fit, and genuine parallelism.
2. **Slicing, shape & granularity.**
   - Check every task against [Task Shapes And
     Granularity](planning.md#task-shapes-and-granularity) and [Conditional
     Planning Branches](planning/conditional-branches.md).
   - `FAIL` on mixed independent outcomes, horizontal layers (including
     standalone docs/cleanup tails or a `shared-contract` without ≥2
     lock-disjoint parallel consumers), false parallelism (tasks split solely
     for concurrency that serialize on a shared lock), un-gated
     `integration-first` dependents, or collapsed deploy-separated `rollout.md`
     phases in one ledger.
   - *Short-circuit:* if slicing fails, skip Tier 2 cold-start dry runs on
     malformed packets while still reporting upstream gaps or source
     contradictions found in the same pass.
3. **DAG, Rust compile edges & concurrent collisions.**
   - Verify an acyclic DAG, non-empty Ready Frontier, topological + priority
     order, and all code tasks reaching `Implemented` before final validation.
   - Verify every edge matches [Task Ledger
     V1](../interfaces/task-ledger-v1.md#authoring-schema-planning-owned)
     (earliest consuming stage, keeping coding edges for Rust compile-time
     dependencies and risk-retirement gates).
   - Reconstruct the Ready Frontier and every concurrently eligible pair;
     `FAIL` on undeclared collisions across `Mutable owners`, `Exclusive
     locks`, shared wiring files, exhaustive public workspace enums/structs,
     inconsistent lock paths, or unpinned shared identifiers.

### Tier 2 — Primary-Source Truth, Cold-Start & Proof Discipline

4. **Primary-source truth & blast radius.**
   - Inspect workspace code for touched symbols, callers, generated contracts,
     migrations, manifests, and superseded paths, plus `Cargo.lock` sources for
     **non-obvious, unanchored, or slicing-critical** external crate APIs
     (signatures, trait bounds, semantics).
   - Confirm every blast-radius caller is proved unchanged, updated inside the
     mutating task (or via additive `expand -> migrate -> contract`) when
     broken by a signature or exhaustive-type change, or compiles as a new
     consumer behind `Depends on`. `FAIL` on missing anchors or source
     contradictions (an uncached crate flagged concretely in `Reopen if` is
     `CONCERNS`).
5. **Zero-history cold-start, producer isolation & parity.**
   - Walk every task from `tasks.md`, its packet, cited `Sources`, and primary
     code alone—zero chat history.
   - `FAIL` if two fresh Leads could build incompatible interfaces, names,
     placements, or edge-case behavior, or if reading a producer packet alone
     omits any type, signature, error mode, export, or schema invariant a
     dependent's `Depends on` assumes.
   - Verify exact ID, non-default shape tag, and stage-tag parity between
     `tasks.md` and packets, zero orphan files under `specs/<task>/tasks/`,
     migration of active `workflow-plan.md` rules and `Carried concerns`, and
     propagation of applicable upstream `Assumptions and reopen triggers` into
     `Reopen if`.
6. **Base-commit falsifiability & proof boundary.**
   - Check each task's `Outcome`, `Observable claim`, and `tasks.md`'s
     `Completion` against current code and [Task Packet
     V1](../interfaces/task-packet-v1.md#field-rules).
   - `FAIL` if a claim holds on the base commit outside its shape rule, passes
     on a stub or partial wiring, requires unlanded downstream/sibling code,
     omits its seam, or prescribes tests, fixtures, proving layers, commands,
     or unanchored `Additional proof`.

## Verdict

- **Draft ledger:**
  - `PASS`: all six falsifiers clear with all four `evidence_boundary`
    reconstructions recorded, fresh Leads can start from the Ready Frontier and
    build compatible code concurrently without collisions or invention, and
    Delivery Owner can falsify every obligation at assembly. Never block `PASS`
    on absent test cases/commands, optional infrastructure, routine
    Implementation choices, semantic `Mutable owners` without file lists, a
    1-task ledger fitting one smart zone, or equivalent valid decompositions.
  - `CONCERNS`: only a bounded later risk consistent with readiness (such as an
    uncached crate noted in `Reopen if`); name the risk, affected task, and
    where Planning must carry it (`Global constraints` or `Reopen if`—never
    `Final validation`) before setting `status: ready`.
  - `FAIL`: any surviving falsifier; anchor each finding to its task,
    obligation (`R<n>`/`G<n>`), or primary source and name the smallest repair
    or reopen owner (`Planning`, `Technical Design`, `Specification`, `Intake`,
    or external authority).
- **No-implementation result:** `PASS` requires primary-source evidence proving
  every obligation already holds or an authorized scope exit, with owners and
  stages named for any outstanding proof or external effects; otherwise `FAIL`.
