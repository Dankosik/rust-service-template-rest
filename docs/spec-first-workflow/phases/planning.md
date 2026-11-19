# Planning

Turn accepted upstream contracts into a ready task ledger for fresh parallel
Leads. Consume closed decisions from `spec.md`, `design/`, and primary code —
reopen the smallest upstream owner on an open product or architecture fork
rather than loading `.agents/skills/rust-*` here, and leave test cases,
fixtures, proving layers, and test commands to Implementation.

## Steps

1. **Reconcile accepted inputs and delta.**
   - Read present artifacts in `specs/<task>/`: `intent.md`, `spec.md` (or the
     cited external contract when Definition was skipped), `design/` (or
     Technical Design's reviewed no-open-decisions `Transition Result` when
     Technical Design ran and omitted `design/design.md`), `rollout.md`,
     `research/*.md`, `workflow-plan.md`, and any existing `tasks.md`
     (`blocked`, `draft`, or `ready` on reopen).
   - Verify every present upstream artifact (`intent.md`, `spec.md`,
     `design/design.md`, `rollout.md`) is `status: ready` (or Technical
     Design's reviewed no-open-decisions result when Technical Design ran
     without `design.md`); on unready, missing, or conflicting authority, take
     [Blocked Exit](#blocked-exit).
   - **On reopen:** keep `tasks.md` untouched through Steps 1–3; note
     unaffected `T<ID>` anchors and any already-integrated `[x]` tasks (`Base:`
     and landed receipts; drop runtime fields if zero `[x]` tasks landed),
     reconcile only invalidated slices, and write `status: draft` (removing
     `Blocked stop:`) in Step 4.
   - Dispose every accepted obligation:

     | Input | Exact fields | Target home |
     | --- | --- | --- |
     | `spec.md` (or external contract) | `R<n>`, `G<n>`, limits | Owning task `Sources` + `Outcome`/`Observable claim` (or `Completion`) |
     | `spec.md` | `Downstream handoff` (`Planning`), `Durable contract update` | Fold `Durable contract update` into landing slice `Boundary -> In scope`; route `Downstream handoff` (`Planning`) to `Boundary -> In scope`, `Global constraints`, or slicing shape |
     | `design/design.md` (or Technical Design's reviewed no-open-decisions result) | `D<n>`, `S<n>`, `forced owners`, `Verified anchors`, `Pinned shared identifiers`, `Shared mutated surfaces`, `Superseded paths and generated contracts`, `Durable doc updates`, `Planning hooks` | Task `Sources`, `Mutable owners`, `Boundary -> In scope`, `Exclusive locks`, `Global constraints`, or slicing shape |
     | `rollout.md` | Gates, deploy boundaries | Consuming-stage `[effect:<action>]` gates; keep post-deploy phases out of Release 1's ledger |
     | `spec.md`, `design/design.md` (or no-open-decisions result) | `Carried concerns` | **Close** `owner: Planning` concerns now (take [Blocked Exit](#blocked-exit) if unclosable); carry `Implementation` / `Delivery Owner` concerns into `Global constraints` or affected packet `Reopen if` (never `Final validation`) |
     | `spec.md`, `design/design.md` (or no-open-decisions result), `workflow-plan.md` | `Decisions, assumptions, and reopen triggers`, `Assumptions and reopen triggers`, `D<n>` `Reopen if`, active rules | Affected packet `Reopen if` and `tasks.md` `Global constraints` |

   - If zero implementation delta and zero unaccepted integrated `[x]` tasks
     remain, follow [No-Implementation Exit](#no-implementation-exit) (if
     unaccepted `[x]` tasks remain with zero new coding delta, keep only those
     `[x]` tasks in `tasks.md` and run Steps 4–7 to hand off Final Validation).
   - If a ledger is produced while an individual `R<n>`/`D<n>`/`S<n>` already
     holds on base code, record `<ID>: already holds (<primary locator>)` in
     `tasks.md` `Global constraints` (or take [Blocked Exit](#blocked-exit) if
     stale upstream prose contradicts the accepted delta).
   - *Done when:* every present upstream artifact is `status: ready` and every
     row above maps to `T<ID>`, `<ID>: already holds (<primary locator>)`,
     `closed in Planning`, or an authorized exit.

2. **Ground in primary code, `--all-targets` blast radius, and locked crates
   (static — read-only, no builds, tests, or services).**
   - Verify upstream claims against primary code: inspect touched symbols,
     trait bounds, error/edge behavior, [Task Packet
     V1](../interfaces/task-packet-v1.md#field-rules)'s four `Exclusive locks`
     categories, and superseded code/tests. For every modified signature or
     exhaustive public workspace enum/struct, **search all call and match sites
     across the workspace — including tests compiled under `--all-targets`**.
     Spot `prefactor` opportunities now.
   - Check `Cargo.lock` sources (`cargo metadata --locked --offline
     --format-version 1` or cached paths) for **non-obvious, unanchored (absent
     from `design.md -> Verified anchors`), or slicing-critical** external
     crate APIs. Flag uncached crates for `Reopen if`; take [Blocked
     Exit](#blocked-exit) on any source contradiction or unpinned shared
     identifier needed across concurrent tasks.
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
     Axis 1 / Full brief). Keep `quality/architecture.json`,
     `template/profiles.json`, and `Cargo.toml`/`Cargo.lock` updates
     atomic with their slice, and update broken `--all-targets` callers inside
     the mutating task (or via additive `expand -> migrate -> contract`).
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
   - *Done when:* every remaining implementation obligation maps to a task,
     every task passes the **deletion test** and shape rules, every broken
     `--all-targets` caller compiles on landing, and the DAG is acyclic with a
     non-empty Ready Frontier.

4. **Persist draft ledger and packets.**
   Write `specs/<task>/tasks.md` at `status: draft` ([Authoring
   Schema](../interfaces/task-ledger-v1.md#authoring-schema-planning-owned),
   even for one task, removing any resolved `Blocked stop:`) and each packet
   ([Task Packet V1](../interfaces/task-packet-v1.md)), placing all Step 1
   items in their schema homes and deleting any superseded orphan packets under
   `specs/<task>/tasks/`.
   - *Done when:* ledger and packets exist on disk conforming to both schemas
     with zero orphans.

5. **Run mechanical check and semantic pre-review audit (static).**
   - **Mechanical gate:** run `make ledger-check TASK=<task>` (`python3
     scripts/check-ledger.py specs/<task>`) and fix every error.
   - **Semantic audit (close gaps in place, or take [Blocked
     Exit](#blocked-exit) on upstream gaps):**
     - *Producer isolation:* for each edge `T_a -> T_b`, read *only* `T_a`'s
       packet and cited `Sources` and confirm every contract `T_b` assumes is
       pinned in `T_a`.
     - *Concurrent collisions:* check every concurrently eligible pair (no
       directed coding-dependency path between them) for **undeclared**
       collisions across `Mutable owners`, [Task Packet
       V1](../interfaces/task-packet-v1.md#field-rules)'s four `Exclusive
       locks` categories, and exhaustive-type updates, with cross-task
       identifiers pinned once.
     - *Base-commit witness:* record one current-code `path::Symbol` (or shape
       witness) falsifying each `Outcome`, `Observable claim`, and `Completion`
       today and rejecting stubs or partial wiring.
   - *Done when:* `make ledger-check TASK=<task>` passes and every semantic
     witness holds.

6. **Submit for independent readiness review.**
   Dispatch fresh `reviewer-agent`(s) running [Task Review /
   Readiness](task-review-readiness.md) under [Review](../shared/review.md):
   **1 reviewer (`Full`)** for 1–2 task ledgers with `≤2` mutated crates and no
   `rollout.md` or wide refactor, or **2 parallel fresh reviewers (`Axis 1` and
   `Axis 2`)** when `≥3` tasks, `rollout.md`, wide refactor, or `≥3` mutated
   crates apply.
   - **Brief hygiene:** pass assigned mode (`Full`, `Axis 1`, `Axis 2`, or
     `Delta Recheck`), method path (`task-review-readiness.md`), candidate
     paths (`tasks.md`, packets), and `make ledger-check` output. Pass present
     upstream input paths, declined `Planning hooks` rationale, and `owner:
     Planning` concern closure locators to **Axis 1 / Full only**—omit author
     summaries or `"already verified"` claims so Axis 2 stays strictly
     cold-start over candidate files and cited `Sources`. `Delta Recheck` adds
     only anchored findings and the repair delta or refuting primary locators.
   - **On `FAIL`:** verify **every** finding's citation and witness against
     primary sources and handle refuted findings under [Review
     Lifecycle](../shared/review.md#lifecycle):
     - *Confirmed `PHASE_DEFECT`:* repair in place (deleting orphans on reslice
       and rerunning `make ledger-check` and affected Step 5 checks); re-run
       failed or invalidated axes (an Axis 1 repair altering edges, locks, or
       sources invalidates Axis 2; an Axis 2 repair altering slicing, coverage,
       or claims invalidates Axis 1), or run fresh review on reslice or changed
       outcomes/boundaries per [Review Lifecycle](../shared/review.md#lifecycle).
     - *Confirmed `UPSTREAM_GAP`:* take [Blocked Exit](#blocked-exit) carrying
       the review result.
   - *Done when:* every active axis returns `PASS` or `CONCERNS` on the current
     candidate (or closes via `adjudicator-agent` under [Review
     Lifecycle](../shared/review.md#lifecycle)), or [Blocked
     Exit](#blocked-exit) returns.

7. **Finalize and hand off.**
   Ensure every bounded review `CONCERN` is recorded in its Step 1 target home,
   set `status: ready` in `tasks.md`, delete `workflow-plan.md` when present
   ([Artifacts](../shared/artifacts.md)), and rerun `make ledger-check
   TASK=<task>`. Emit [Transition Result
   V1](../shared/transition.md#transition-result-v1) (`status: ready`, `owner:
   Planning`, `result: specs/<task>/tasks.md`, `movement_evidence: <carried
   CONCERNS, plus execution waves, lock serializations, and active Reopen if
   risks from ledger-check>`, `next_owner: Implementation`) for [Implementation
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
  unless risk/gate isolation (3), smart-zone overload (4), or a valid
  prerequisite shape (5) applies; tasks split solely for concurrency (2) that
  serialize on a shared lock or owner; horizontal layers (`types -> repo ->
  handler -> tests`); or standalone cleanup/docs tails (fold superseded cleanup
  and durable doc updates into the landing slice, except `Contract`).
- **Task vs lane:** parallel subtasks inside one outcome belong to Lead-chosen
  lanes, not separate ledger tasks.

| Shape | Tag | When to use | Guardrail |
| --- | --- | --- | --- |
| **Vertical slice (tracer bullet)** | *(default; omit)* | Narrow, complete path through touched layers delivering observable behavior (or a behavior-preserving/quantitative delta) at its seam alone. | Litmus test: *"With prerequisites met and other tasks deferred, what works or is provably changed at the seam now?"* |
| **Prefactor** | `[prefactor]` | Behavior-preserving cleanup of *existing* code ordered first to simplify the next slice(s). | Compiles alone; zero new feature types, schemas, or behavior. |
| **Shared contract** | `[shared-contract]` | Minimal compiling contract/schema (types, traits, proto, CLI/Make contract, migration) ordered first. | Valid **only** when `≥2` lock-disjoint downstream slices run in parallel after it lands; otherwise fold into the first vertical slice. |
| **Integration-first** / **Wide refactor** | `[integration-first]` / `[expand]`, `[migrate]`, `[contract]` | Unverified external seam is the primary risk, blast radius prevents one atomic green task, or `design.md -> Planning hooks` flags either. | Load [Conditional Planning Branches](planning/conditional-branches.md) only when triggered. |

## Blocked Exit

Use on any unready, missing, or conflicting upstream input, unclosable `owner:
Planning` concern, source contradiction, unpinned concurrent shared identifier,
or confirmed review `UPSTREAM_GAP`:

- If `specs/<task>/tasks.md` exists and clean slices or already-integrated
  `[x]` tasks (`Base:` runtime state) survive the blocker, keep `tasks.md` at
  `status: blocked` with `Blocked stop:` naming the missing input and owner,
  and set `result: specs/<task>/tasks.md`.
- Otherwise delete any stale draft `tasks.md` and `specs/<task>/tasks/*.md` and
  carry collected blockers inline in `result:`.
- Emit [Transition Result V1](../shared/transition.md#transition-result-v1)
  (`status: blocked`, `owner: Planning`, `reopen_owner: <smallest upstream
  owner>`).

## No-Implementation Exit

When Step 1 proves zero implementation delta and zero unaccepted integrated
`[x]` tasks remain:

1. Create no new ledger (keep any existing `tasks.md` / `specs/<task>/tasks/`
   untouched until review passes); submit the verified primary evidence (naming
   owners and stages of any outstanding proof or external effects) to 1 fresh
   `reviewer-agent` running [No-Implementation
   Result](task-review-readiness.md#no-implementation-result).
2. On `FAIL`: verify citations and handle refuted findings under [Review
   Lifecycle](../shared/review.md#lifecycle). Continue from Step 2 on confirmed
   `PHASE_DEFECT` (`reopen_owner: Planning`), or take [Blocked
   Exit](#blocked-exit) carrying the review result on confirmed `UPSTREAM_GAP`.
3. On `PASS` (or review closure under [Review
   Lifecycle](../shared/review.md#lifecycle)): delete any stale `tasks.md` and
   `specs/<task>/tasks/*.md`, carry active `workflow-plan.md` rules needed by a
   named external `next_owner` into `result:`, delete `workflow-plan.md` when
   present ([Artifacts](../shared/artifacts.md)), and emit [Transition Result
   V1](../shared/transition.md#transition-result-v1) (`status: ready`, `owner:
   Planning`, `result: <verified primary evidence>`, `movement_evidence: <zero
   implementation delta verified>`, `next_owner: none` or named external owner).
