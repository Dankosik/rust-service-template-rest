# Planning

Use when structured work needs a ready task ledger for implementation. Own task
slicing, dependency ordering, lock isolation, and falsifiable claims from
accepted inputs. Preserve upstream behavior and architecture; leave test cases,
fixtures, proving layers, and test commands to Implementation.

## Task Shapes And Granularity

Start from a **1-task** hypothesis whenever one fresh Lead can hold the change
in its **smart zone** (one coherent outcome, bounded crate set, uniform risk
and gate boundary).

- **Split** only for: (1) independent observable outcomes compiling and
  verifying alone; (2) genuine parallelism across disjoint `Mutable owners`,
  `Exclusive locks`, and exhaustive-type updates; (3) isolating an unverified
  integration seam or external gate; (4) smart-zone or blast-radius overload
  across independent subsystems; or (5) a prerequisite shape below.
- **Merge** tasks belonging to one outcome that share an `Exclusive lock` or
  `Mutable owner` without risk/gate isolation or smart-zone overload, tasks
  split solely for concurrency that serialize on a shared lock, horizontal
  feature layers (`types -> repo -> handler -> tests`), or standalone
  cleanup/docs tail tasks (fold superseded cleanup and required durable
  contract or decision-doc updates into the slice landing that decision, except
  `Contract`).
- **Task vs lane:** parallel subtasks inside one outcome belong to Lead-chosen
  lanes, not separate ledger tasks.

| Shape | Tag | When to use | Guardrail |
| --- | --- | --- | --- |
| **Vertical slice (tracer bullet)** | *(default; omit)* | Narrow, complete path through all touched layers delivering observable behavior at its public **seam** alone. | Litmus test: *"With prerequisites met and other tasks deferred, what works at the public seam now?"* |
| **Prefactor** | `[prefactor]` | Behavior-preserving cleanup of *existing* code ordered first to simplify the next slice(s). | Compiles alone; zero new feature types, schemas, or behavior. |
| **Shared contract** | `[shared-contract]` | Minimal compiling contract/schema (types, traits, proto, CLI/Make contract, migration) ordered first. | Valid **only** when ≥2 downstream slices with disjoint `Mutable owners` and `Exclusive locks` run in parallel after it lands; otherwise fold into the first vertical slice. |
| **Integration-first** / **Wide refactor** | `[integration-first]` / `[expand]`, `[migrate]`, `[contract]` | Unverified external seam is the primary risk, or blast radius prevents one atomic green task. | Load [Conditional Planning Branches](planning/conditional-branches.md) only when triggered. |

## Steps

