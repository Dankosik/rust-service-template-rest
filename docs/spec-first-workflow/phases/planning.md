# Planning

Use when structured work needs a ready task ledger for implementation. Own task
slicing, dependency ordering, writable boundaries, and final validation claims
from accepted inputs. Preserve upstream behavior and architecture; leave test
cases, fixtures, proving layers, and exact test commands to Implementation.

## Steps

1. **Reconcile accepted inputs and delta.** Read all accepted artifacts in
   `specs/<task>/` (`intent.md`, `spec.md`, `design/`, `rollout.md`,
   `research/*.md`, and `workflow-plan.md` when present), or Technical Design's
   reviewed no-open-decisions evidence. Map every `R<n>`, `G<n>`, limit,
   exception, rollout gate, `Carried concerns` item, and required durable
   decision-doc update to a task delta, primary evidence proving it is already
   met, or an authorized scope exit.
   - Missing or conflicting upstream decision: stop and return [Transition
     Result V1](../shared/transition.md#transition-result-v1) (`status:
     blocked`, `reopen_owner` naming the smallest upstream owner).
   - No implementation delta remains: create no ledger and submit the primary
     evidence (naming owners and stages of any outstanding proof or external
     effects) directly to step 6.
   - *Complete this step only when* every accepted obligation, guarantee,
     carried concern, and required doc update has an explicit disposition.

2. **Ground in primary code, blast radius, and locked dependencies (static —
   run no builds, tests, or services).** Inspect actual current code **before**
   slicing. Treat upstream prose and `design/` summaries as claims to verify
   against primary sources, never as evidence:
   - **Consumed symbols & locked crates:** verify each distinct consumed symbol,
     config key, CLI/Make contract, and—for external crates—the source of the
     `Cargo.lock` version (resolving cached paths via `Cargo.lock` or read-only
     `cargo metadata --locked --format-version 1`), checking **signatures and
     semantics**, not mere existence. Build on verified anchors in `design/`
     when present, checking any newly touched or unanchored items. Flag
     uncached crate sources for `Reopen if`; stop with `status: blocked` and
     the smallest upstream `reopen_owner` on any source contradiction.
   - **Blast radius & shared surfaces:** trace callers, generated contracts,
     migrations, manifests, shared registration files (`lib.rs`, router or
     bootstrap wiring), and superseded code/tests needing cleanup. Spot
     existing-code **prefactor** opportunities now.
   - **Smart-zone delegation:** for large multi-crate or multi-surface changes,
     delegate bounded read-heavy caller or `Cargo.lock` lookups to
     `evidence-agent` under [Read-Only
     Delegation](../shared/read-only-delegation.md), verifying decision-critical
     facts directly.
   - *Complete this step only when* every consumed API, blast-radius companion,
     and shared mutated surface is grounded in primary source with zero
     contradictions.

3. **Design and pressure-test the task graph before writing files.** Slice under
   [Task Shapes And Granularity](#task-shapes-and-granularity). Load
   [Conditional Planning Branches](planning/conditional-branches.md) only when
   an unverified integration seam is the primary risk (**integration-first
   slice**) or a **wide refactor**'s **blast radius** prevents one atomic green
   task (**expand–migrate–contract**). Place any required external action at
   its consuming stage with its owner and success condition (a change to
   release tooling can be its own task; approval to publish gates publication).
   - **Alternative-cut check (when ≥2 tasks):** compare your first cut against
     the sharpest alternative (merging lock-serialized tasks, or pulling a
     shared seam or high-risk slice earlier) rather than mirroring `spec.md`
     headings. Keep the cut with fewer tasks, less lock serialization, and
     earlier risk retirement.
   - *Complete this step only when:* every obligation, `G<n>`, carried concern,
     and durable doc update maps to a task; every task's `Provides` feeds a
     downstream task or `Completion` (**deletion test**); every task meets a
     valid shape and split/merge bound with zero false parallelism; every
     blast-radius companion sits inside its unit or behind a valid compiling
     dependency; and the DAG is acyclic with all code reaching `Implemented`
     before final validation.

4. **Persist draft ledger and packets.** Write `specs/<task>/tasks.md` at
   `status: draft` using [Task Ledger V1 (Authoring
   Schema)](../interfaces/task-ledger-v1.md#authoring-schema-planning-owned)
   (even for one task), ordered topologically (Ready Frontier first), migrating
   active `workflow-plan.md` instructions and unresolved `Carried concerns`
   into `Global constraints` or affected packets' `Reopen if`. Write each
   `specs/<task>/tasks/<ID>-<slug>.md` using [Task Packet
   V1](../interfaces/task-packet-v1.md). Record a predecessor only when its
   output or an accepted sequencing constraint is required, attaching each
   dependency to its earliest consuming stage with identical IDs and stage tags
   in both files. Record verified non-obvious anchors in `Sources`, canonical
   repo-relative lock paths in `Exclusive locks`, and pinned shared identifiers
   wherever concurrent tasks touch a shared concept. Delete any superseded
   orphan packets under `specs/<task>/tasks/`.

5. **Run the Cold-Start Self-Check (static).** Re-read **only** `tasks.md`,
   each packet, cited `Sources`, and referenced code as a zero-history Lead:
   - confirm two competent fresh Leads could not build incompatible interfaces,
     names, placements, or edge-case behavior;
   - try to satisfy every `Outcome` and `Observable claim` while breaking an
     obligation or `G<n>`, and verify each fails on the base commit and
     distinguishes accepted behavior from a plausible incomplete change (or
     meets the removal/prefactor/expand rule) without downstream or sibling
     tasks;
   - verify exact ID, stage-tag, and `Provides` parity between `tasks.md` and
     packets (zero orphan files), identify every individually ready task
     (including both tasks when two otherwise ready tasks share a free lock;
     dispatch chooses priority), and confirm every concurrently eligible pair
     either has disjoint `Mutable owners`, `Exclusive locks`, and shared files
     or declares the shared lock so dispatch serializes them.
   - *Complete this step only when* every exposed gap is closed in place.

6. **Submit for independent readiness review.** Route the fixed `draft` ledger
   (or no-implementation evidence) to a fresh `reviewer-agent` running [Task
   Review / Readiness](task-review-readiness.md) under
   [Review](../shared/review.md). Repair plan-local `FAIL` findings in place
   (deleting orphaned packets on reslice, rerunning affected step 5 checks, and
   using a fresh reviewer when reslicing or changing outcomes/boundaries under
   [Review](../shared/review.md#lifecycle)). Return `status: blocked` with
   `reopen_owner` in [Transition Result
   V1](../shared/transition.md#transition-result-v1) for upstream gaps or
   primary-source contradictions.

7. **Finalize and hand off.** After review returns `PASS` or `CONCERNS`:
   - **No-implementation result:** return [Transition Result
     V1](../shared/transition.md#transition-result-v1) (`status: ready`,
     `owner: Planning`, `result: <verified primary evidence>`, `review: <locator
     or inline result>`, `next_owner: none` or named external owner) without
     creating a ledger.
   - **Draft ledger:** carry any bounded `CONCERNS` into `Global constraints`
     or the affected packet's `Reopen if` (never into `Final validation`), set
     `status: ready` in `tasks.md`, delete `workflow-plan.md` when present
     ([Artifacts](../shared/artifacts.md)), and return [Transition Result
     V1](../shared/transition.md#transition-result-v1) (`status: ready`,
     `owner: Planning`, `result: specs/<task>/tasks.md`, `review: <locator or
     inline result>`, `next_owner: Implementation`) for [Implementation
     Handoff](../shared/transition.md#implementation-handoff). Stop here; never
     start implementation inside Planning.

## Task Shapes And Granularity

- **Smallest valid ledger first.** Default to one cohesive task when the change
  fits one fresh Lead's smart zone without mixing independent outcomes or
  risk/gate boundaries.
- **Split vs merge bounds:**
  - **Split** only for: (1) independent observable outcomes compiling and
    verifying alone, (2) smart-zone or blast-radius overload for one Lead,
    (3) genuine concurrency across disjoint `Mutable owners` and `Exclusive
    locks`, (4) isolating an unverified integration seam or external gate, or
    (5) a prerequisite shape below.
  - **Merge** tasks that share an `Exclusive lock` or `Mutable owner` without
    independent risk isolation or smart-zone overload, or any horizontal layer
    of one feature.
- **Valid task shapes** (horizontal layer splits like `types -> repo -> handler
  -> tests` for one feature are invalid):
  - **Vertical slice (tracer bullet):** default—a narrow, complete path through
    all touched layers delivering observable behavior at its public **seam**
    alone. Litmus test: *"Assume its prerequisites are met and all remaining
    work is deferred: who can use this outcome now, and what observable
    behavior can they verify at the public seam?"*
  - **Prefactor:** behavior-preserving refactor of *existing* code (zero new
    feature types, schemas, or behavior) ordered first only when it compiles
    alone and materially simplifies the next slice(s) ("make the change easy,
    then make the easy change").
  - **Shared seam:** minimal compiling contract or schema (types, traits,
    signatures, proto, CLI/Make contract, or migration) ordered first *only*
    when **≥2 parallel downstream slices** consume it concurrently—never for a
    single downstream slice.
  - **Integration-first slice** and **Wide refactor (expand–migrate–contract):**
    see [Conditional Planning Branches](planning/conditional-branches.md).
- **Task vs lane:** parallel work inside one outcome belongs to Lead-chosen
  lanes, not separate ledger tasks.
