# Technical Design

Use when structured work has open technical choices—where two competent Rust
engineers reading `spec.md` and current code could build incompatible
mechanisms, contracts, failure semantics, rollout paths, or crate/module seams.
Own technical architecture autonomously under [Decision
Ownership](../../../AGENTS.md#decision-ownership). Preserve `spec.md`'s
observable behavior; leave task slicing and write locks to Planning, and
function bodies, private file layout, and test implementation to
Implementation.

## Steps

1. **Reconcile obligations and ground in source truth.** Read
   `specs/<task>/intent.md`, `specs/<task>/spec.md` (every `R<n>`, `G<n>`,
   `Terminology`, and `Carried concerns`), any `research/*.md`, and
   `workflow-plan.md` when present. Inspect **primary repository sources**
   before proposing any mechanism or seam: affected crates and callers,
   [Repository Architecture](../../repo-architecture.md) (including `Domain
   Vocabulary`), [`quality/architecture.json`](../../../quality/architecture.json),
   relevant `docs/decisions/*.md`, [Backend Library
   Selection](../../backend-library-selection.md), and [Production
   Contract](../../production-contract.md) when capacity, consistency, trust,
   or recovery applies.
   - Map every `R<n>`, `G<n>`, and carried concern to the branch(es) it opens:
     - runtime boundary, source of truth, material flow, failure/cancellation/race
       behavior, unit cost, operator signal, or rollout -> [System
       Design](technical-design/system-design.md);
     - Rust placement, deep-module seams, invariant ownership, caller
       obligations, dependency direction (even with fixed files), or
       generated/manual containment not mechanically forced by current code and
       `quality/architecture.json` -> [Rust
       Architecture](technical-design/rust-architecture.md);
     - deployable, worker/job, shared wire contract, schema/migration, runtime
       config/trust material, or managed dependency impact -> [Release
       Closure](../rubrics/release-closure.md);
     - missing current or external evidence that can change a technical choice
       -> run supporting [Research](research.md) **inside this phase** (inline
       for a bounded lookup, or via `evidence-agent` for read-heavy discovery;
       persist `specs/<task>/research/<topic>.md` when the evidence must
       survive for review or refresh).
   - **Early exits:**
     - Missing or contradictory product behavior, or a technical trade-off
       whose only differentiator is a user-owned choice (cost, external SLA,
       unauthorized breaking change, legal/irreversible external effect):
       collect all independent upstream blockers in one pass, save resolved
       independent design work in `specs/<task>/design/design.md` at `status:
       blocked` when useful, and return [Transition Result
       V1](../shared/transition.md#transition-result-v1) (`status: blocked`,
       `reopen_owner: Specification` or `Intake`—never ask the user to choose a
       technical mechanism).
     - Every obligation is mechanically forced by current code,
       `quality/architecture.json`, and `spec.md` (zero open technical
       choices): skip drafting `design.md` and submit the grounded evidence
       naming each existing owner directly to step 5.
   - *Complete this step only when* every `R<n>`, `G<n>`, and carried concern
     maps to a triggered branch or forced owner, current code and prior
     decisions (`docs/decisions/`, `docs/backend-library-selection.md`) are
     verified against primary sources, and no decision-changing evidence gap
     remains open.

2. **Execute triggered branches and challenge every choice.** Load only the
   branches triggered in step 1 and only the domain skills whose pressure
   affects an open decision:
   - REST/OpenAPI wiring -> `rust-api-contract`, `rust-axum`;
   - PostgreSQL pools, transactions, or migrations -> `rust-sqlx`;
   - failure mapping, `ServiceFailure`, or degradation -> `rust-errors`;
   - trust, secrets, authorization, signing, or exact bytes -> `rust-security`;
   - config keys, defaults, precedence, or validation -> `rust-config`;
   - deadlines, retries, overload, readiness, or drain -> `rust-reliability`;
   - operator signals, redaction, or metrics/spans/probes -> `rust-observability`;
   - task lifetimes, cancellation, locks, or channels -> `rust-tokio`;
   - numeric scale, latency, memory, or unit-cost bounds -> `rust-performance`;
   - new crate, feature, or custom mechanism vs library -> `rust-dependencies`;
   - structural ownership, deletion test, or polymorphism -> `rust-structural-quality`;
   - CI gates, tool pins, container image, or publication -> `rust-delivery-platform`.

   - **Simpler-alternative challenge:** before adopting any new mechanism,
     crate, module, trait, layer, dependency, queue, or config key, construct
     the strongest simpler alternative (extending an existing owner, `std`, or a
     locked crate) and prove which concrete obligation, invariant, or bound it
     violates. Apply the **deletion test** and honor every rejected choice in
     `docs/decisions/` and `docs/backend-library-selection.md` unless its
     stated reopen condition is met.
   - **Hard-to-reverse open fork:** when an expensive-to-reverse mechanism or
     cross-boundary seam has genuinely competing architectures, load [Design It
     Twice](technical-design/design-it-twice.md) to explore contrasting designs
     in parallel fresh contexts before converging autonomously.
   - **Dependency source truth:** verify every non-obvious consumed symbol and
     external crate API against current code and the source of the version
     locked in `Cargo.lock` (or official primary source when admitting a new
     dependency). If a locked crate source is uncached locally, record the
     unverified assumption with a concrete `Reopen if` instead of relying on
     memory.
   - *Complete this step only when* every triggered branch is closed, every
     non-trivial choice names its decisive constraint, strongest rejected
     alternative, accepted cost, and reopen trigger, and every relied-on
     repo/crate API is source-verified.

3. **Persist draft design artifact(s).** Write `specs/<task>/design/design.md`
   at `status: draft` using [Design V1](../interfaces/design-v1.md) through
   [Artifacts](../shared/artifacts.md) (splitting a large inventory into a
   linked sibling file under `design/` only when it clarifies review). When
   [Release Closure](../rubrics/release-closure.md) finds a non-trivial rollout
   sequence, also write `specs/<task>/rollout.md` at `status: draft`. Preserve
   canonical vocabulary from `spec.md` and `docs/repo-architecture.md`, and
   link unchanged code, schemas, and `research/*.md` instead of copying them.

4. **Run the Technical Design Dry Run (static — run no builds, tests, or
   services).** Attack the complete `draft` against all four checks and close
   surviving gaps in place before review:
   - **Obligation coverage & spec fidelity:** try to implement the entire
     design faithfully while breaking any `R<n>`, `G<n>`, or `Carried concerns`
     item in `spec.md`, or silently adding unrequested behavior; close every
     loophole.
   - **Deletion & simplicity test:** challenge every added component, crate,
     module, trait, dependency, queue, config key, and fallback; delete or
     collapse anything whose removal breaks no accepted invariant, boundary, or
     bound.
   - **Source, architecture & compile reality:** confirm zero contradictions
     with current code, `Cargo.lock` crate sources, `quality/architecture.json`
     role/dependency rules, optional template profile isolation,
     `docs/decisions/`, `docs/backend-library-selection.md`, and Rust's static
     type/lifetime/object-safety rules.
   - **Planning cold-start:** walk `intent.md`, `spec.md`, `design/design.md`
     (and `rollout.md`), and cited code with zero chat history. Confirm a fresh
     Planning session can slice vertical tasks, spot prerequisite shared seams,
     integration risk, or wide blast radius, and assign disjoint `Mutable
     owners` and `Exclusive locks` without inventing architecture or guessing
     private file trees.

5. **Submit for independent review.** Select the review topology under
   [Review Topology](technical-design-review.md#review-topology)—1 fresh
   `reviewer-agent` by default, or 2 parallel fresh `reviewer-agent` lanes
   (System/Flows/Ops + Rust Architecture/Codebase Truth) when both branches are
   material, several crates or runtime boundaries change, or a hard-to-reverse
   restructure is in play—and route the fixed `draft` candidate (or
   no-open-decisions evidence) through shared [Review](../shared/review.md).
   Repair design-local `FAIL` findings in place and rerun affected step 4
   checks; return `status: blocked` with `reopen_owner` in [Transition Result
   V1](../shared/transition.md#transition-result-v1) for upstream product gaps
   or missing external authority.

6. **Finalize and hand off.** After review returns `PASS` or `CONCERNS`:
   - for a reviewed no-open-decisions result, return [Transition Result
     V1](../shared/transition.md#transition-result-v1) (`status: ready`,
     `owner: Technical Design`, `result: <verified forced-owner evidence>`,
     `review: <locator or inline result>`, `next_owner: Planning`) without
     creating an empty `design.md`;
   - otherwise record any bounded `CONCERNS` obligation from review (risk,
     owner, and consuming action) plus any still-unresolved `Carried concerns`
     from `spec.md` in `Carried concerns` inside
     `specs/<task>/design/design.md` so the fresh Planning session receives
     them, set `status: ready` in `design/design.md` (and mark `rollout.md`
     ready when present), and return [Transition Result
     V1](../shared/transition.md#transition-result-v1) (`status: ready`,
     `owner: Technical Design`, `result: specs/<task>/design/design.md`,
     `review: <locator or inline result>`, `next_owner: Planning`). Stop here;
     leave task slicing to Planning's fresh session.