1. **Reconcile accepted inputs and delta.**
   - Read `specs/<task>/`: `intent.md`, `spec.md`, `design/` (including
     `Grounding and planning handoff`—or Technical Design's reviewed
     no-open-decisions evidence when `design/` was skipped), `rollout.md`,
     `research/*.md`, `workflow-plan.md`, and any existing `status: blocked`
     `tasks.md` (to reconcile on re-entry) when present.
   - Verify `spec.md`, plus `design/design.md` and `rollout.md` when present,
     are `status: ready` (and Technical Design's reviewed `Transition Result`
     when `design/` was skipped); on any unready, missing, or conflicting
     upstream decision, return [Transition Result
     V1](../shared/transition.md#transition-result-v1) (`status: blocked`,
     smallest upstream `reopen_owner`).
   - Map every accepted item—`R<n>`, `G<n>`, `Planning` `Downstream handoff`,
     limits, rollout gates, `Carried concerns`, upstream `Assumptions and
     reopen triggers`, `design/` `Planning hooks` and `Pinned shared
     identifiers`, and required durable contract or decision-doc updates—to a
     task delta, primary evidence proving it already holds, or an authorized
     scope exit. If zero implementation delta remains, follow
     [No-Implementation Exit](#no-implementation-exit).
   - *Done when:* every mapped item has an explicit disposition.

2. **Ground in primary code, blast radius, and locked crates (static — read-only,
   no builds, tests, or services).**
   - Verify upstream prose and `design/` summaries against primary source
     before slicing:
     - **Workspace & blast radius:** inspect signatures, trait bounds,
       error/edge behavior, callers, shared registration/wiring files (starting
       from `design/` `Shared mutated surfaces` when present), generated
       contracts, migrations, manifests, exhaustive public workspace
       enums/structs, and superseded code/tests. Spot `prefactor` opportunities
       now.
     - **Locked external crates (targeted):** check `Cargo.lock` sources
       (cached paths or read-only `cargo metadata --locked --format-version 1`)
       for **non-obvious, unanchored (not in `design.md -> Verified anchors`),
       or slicing-critical** external APIs. Flag uncached crates for `Reopen
       if`; stop `status: blocked` with the smallest upstream `reopen_owner` on
       any source contradiction.
     - **Smart-zone delegation:** for large multi-crate changes, delegate
       bounded read-heavy caller or `Cargo.lock` lookups to `evidence-agent`
       ([Read-Only Delegation](../shared/read-only-delegation.md)), verifying
       decision-critical facts directly.
   - *Done when:* touched workspace contracts, blast-radius callers, shared
     surfaces, exhaustive-type fan-out, and non-obvious crate APIs are grounded
     with zero contradictions.

3. **Design and pressure-test the task graph before writing files.**
   - Slice under [Task Shapes And Granularity](#task-shapes-and-granularity),
     honoring `Planning hooks` (if a simpler cut declines a hook, include why
     it still retires that risk in the Step 6 review brief). Place external
     actions at their consuming stage with owner and success condition,
     preserving `rollout.md` deploy boundaries.
   - **Alternative-cut check (when ≥2 tasks):** test two concrete counter-cuts:
     (a) merge any pair sharing an `Exclusive lock` or `Mutable owner` unless
     independent outcomes, risk/gate isolation, or smart-zone limits justify
     keeping them separate; (b) pull forward a `prefactor` or
     `integration-first` slice when it simplifies or de-risks ≥2 downstream
     tasks. Keep the cut that removes lock serialization and retires risk
     earliest inside each Lead's smart zone.
   - *Done when:*
     - every mapped delta sits in a task and every task's `Provides` feeds a
       downstream task or `Completion` (**deletion test**);
     - every task matches a valid shape with zero horizontal layers or false
       parallelism;
     - every existing caller broken by a signature or exhaustive-type change
       sits inside the mutating task (or uses additive `expand -> migrate ->
       contract`), while new consumers compile behind their `Depends on` edge;
     - the DAG is acyclic, has a non-empty Ready Frontier, and lands all code
       tasks `Implemented` before final validation.

4. **Persist draft ledger and packets.**
   Write `specs/<task>/tasks.md` at `status: draft` ([Authoring
   Schema](../interfaces/task-ledger-v1.md#authoring-schema-planning-owned),
   even for one task) and each packet ([Task Packet
   V1](../interfaces/task-packet-v1.md)), migrating active `workflow-plan.md`
   rules, `Carried concerns`, cross-task shared pins, and upstream reopen
   triggers into their schema homes, and deleting orphan packets under
   `specs/<task>/tasks/`.

5. **Run the Pre-Review Artifact Audit (static).**
   Verify four objective properties over the written files and primary code,
   closing every plan-local gap in place (or stopping `status: blocked` with
   the smallest upstream `reopen_owner` on any exposed spec/design gap):
   - **Producer isolation:** for each edge `T_a -> T_b`, read *only* `T_a`'s
     packet and cited `Sources` (as `T_a`'s Lead will) and confirm every type,
     signature, error mode, export, or schema invariant `T_b` assumes is pinned
     in `T_a`.
   - **Shared tokens & collisions:** for every concurrently eligible pair,
     confirm disjoint `Mutable owners`, `Exclusive locks` (or identical shared
     lock tokens serializing them), and exhaustive-type updates, with all
     shared identifiers pinned in cited `Sources` or `Global constraints`.
   - **Base-commit witness:** for each task's `Outcome`, `Observable claim`,
     and `tasks.md`'s `Completion`, verify a current-code fact (`path::Symbol`)
     falsifies it today (or confirm its [Task Packet
     V1](../interfaces/task-packet-v1.md#field-rules) shape rule) and that a
     stub or partial wiring cannot satisfy it.
   - **Graph, parity & propagation:** confirm exact ID, non-default shape tag,
     and stage-tag parity between `tasks.md` and packets, topological +
     priority order, zero orphan files, and migration of `workflow-plan.md`
     rules, `Carried concerns`, and upstream `Assumptions and reopen triggers`.
   - *Done when:* every exposed gap is repaired in place or routed upstream.

6. **Submit for independent readiness review.**
   Route the fixed `draft` ledger to a fresh `reviewer-agent` running [Task
   Review / Readiness](task-review-readiness.md) under
   [Review](../shared/review.md). Repair plan-local `FAIL` findings in place
   (deleting orphans on reslice, rerunning affected Step 5 checks, and using a
   fresh reviewer when reslicing or changing outcomes/boundaries per
   [Review](../shared/review.md#lifecycle)). On upstream gaps or source
   contradictions, set `status: blocked` with `Blocked stop:` in `tasks.md`
   when preserving partial work (otherwise delete `tasks.md` and
   `specs/<task>/tasks/*.md`) and return [Transition Result
   V1](../shared/transition.md#transition-result-v1) (`status: blocked`,
   smallest upstream `reopen_owner`).

7. **Finalize and hand off.**
   After review returns `PASS` or `CONCERNS`, carry bounded `CONCERNS` into
   `Global constraints` or the affected packet's `Reopen if` (never into `Final
   validation`), set `status: ready` in `tasks.md`, delete `workflow-plan.md`
   when present ([Artifacts](../shared/artifacts.md)), and return [Transition
   Result V1](../shared/transition.md#transition-result-v1) (`status: ready`,
   `owner: Planning`, `result: specs/<task>/tasks.md`, `review: <locator or
   inline result>`, `next_owner: Implementation`) for [Implementation
   Handoff](../shared/transition.md#implementation-handoff). **Stop here; never
   start implementation inside Planning.**

## No-Implementation Exit

When Step 1 proves zero implementation delta remains:

1. Create no ledger; submit the verified primary evidence (naming owners and
   stages of any outstanding proof or external effects) to a fresh
   `reviewer-agent` running [Task Review / Readiness](task-review-readiness.md).
2. On `FAIL`: continue from Step 2 when implementation delta remains
   (`reopen_owner: Planning`), or return [Transition Result
   V1](../shared/transition.md#transition-result-v1) (`status: blocked`,
   smallest upstream `reopen_owner`) on an upstream gap or false premise.
3. On `PASS`: carry any active `workflow-plan.md` instructions needed by a
   named external `next_owner` into `result:`, delete `workflow-plan.md` when
   present ([Artifacts](../shared/artifacts.md)), and return [Transition Result
   V1](../shared/transition.md#transition-result-v1) (`status: ready`,
   `owner: Planning`, `result: <verified primary evidence>`, `review: <locator
   or inline result>`, `next_owner: none` or named external owner).
