# Planning

Turn accepted upstream contracts into a ready task ledger for fresh parallel
Leads. Consume closed decisions from `spec.md`, `design/`, and primary code —
reopen the smallest upstream owner on an open product or architecture fork
rather than loading `.agents/skills/rust-*` here, and leave test cases,
fixtures, proving layers, and test commands to Implementation.

## Steps

1. **Reconcile accepted inputs and delta.**
   - Read `specs/<task>/`: `intent.md`, `spec.md`, `design/` (or Technical
     Design's reviewed no-open-decisions result when skipped), `rollout.md`,
     `research/*.md`, `workflow-plan.md`, and any existing `tasks.md`
     (`blocked`, `draft`, or `ready` on reopen).
   - Verify every present upstream artifact (`spec.md`, `design/design.md`,
     `rollout.md`) is `status: ready` (or Technical Design's reviewed
     `Transition Result` when skipped); on unready, missing, or conflicting
     authority, return [Transition Result
     V1](../shared/transition.md#transition-result-v1) (`status: blocked`,
     smallest upstream `reopen_owner`).
   - **On reopen:** reset `tasks.md` to `status: draft`, preserve unaffected
     `T<ID>` anchors and any already-integrated `[x]` tasks (`Base:` and
     landed receipts), reconcile only invalidated slices, and delete obsolete
     packets in Step 4.
   - Dispose every accepted obligation:

     | Input | Exact fields | Target home |
     | --- | --- | --- |
     | `spec.md` | `R<n>`, `G<n>`, limits | Owning task `Sources` + `Outcome`/`Observable claim` (or `Completion`) |
     | `spec.md` | `Downstream handoff` (`Planning`), `Durable contract update` | Fold into `Boundary -> In scope` of the slice landing that decision |
     | `design/design.md` | `D<n>`, `S<n>`, `Verified anchors`, `Pinned shared identifiers`, `Shared mutated surfaces`, `Superseded paths and generated contracts`, `Durable doc updates`, `Planning hooks` | Task `Sources`, `Boundary -> In scope`, `Exclusive locks`, `Global constraints`, or slicing shape |
     | `rollout.md` | Gates, deploy boundaries | Consuming-stage `[effect:<action>]` gates; keep post-deploy phases out of Release 1's ledger |
     | `spec.md`, `design/design.md` | `Carried concerns` | **Close** `owner: Planning` concerns now (block upstream if unclosable); carry `Implementation` / `Delivery Owner` concerns into `Global constraints` or affected packet `Reopen if` |
     | `spec.md`, `design/design.md`, `workflow-plan.md` | `Assumptions and reopen triggers`, active rules | Affected packet `Reopen if` and `tasks.md` `Global constraints` |

   - If zero implementation delta remains, follow [No-Implementation
     Exit](#no-implementation-exit).
   - *Done when:* every present upstream artifact is `status: ready` and every
     row above maps to `T<ID>`, `already holds + primary locator`, `closed in
     Planning`, or an authorized exit.

2. **Ground in primary code, `--all-targets` blast radius, and locked crates
   (static — read-only, no builds, tests, or services).**
   - Verify upstream claims against primary code: inspect touched symbols,
     trait bounds, error/edge behavior, generated contracts, migrations,
     manifests (`Cargo.toml`, `Cargo.lock`, `quality/architecture.json`,
     `scripts/template/profiles.json`), shared wiring (including parent
     `lib.rs`/`mod.rs`), and superseded code/tests. For every modified
     signature or exhaustive public workspace enum/struct, **search all call
     and match sites across the workspace — including tests compiled under
     `--all-targets`**. Spot `prefactor` opportunities now.
   - Check `Cargo.lock` sources (`cargo metadata --locked --format-version 1`
     or cached paths) for **non-obvious, unanchored (absent from `design.md ->
     Verified anchors`), or slicing-critical** external crate APIs. Flag
     uncached crates for `Reopen if`; block upstream on any source
     contradiction.
   - For large multi-crate lookups, delegate bounded searches to
     `evidence-agent` ([Read-Only Delegation](../shared/read-only-delegation.md)),
     verifying decision-critical facts directly.
   - *Done when:* every modified signature/exhaustive type has its
     `--all-targets` call sites listed, every mutated lock surface is
     identified, and non-obvious external APIs are source-verified (or recorded
     in `Reopen if`) with zero contradictions.

3. **Design and pressure-test the task graph.**
   - Slice under [Task Shapes And Granularity](#task-shapes-and-granularity),
     honoring `Planning hooks` (if declining a hook, pass why in the Step 6
     brief). Keep `quality/architecture.json`, `scripts/template/profiles.json`,
     and `Cargo.toml`/`Cargo.lock` updates atomic with their slice, and update
     broken `--all-targets` callers inside the mutating task (or via additive
     `expand -> migrate -> contract`). Place external gates at their consuming
     stage, preserving `rollout.md` deploy boundaries.
   - **Two-way alternative-cut check:**
     - *If 1 task (spanning `≥3` crates, mixed risk/gate boundaries, or an
       unverified external seam):* test whether reading inputs, callers, tests,
       and compile iterations truly fit one fresh Lead's smart zone, or whether
       splitting independent outcomes / pulling forward `integration-first` or
       `prefactor` retires risk earlier.
     - *If `≥2` tasks:* test (a) merging lock- or owner-coupled pairs (including
       `Cargo.toml`/`Cargo.lock` or `lib.rs` serialization); and (b) pulling
       forward a `prefactor` or `integration-first` slice when it simplifies or
       de-risks `≥2` downstream tasks.
   - *Done when:* every Step 1 obligation maps to a task, every task passes the
     **deletion test** and shape rules, every broken `--all-targets` caller
     compiles on landing, and the DAG is acyclic with a non-empty Ready
     Frontier.

4. **Persist draft ledger and packets.**
   Write `specs/<task>/tasks.md` at `status: draft` ([Authoring
   Schema](../interfaces/task-ledger-v1.md#authoring-schema-planning-owned),
   even for one task) and each packet ([Task Packet
   V1](../interfaces/task-packet-v1.md)) with exact owned `R<n>`/`G<n>`/`D<n>`/
   `S<n>` anchors in `Sources`, exact downstream contracts in producer
   `Provides`, and zero orphan packets under `specs/<task>/tasks/`.
   - *Done when:* ledger and packets exist on disk with all Step 1 items placed
     in their schema homes and zero orphans.

5. **Run mechanical check and semantic pre-review audit (static).**
   - **Mechanical gate:** run `make ledger-check TASK=<task>` (`python3
     scripts/verify/check_ledger.py specs/<task>`) and fix every error.
   - **Semantic audit (close gaps in place, or block upstream on spec/design
     gaps):**
     - *Producer isolation:* for each edge `T_a -> T_b`, read *only* `T_a`'s
       packet and cited `Sources` and confirm every contract `T_b` assumes is
       pinned in `T_a`.
     - *Concurrent collisions:* check every concurrent pair in the generated
       waves for disjoint `Mutable owners`, `Exclusive locks`, and
       exhaustive-type updates, with cross-task identifiers pinned once.
     - *Base-commit witness:* record one current-code `path::Symbol` (or shape
       witness) falsifying each `Outcome`, `Observable claim`, and `Completion`
       today and rejecting stubs or partial wiring.
   - *Done when:* `make ledger-check TASK=<task>` passes and every semantic
     witness holds.

6. **Submit for independent readiness review.**
   Dispatch fresh `reviewer-agent`(s) running [Task Review /
   Readiness](task-review-readiness.md) under [Review](../shared/review.md):
   **1 reviewer** for 1–2 task ledgers with `≤2` mutated crates and no
   `rollout.md` or wide refactor, or **2 parallel fresh reviewers (`Axis 1` and
   `Axis 2`)** when `≥3` tasks, `rollout.md`, wide refactor, or `≥3` mutated
   crates apply.
   - **Reviewer brief:** mode (`Full`, `Axis 1`, `Axis 2`, or `Delta Recheck`
     with anchored findings and edited packets), candidate paths +
     `check_ledger.py` output, present upstream inputs, declined `Planning
     hooks` rationale, and closure evidence for `owner: Planning` `Carried
     concerns`.
   - Verify every `PHASE_DEFECT` finding against primary sources before
     repairing; repair plan-local defects in place (deleting orphans on
     reslice, rerunning `make ledger-check` and affected Step 5 checks,
     rechecking only failed/invalidated axes for non-reslice repairs, and using
     fresh review on reslice or changed outcomes/boundaries per
     [Review](../shared/review.md#lifecycle)).
   - On `UPSTREAM_GAP` or source contradictions: preserve `tasks.md` at
     `status: blocked` with `Blocked stop:` whenever clean slices or
     already-integrated `[x]` tasks (`Base:` runtime state) survive the blocker
     (otherwise delete `tasks.md` and `specs/<task>/tasks/*.md`), and return
     [Transition Result V1](../shared/transition.md#transition-result-v1)
     (`status: blocked`, smallest upstream `reopen_owner`).
   - *Done when:* all active review axes clear (`PASS` or `CONCERNS`), or a
     `blocked` handoff is returned.

7. **Finalize and hand off.**
   Carry bounded review `CONCERNS` into `Global constraints` or affected packet
   `Reopen if` (never `Final validation`), set `status: ready` in `tasks.md`,
   delete `workflow-plan.md` when present ([Artifacts](../shared/artifacts.md)),
   and rerun `make ledger-check TASK=<task>`. Emit [Transition Result
   V1](../shared/transition.md#transition-result-v1) (`status: ready`, `owner:
   Planning`, `result: specs/<task>/tasks.md`, `review: <locator or inline
   result>`, `movement_evidence: <execution waves, lock serializations, carried
   CONCERNS, and active Reopen if risks from ledger-check>`, `next_owner:
   Implementation`) for [Implementation
   Handoff](../shared/transition.md#implementation-handoff).
   - *Done when:* `tasks.md` is `status: ready`, `make ledger-check` exits
     clean, and the receipt is emitted. **Stop here; never start implementation
     inside Planning.**

## Task Shapes And Granularity

Default to **1 task** whenever one fresh Lead holds the change in its **smart
zone** (one coherent outcome, bounded crate set, uniform risk/gate boundary).

- **Split** only for: (1) independent observable outcomes compiling and
  verifying alone; (2) genuine parallelism across disjoint `Mutable owners`,
  `Exclusive locks`, and exhaustive-type updates; (3) isolating an unverified
  integration seam or external gate; (4) smart-zone or blast-radius overload
  across independent subsystems; or (5) a prerequisite shape below.
- **Merge** tasks of one outcome sharing an `Exclusive lock` or `Mutable owner`
  without risk/gate isolation or smart-zone overload, tasks split solely for
  concurrency that serialize on a shared lock, horizontal layers (`types ->
  repo -> handler -> tests`), or standalone cleanup/docs tails (fold superseded
  cleanup and durable doc updates into the landing slice, except `Contract`).
- **Task vs lane:** parallel subtasks inside one outcome belong to Lead-chosen
  lanes, not separate ledger tasks.

| Shape | Tag | When to use | Guardrail |
| --- | --- | --- | --- |
| **Vertical slice (tracer bullet)** | *(default; omit)* | Narrow, complete path through touched layers delivering observable behavior (or a behavior-preserving/quantitative delta) at its seam alone. | Litmus test: *"With prerequisites met and other tasks deferred, what works or is provably changed at the seam now?"* |
| **Prefactor** | `[prefactor]` | Behavior-preserving cleanup of *existing* code ordered first to simplify the next slice(s). | Compiles alone; zero new feature types, schemas, or behavior. |
| **Shared contract** | `[shared-contract]` | Minimal compiling contract/schema (types, traits, proto, CLI/Make contract, migration) ordered first. | Valid **only** when `≥2` lock-disjoint downstream slices run in parallel after it lands; otherwise fold into the first vertical slice. |
| **Integration-first** / **Wide refactor** | `[integration-first]` / `[expand]`, `[migrate]`, `[contract]` | Unverified external seam is the primary risk, blast radius prevents one atomic green task, or `design.md -> Planning hooks` flags either. | Load [Conditional Planning Branches](planning/conditional-branches.md) only when triggered. |

## No-Implementation Exit

When Step 1 proves zero implementation delta remains:

1. Create no new ledger (keep any existing `tasks.md` / `specs/<task>/tasks/`
   untouched until review passes); submit the verified primary evidence (naming
   owners and stages of any outstanding proof or external effects) to a fresh
   `reviewer-agent` running [Task Review /
   Readiness](task-review-readiness.md).
2. On `FAIL`: continue from Step 2 when delta remains (`reopen_owner:
   Planning`), or return `status: blocked` with the smallest upstream
   `reopen_owner` on an upstream gap or false premise.
3. On `PASS`: delete any stale `tasks.md` and `specs/<task>/tasks/*.md` from an
   earlier blocked or reopened run (only when zero integrated `[x]` tasks need
   ledger history), carry active `workflow-plan.md` rules needed by a named
   external `next_owner` into `result:`, delete `workflow-plan.md`, and return
   [Transition Result V1](../shared/transition.md#transition-result-v1)
   (`status: ready`, `owner: Planning`, `result: <verified primary evidence>`,
   `review: <locator or inline result>`, `next_owner: none` or named external
   owner).
