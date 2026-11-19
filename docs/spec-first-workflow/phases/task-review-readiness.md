# Task Review / Readiness

Use when [Review](../shared/review.md) routes a fixed Planning result (a draft
ledger or no-implementation disposition) or on an explicit standalone readiness
review. Run statically over repository code, `Cargo.lock` crate sources, and
accepted artifacts—without builds, tests, or services. Stay read-only: return
anchored findings and their smallest owner in [Review Result
V1](../interfaces/review-result-v1.md) without reslicing, rewriting packets, or
inventing decisions.

For a no-implementation result, jump directly to [Verdict](#verdict).

Treat upstream prose and packet `Sources` annotations as claims to verify
against primary code and `Cargo.lock` sources, never as evidence. Record the
concrete symbols, caller searches, and locked crate sources actually inspected
(plus any uncached crates) in `evidence_boundary`.

## Adversarial Falsifiers

Run Tier 1 before Tier 2. Stop any broken path at its earliest defect while
continuing independent paths so one pass surfaces all material findings. Give
scenario findings a concrete witness (task/edge, failure scenario, exact
anchor).

### Tier 1 — Graph, Slicing & Concurrency

1. **Coverage, scope creep & deletion.** Independently check accepted inputs
   (`intent.md`, `spec.md`, `design/`, `rollout.md`). Construct a scenario
   where every task meets its `Outcome` and `Observable claim` yet an accepted
   `R<n>`, `G<n>`, `Downstream handoff`, rollout gate, `Carried concerns`, or
   required durable contract or decision-doc update still fails. Trace every
   task's `Outcome` and `Boundary -> In scope`
   back to an accepted obligation (`FAIL` on scope creep or invented behavior
   or architecture). Test whether deleting or merging any task preserves all
   obligations and prerequisites.
2. **Slicing, shape & granularity.** Check each task against [Task Shapes And
   Granularity](planning.md#task-shapes-and-granularity). `FAIL` on mixed
   independent outcomes, a horizontal layer split (including a "shared seam"
   with only one downstream slice), or false parallelism (tasks split solely
   for concurrency that serialize on a shared lock). *If slicing fails, skip
   Tier 2 cold-start dry runs on malformed packets while still reporting
   upstream gaps or source contradictions found in the same pass.*
3. **DAG, stage timing & concurrent collisions.** Confirm an acyclic DAG where
   all code tasks reach `Implemented` before final validation, every edge uses
   its earliest consuming stage (omitting coding edges where Design already
   settled a contract needing no upstream code), and the initial Ready Frontier
   is non-empty. Independently reconstruct the Ready Frontier (including tasks
   sharing a free lock) and every concurrently eligible task pair; `FAIL` on
   frontier mismatch, undeclared collisions across `Mutable owners`, `Exclusive
   locks`, or shared registration files, inconsistent lock paths, or unpinned
   shared identifiers (config keys, error codes, flags, wire names) that
   parallel Leads could invent divergently.

### Tier 2 — Primary-Source Truth, Cold-Start & Proof Discipline

4. **Source truth & blast radius.** Independently inspect current code and
   `Cargo.lock` crate sources for every distinct consumed symbol, config key,
   CLI/Make contract, and crate API—verifying **signatures and semantics**, not
   mere existence. Falsify each changed contract against its **blast radius**
   (callers, generated contracts, migrations, manifests, removed/superseded
   paths): every companion sits inside its unit, behind a valid compiling
   dependency, or is proved unchanged. `FAIL` on missing anchors or source
   contradictions (an uncached crate flagged concretely in `Reopen if` is
   `CONCERNS`).
5. **Cold-start self-sufficiency & index/packet parity.** Walk every task from
   `tasks.md`, its packet, cited `Sources`, and code alone—zero chat history.
   Apply the **two-Leads divergence probe**: `FAIL` if two competent fresh
   Leads could build incompatible interfaces, names, placements, or edge-case
   behavior, or if a producer's `Provides` omits what a dependent assumes.
   Verify exact ID, stage-tag, and `Provides` parity between `tasks.md` and
   packets, zero orphan packet files under `specs/<task>/tasks/`, and migration
   of active `workflow-plan.md` instructions and unresolved `Carried concerns`
   into `Global constraints` or affected packets' `Reopen if`.
6. **Base-commit falsifiability & proof boundary.** Check each `Outcome` and
   `Observable claim` against current code. `FAIL` if a claim already holds on
   the base commit (except non-regression `G<n>` invariants paired with a
   base-commit-failing delta, or removal/prefactor/expand invariants per [Task
   Packet V1](../interfaces/task-packet-v1.md#field-rules)), cannot distinguish
   accepted behavior from a plausible incomplete change, requires a downstream
   or sibling task's unlanded code, omits its public seam/entry point,
   prescribes test cases, fixtures, proving layers, or commands, or adds
   `Additional proof` without an accepted source anchor.

## Verdict

- **Draft ledger:**
  - `PASS`: all six falsifiers clear with primary-source inspection recorded in
    `evidence_boundary`, at least one task sits on the Ready Frontier, the
    plan's frontier matches the tasks the reviewer can start individually from
    source-verified inputs without chat history, and concurrent scopes and
    locks are declared accurately. Never block `PASS` on absent test
    cases/commands, optional integration infrastructure, routine coding choices
    owned by Implementation, or equivalent valid decompositions.
  - `CONCERNS`: only a bounded later risk consistent with readiness (such as an
    uncached crate noted in `Reopen if`); name the risk, affected task, and
    where Planning must carry it (`Global constraints` or `Reopen if`—never
    `Final validation`) before setting `status: ready`.
  - `FAIL`: any surviving falsifier; anchor each finding to its task,
    obligation (`R<n>`/`G<n>`), or primary source and name the smallest repair
    or reopen owner (`Planning`, `Technical Design`, `Specification`, `Intake`,
    or external authority).
- **No-implementation result:** `PASS` requires primary-source evidence proving
  every obligation is already met or an authorized scope exit, with owners and
  stages named for any outstanding proof or external effects; otherwise `FAIL`.
