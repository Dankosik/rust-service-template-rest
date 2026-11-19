# Technical Design

Use when structured work has open technical choices—**two-engineer divergence**
across mechanisms, contracts, failure or trust semantics, rollout paths, or
seams. Own technical architecture autonomously under [Decision
Ownership](../../../AGENTS.md#decision-ownership) while preserving `spec.md`'s
observable behavior.

**Boundary rule:** Fix every choice that can collide across parallel units,
alter a caller, wire, schema, config, or operator contract, break an invariant
or bound, or lock in a **one-way door**. Leave everything behind one module's
seam open to Implementation.

## Steps

1. **Ground in primary sources and map the Decision Frontier (static by default
   — no builds, tests, or services).**
   - Verify `specs/<task>/spec.md` is `status: ready` (otherwise take [Blocked
     Exit](#blocked-exit) with `reopen_owner: Specification`). If
     `specs/<task>/design/design.md` already exists (`blocked` or `ready`),
     inspect only what changed, keep surviving `D<n>`/`S<n>` IDs stable, and
     mark superseded `design/*.md` or `rollout.md` siblings for removal in step
     3 (or [No-Open-Decisions Exit](#no-open-decisions-exit)).
   - Read `specs/<task>/intent.md`, `specs/<task>/spec.md` (all obligations:
     `R<n>`, `G<n>`, `Terminology`, `Non-goals`, `Decisions, assumptions, and
     reopen triggers`—including `Technical Design` `Downstream handoff` and
     `Durable contract update`—and `Carried concerns`), any `research/*.md`,
     and `workflow-plan.md` when present.
   - Treat `spec.md` baseline prose as claims to verify against **primary
     sources**: affected crates and callers, **one existing sibling subsystem**
     (local error, config, wiring, and telemetry idioms), committed contracts
     (`api/`), [Repository Architecture](../../repo-architecture.md) (including
     `Domain Vocabulary`),
     [`quality/architecture.json`](../../../quality/architecture.json),
     relevant `docs/decisions/*.md`, [Backend Library
     Selection](../../backend-library-selection.md), affected profile sections
     of [Production Contract](../../production-contract.md) when applicable,
     and cached sources of non-obvious `Cargo.lock` crate APIs (via
     `Cargo.lock` or read-only `cargo metadata --locked --format-version 1`).
     Delegate read-heavy multi-crate or `Cargo.lock` scans to `evidence-agent`
     under [Read-Only Delegation](../shared/read-only-delegation.md), verifying
     decision-critical facts directly.
   - **Map the Decision Frontier** before proposing solutions: separate
     obligations into (a) **forced** by existing owners, (b) **open forks**
     tagged **one-way door** vs **two-way door**, (c) **terminology
     collisions** across `spec.md`, code, and `Domain Vocabulary`, and (d)
     **upstream blockers or evidence gaps**.
   - **Route open forks:**

     | Open fork or trigger | Load |
     | --- | --- |
     | Runtime crossings, data/schema, truth, trust, failure/concurrency, budgets, config, signals, or rollout | [System Design](technical-design/system-design.md) |
     | Rust modules, seams, error/invariant ownership, workspace graph, or generated/profile containment not forced by code and `quality/architecture.json` | [Rust Architecture](technical-design/rust-architecture.md) |
     | Deployable, worker, wire contract, migration, runtime config/trust material, or managed dependency change | [Release Closure](../rubrics/release-closure.md) |
     | Decision-changing evidence gap (code, external contract, or authorized read-only empirical probe without mutating repo source) | Supporting [Research](research.md) **inside this phase** (inline, or `specs/<task>/research/<topic>.md` when reused downstream) |

   - **Early exits:** on upstream blockers or spec falsification, take [Blocked
     Exit](#blocked-exit). When zero open forks exist and no non-trivial
     `rollout.md` is required, take [No-Open-Decisions
     Exit](#no-open-decisions-exit).
   - *Complete step 1 only when* every `spec.md` obligation is reconciled
     against primary sources, the Decision Frontier is classified, and zero
     decision-changing static gaps remain.

2. **Close open decisions across triggered branches.** Load only routed
   branches and matching **design-time** domain skills from the catalog
   (including `rust-idiomatic` and `rust-structural-quality` for Rust seams,
   and `rust-dependencies` for crates or custom-vs-library forks; keep
   coding/testing skills in Implementation). When **both** System Design and
   Rust Architecture trigger, **close System Design first** so Rust seams
   encode system invariants (allowing Rust compile-reality findings to push
   back on System Design).
   - **Reuse-first & steelman:** before adding any mechanism, crate, module,
     trait, dependency, queue, fallback, or config key, steelman the strongest
     simpler substitute (`extend owner / collapse shallow chain -> std ->
     locked Cargo.lock crate -> maintained OSS -> custom`) and name the exact
     `R<n>`, `G<n>`, invariant, or bound it breaks. Honor rejected choices in
     `docs/decisions/` and `docs/backend-library-selection.md` unless their
     reopen condition is met.
   - **Design It Twice:** load [Design It
     Twice](technical-design/design-it-twice.md) when an open fork hits an
     unsettled **one-way door**—persisted schema or durable state machine,
     new/changed external wire or API contract, ≥2-consumer cross-crate public
     seam, async durability/recovery topology, or new crate, dependency, or
     profile.
   - **Uncertainty disposition:**

     | Uncertainty | Action |
     | --- | --- |
     | Statically checkable in code, `Cargo.lock`, or primary docs | Settle now from primary sources. |
     | Non-obvious locked crate source genuinely uncached locally | Record unverified assumption + concrete `Reopen if` in `Verified anchors` and `Assumptions`. |
     | Decision-changing runtime/integration behavior, hard budget, or unprovable Rust type/macro bound | Run an authorized read-only [Research](research.md) probe without mutating repo source, or flag `Planning hooks: integration-first` + concrete `Reopen if`. |
     | Non-decisive local detail behind one seam | Leave open to Implementation. |

   - If step 2 falsifies `spec.md` or exposes a user-owned trade-off, take
     [Blocked Exit](#blocked-exit).
   - *Complete step 2 only when* every triggered branch and terminology
     collision is closed, every genuine fork has a steelman substitute,
     decisive constraint, accepted cost, and objective `Reopen if`, and every
     relied-on symbol/API is source-verified or carried in `Reopen if`.

3. **Persist draft artifact(s).** Write `specs/<task>/design/design.md` at
   `status: draft` (removing resolved `Blocked decisions`) using [Design
   V1](../interfaces/design-v1.md) via [Artifacts](../shared/artifacts.md), and
   `specs/<task>/rollout.md` at `status: draft` when [Release
   Closure](../rubrics/release-closure.md) needs a non-trivial sequence. Delete
   superseded `design/*.md` or `rollout.md`.
   - *Complete step 3 only when* draft artifact(s) are on disk with every
     applicable `Design V1` section concretely bound and zero residual `Blocked
     decisions`.

4. **Run the Author Pre-Review Audit (static, on-disk files only).**
   - **Trace & handoff:** every `spec.md` obligation sits in `Obligation trace`
     with owner and proving surface; zero `Non-goals` leaked; `Grounding and
     planning handoff` pins all cross-unit identifiers, narrow canonical lock
     paths, superseded deletions, generated contracts, and durable doc updates.
   - **Deletion & boundary:** every added mechanism or structure traces to an
     explicit `spec.md` obligation or a steelman `D<n>`; strip leaked function
     bodies, private file trees, task slicing, or test prescriptions.
   - **Compile & callers:** every anchor and `S<n>` signature matches inspected
     primary sources and static compile reality; walk each `S<n>` caller sketch
     end-to-end.
   - **Witnesses & divergence:** when System Design triggered, walk one
     worst-case witness per active risk class (`CommitUnknown`/failure,
     cancel/drop, race/interleaving, trust); verify from `spec.md`, `design.md`,
     and cited code alone that two fresh Leads cannot diverge.
   - *Complete step 4 only when* all four checks clear on the on-disk `draft`.

5. **Submit for independent review.** Dispatch fresh `reviewer-agent`(s)
   running [Technical Design Review](technical-design-review.md) under shared
   [Review](../shared/review.md):
   - **Default (1 fresh `reviewer-agent`):** single triggered branch or small
     local design.
   - **Two-axis parallel review (2 fresh `reviewer-agent` lanes, never forking
     parent history):** use only when **both** System Design and Rust
     Architecture triggered **and** at least one applies: multiple crates or
     crossings, a **one-way door** fork, or a non-trivial rollout. Assign
     `Axis 1` and `Axis 2` under [Review
     Topology](technical-design-review.md#review-topology). Keep reports
     distinct; recheck only failed/invalidated axes under shared
     [Review](../shared/review.md), and route one surviving evidence-backed
     conflict to `adjudicator-agent` ([Review](../shared/review.md#route)).
   - **Verify before repairing:** check every `FAIL` finding against primary
     sources and weigh suggested remedies against the deletion test and [Review
     Lifecycle](../shared/review.md#lifecycle). Repair confirmed design-local
     `FAIL` findings in place and rerun affected step 4 checks; refute false
     positives with primary anchors in the delta recheck (or escalate a
     surviving conflict to `adjudicator-agent`). On upstream gaps, spec
     falsification, or missing external authority, preserve any clean
     independent design-local repairs without another review pass, then take
     [Blocked Exit](#blocked-exit).
   - *Complete step 5 only when* review clears (`PASS` or `CONCERNS`) across all
     active axes on the current candidate, or [Blocked Exit](#blocked-exit)
     returns.

6. **Finalize and hand off.** Record bounded review `CONCERNS` (`risk,
   downstream owner, closing action`) and unresolved `spec.md` `Carried
   concerns` in `design.md`, flip `status: ready` in `design/design.md` (and
   `rollout.md` when present), emit [Transition Result
   V1](../shared/transition.md#transition-result-v1) (`status: ready`, `owner:
   Technical Design`, `result: specs/<task>/design/design.md`, `review:
   <locator or inline result>`, `next_owner: Planning`), and stop.

## No-Open-Decisions Exit

When step 1 proves zero open forks and no non-trivial `rollout.md` is required:

1. Create no new `design.md`; submit inline **forced-owner evidence** to 1
   fresh `reviewer-agent` running [Technical Design
   Review](technical-design-review.md): an `Obligation -> forced owner ->
   primary locator -> proving surface` table plus `Shared mutated surfaces`,
   `Superseded paths and generated contracts`, and `Durable doc updates` when
   any apply.
2. On `FAIL`: use a bounded delta recheck only when the forced path is
   unchanged and only handoff metadata was incomplete; if review exposes a
   genuine open fork (`reopen_owner: Technical Design`), enter steps 2–6 and
   run a **fresh** review on the drafted design; on upstream gaps or spec
   falsification, take [Blocked Exit](#blocked-exit).
3. On `PASS` or `CONCERNS`: delete any stale `specs/<task>/design/*.md` or
   `rollout.md` left from an earlier run, emit [Transition Result
   V1](../shared/transition.md#transition-result-v1) (`status: ready`, `owner:
   Technical Design`, `result: <reviewed forced-owner evidence + carried
   CONCERNS>`, `review: <locator or inline result>`, `next_owner: Planning`),
   and stop.

## Blocked Exit

Use on non-`ready` `spec.md`, missing/contradictory product behavior, tripped
`spec.md` `Reopen if`, falsified baseline/requirement, missing external
authority, or a surviving trade-off differing strictly on a user-owned choice
(never ask the user to choose an internal mechanism). Collect all independent
blockers in one pass, then emit [Transition Result
V1](../shared/transition.md#transition-result-v1) (`status: blocked`, `owner:
Technical Design`, `review: none`, `reopen_owner: Specification | Intake |
<external authority>`, `next_owner: none`):

- **Existing or worthwhile `design/design.md`:** write/update
  `specs/<task>/design/design.md` at `status: blocked` with `Blocked decisions`
  (primary evidence, options & consequences, recommendation), flip any
  persisted `specs/<task>/rollout.md` to `status: blocked`, and set `result:
  specs/<task>/design/design.md`.
- **No `design/design.md` on disk and no partial work to preserve:** flip any
  persisted `specs/<task>/rollout.md` to `status: blocked` and carry the
  collected blockers inline in `result:`.
