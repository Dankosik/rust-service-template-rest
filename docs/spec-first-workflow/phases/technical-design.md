# Technical Design

Use when structured work has open technical choices—where two competent Rust
engineers reading `spec.md` and current code could build incompatible
mechanisms, contracts, failure or trust semantics, rollout paths, or
crate/module seams. Own technical architecture autonomously under [Decision
Ownership](../../../AGENTS.md#decision-ownership) while preserving `spec.md`'s
observable behavior.

**Boundary rule:** Fix every choice where two valid implementations would
collide across parallel units, alter a caller, wire, schema, config, or
operator contract, break an invariant or resource bound, or require a later
migration. Leave open everything behind one module's seam that one unit can
change without affecting callers, contracts, schemas, bounds, or signals.

## Steps

1. **Ground in primary sources and reconcile obligations (static — no builds,
   tests, or services).** Read `specs/<task>/intent.md`,
   `specs/<task>/spec.md` (every `R<n>`, `G<n>`, `Terminology`, `Non-goals`,
   `Decisions, assumptions, and reopen triggers`, and `Carried concerns`), any
   `research/*.md`, and `workflow-plan.md` when present. Treat `spec.md`'s
   `Current baseline` and prior prose as claims to verify against primary
   sources, never as proof. Inspect affected crates and callers, [Repository
   Architecture](../../repo-architecture.md),
   [`quality/architecture.json`](../../../quality/architecture.json), relevant
   `docs/decisions/*.md`, [Backend Library
   Selection](../../backend-library-selection.md), only the affected profile
   sections of [Production Contract](../../production-contract.md) when
   capacity, consistency, trust, or recovery applies, and the source of any
   non-obvious `Cargo.lock` crate API (resolving cached paths via `Cargo.lock`
   or read-only `cargo metadata --locked --format-version 1`). Delegate
   read-heavy multi-crate or `Cargo.lock` lookups to `evidence-agent` under
   [Read-Only Delegation](../shared/read-only-delegation.md), verifying
   decision-critical facts directly.
   - Route each open `R<n>`, `G<n>`, and carried concern to its branch(es):
     - runtime crossings, data/schema, truth, trust, failure/concurrency,
       budgets, signals, or rollout -> [System
       Design](technical-design/system-design.md);
     - Rust modules, seams, invariant ownership, dependency edges, or
       generated/profile containment not forced by code and
       `quality/architecture.json` -> [Rust
       Architecture](technical-design/rust-architecture.md);
     - deployable, worker, wire contract, migration, runtime config/trust
       material, or managed dependency change -> [Release
       Closure](../rubrics/release-closure.md);
     - missing current or external evidence that can change a technical choice
       -> supporting [Research](research.md) **inside this phase** (inline or
       via `evidence-agent`; persist `specs/<task>/research/<topic>.md` when
       review or later phases consume it).
   - **Early exits:**
     - *Upstream blocker or spec falsification:* missing/contradictory product
       behavior, a tripped `spec.md` `Reopen if` trigger, a baseline or
       requirement falsified by code/`Cargo.lock`/external constraints, or a
       trade-off differing strictly on a user-owned choice. Collect all
       independent blockers in one pass, save useful resolved work in
       `specs/<task>/design/design.md` at `status: blocked` when worthwhile,
       and return [Transition Result
       V1](../shared/transition.md#transition-result-v1) (`status: blocked`,
       `reopen_owner: Specification` or `Intake`—never ask the user to choose
       an internal mechanism).
     - *Mechanically forced (zero open choices):* current code,
       `quality/architecture.json`, and `spec.md` leave zero architectural
       ambiguity. Skip drafting `design.md` and submit the source-verified
       forced-owner evidence directly to step 5.
   - *Complete step 1 only when* every `R<n>`, `G<n>`, carried concern, and
     `spec.md` assumption/non-goal is reconciled against primary code and
     `Cargo.lock` sources, every open choice maps to a branch, and zero
     decision-changing evidence gaps remain.

2. **Close open decisions across triggered branches.** Load only the branches
   routed in step 1 and only the `.agents/skills/rust-*` domain methods whose
   trigger matches an open decision (including `rust-idiomatic` and
   `rust-structural-quality` for Rust seams; leave `rust-testing` to
   Implementation).
   - **Reuse-first & steelman challenge:** before adopting any new mechanism,
     crate, module, trait, dependency, queue, fallback, or config key, steelman
     the strongest simpler substitute (`existing repo owner -> std -> locked
     workspace crate -> approved/maintained OSS -> custom mechanism`) and name
     the exact `R<n>`, `G<n>`, invariant, or bound it breaks. Honor every
     rejected choice in `docs/decisions/` and
     `docs/backend-library-selection.md` unless its stated reopen condition is
     met.
   - **Design It Twice:** load [Design It
     Twice](technical-design/design-it-twice.md) when an open fork meets its
     hard-to-reverse triggers.
   - **Uncertainty disposition:** (a) settle statically checkable facts now
     from code, `Cargo.lock`, or primary docs; (b) if a locked crate source is
     genuinely uncached locally, record it as an unverified assumption with a
     concrete `Reopen if` in `Verified anchors` / `Assumptions`; (c) when a
     decision-changing runtime/integration behavior or hard budget can only be
     settled by execution, run an authorized [Research](research.md) empirical
     probe if available, or flag an `integration-first` entry in `Planning
     hooks` with a concrete `Reopen if`; (d) leave non-decisive local details
     behind a seam to Implementation.
   - *Complete step 2 only when* every triggered branch is closed, every genuine
     fork names a steelman rejected alternative, decisive constraint, accepted
     cost, and objective `Reopen if`, and every relied-on symbol and crate API
     is source-verified or carried as an uncached `Reopen if`.

3. **Persist draft artifact(s).** Write `specs/<task>/design/design.md` at
   `status: draft` using [Design V1](../interfaces/design-v1.md) through
   [Artifacts](../shared/artifacts.md), and `specs/<task>/rollout.md` at
   `status: draft` when [Release Closure](../rubrics/release-closure.md)
   requires a non-trivial rollout sequence.
   - *Complete step 3 only when* the draft artifact(s) exist on disk with every
     applicable `Design V1` section concretely bound.

4. **Run the Checkable Pre-Review Audit (static).** Attack only what is written
   on disk (never unwritten chat context) and close every gap in place:
   - **Trace & identifier parity:** every `R<n>`, `G<n>`, and `spec.md` carried
     concern sits in `Obligation trace` with owner and proving surface; zero
     `Non-goals` leaked in; every shared identifier across units is pinned.
   - **Deletion & steelman check:** every added structure, dependency, queue,
     config key, or fallback either traces directly to an explicit `spec.md`
     obligation or has a `D<n>` entry naming the invariant its deletion breaks
     and a serious simplest rejected alternative.
   - **Compile & caller walkthrough:** every anchor and proposed Rust signature
     matches inspected code, `Cargo.lock`, `quality/architecture.json`, and
     static compile reality; walk one realistic call site per new/changed seam.
   - **Failure, trust & handoff witness:** walk one worst-case
     failure/cancellation/race/attacker-path witness against the written design
     (when System Design triggered), and confirm `Shared mutated surfaces`
     uses canonical repo-relative paths matching [Task Packet
     V1](../interfaces/task-packet-v1.md).
   - *Complete step 4 only when* all four checks clear on the on-disk `draft`.

5. **Submit for independent review.** Dispatch fresh `reviewer-agent`(s)
   running [Technical Design Review](technical-design-review.md) under shared
   [Review](../shared/review.md):
   - Use **1 fresh `reviewer-agent`** by default (single triggered branch,
     small local design, or no-open-decisions result).
   - Use **2 parallel fresh `reviewer-agent` lanes** (never forking parent
     history) only when *both* System Design and Rust Architecture triggered
     and the design spans several crates or runtime crossings, or involves a
     hard-to-reverse fork or rollout—assigning `Axis 1` and `Axis 2` under
     [Technical Design Review Axes](technical-design-review.md#axes). Keep both
     reports distinct without masking findings across axes; recheck only failed
     or invalidated axes under shared [Review](../shared/review.md), and route
     one surviving evidence-backed conflict between axes to `adjudicator-agent`
     ([Review](../shared/review.md#route)).
   - Repair design-local `FAIL` findings in place and rerun affected step 4
     checks. For upstream gaps, spec falsification, or missing external
     authority, flip persisted `design/design.md` (and `rollout.md` when
     present) to `status: blocked` and return [Transition Result
     V1](../shared/transition.md#transition-result-v1) (`status: blocked`,
     `reopen_owner: ...`).
   - *Complete step 5 only when* review clears (`PASS` or `CONCERNS`) on the
     current candidate across all active axes.

6. **Finalize and hand off.** Emit [Transition Result
   V1](../shared/transition.md#transition-result-v1) (`status: ready`, `owner:
   Technical Design`, `review: <locator or inline result>`, `next_owner:
   Planning`) and stop:
   - *No-open-decisions result:* set `result` to the verified forced-owner
     evidence plus any carried review `CONCERNS`, without creating an empty
     `design.md`.
   - *Draft design:* record any bounded review `CONCERNS` (risk, owner,
     consuming action) and unresolved `spec.md` `Carried concerns` in
     `design.md`, flip `status: ready` in `design/design.md` (and `rollout.md`
     when present), and set `result: specs/<task>/design/design.md`.
   - *Complete step 6 only when* persisted statuses match the reviewed
     candidate, carried `CONCERNS` are preserved for Planning, and the receipt
     is emitted.
