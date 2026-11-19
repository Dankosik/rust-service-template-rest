# Technical Design

Use when structured work has open technical choices—**two-engineer divergence**
across mechanisms, contracts, failure or trust semantics, rollout paths, or
seams. Own technical architecture autonomously under [Decision
Ownership](../../../AGENTS.md#decision-ownership) while preserving `spec.md`'s
observable behavior.

**Boundary rule:** Close every choice that can collide across parallel units,
alter a caller, wire, schema, config, or operator contract, break an invariant
or bound, or lock in a **one-way door** (persisted schema or durable state
machine, external wire or API contract, ≥2-consumer cross-crate public seam,
async durability/recovery topology, or new crate, dependency, or profile).
Internals behind one module's seam stay open to Implementation.

**Non-standard paths:** For reopen/resume from `blocked` or `ready`, a
behavior-preserving change (`Behavior delta: None`), a fixed external contract
skipping Definition, a no-open-decisions exit, or a blocked exit, load
[Non-Standard Technical Design Paths](technical-design/non-standard-paths.md).

## Steps

1. **Ground in primary sources and map the Decision Frontier (static —
   read-only; skip builds, tests, and live services).**
   - Verify `specs/<task>/spec.md` is `status: ready` (waived only for a
     skipped-Definition fixed external contract in [Non-Standard
     Paths](technical-design/non-standard-paths.md); otherwise exit via
     [Blocked Exit](technical-design/non-standard-paths.md#blocked-exit) with
     `reopen_owner: Specification`).
   - Read `specs/<task>/intent.md` (settled decisions `I<n>`, `Rejected`
     options, `Why`, and `Priority / tie-breakers`), `specs/<task>/spec.md`
     (all obligations:
     `R<n>` including every `Open to Technical Design:` marker, `G<n>`,
     `Terminology`, `Non-goals`, `Downstream handoff`, `Durable contract
     update`, `Assumptions/Reopen if`, and `Carried concerns`), prior
     `research/*.md`, and `workflow-plan.md` when present.
   - Verify baseline claims against **primary sources** in three groups
     (offload wide scans to a fresh `evidence-agent` under [Read-Only
     Delegation](../shared/read-only-delegation.md), verifying decision-critical
     facts directly):
     - **Code & sibling idioms:** affected crates, callers, exhaustive public
       workspace type fan-out, and **one existing sibling subsystem** (adopt its
       error, config, wiring, and telemetry idioms by default after checking its
       invariants; justify any deliberate divergence in `D<n>`).
     - **Architecture & prior decisions:** [Repository
       Architecture](../../repo-architecture.md),
       [`quality/architecture.json`](../../../quality/architecture.json),
       relevant `docs/decisions/*.md`, [Backend Library
       Selection](../../backend-library-selection.md), and affected [Production
       Contract](../../production-contract.md) sections.
     - **Contracts & locked crates:** `api/`, migrations, `Cargo.toml`
       `[workspace.lints]`, `clippy.toml`, `vendor/*/PATCHES.md`, and cached
       sources of non-obvious `Cargo.lock` crate APIs (`Cargo.lock` or
       `cargo metadata --locked --format-version 1`).
   - **Map the Decision Frontier** before proposing solutions: classify each
     obligation into (a) **forced** (zero realistic two-engineer divergence and
     zero unpinned `Open to Technical Design:` identifiers), (b) **open forks**
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

   - **Early exits:** route blockers/falsifications to [Blocked
     Exit](technical-design/non-standard-paths.md#blocked-exit); route zero
     open forks (with zero unpinned `Open to Technical Design:` identifiers and
     no non-trivial `rollout.md`) to [No-Open-Decisions
     Exit](technical-design/non-standard-paths.md#no-open-decisions-exit).
   - *Done when:* every obligation and `Open to Technical Design:` marker is
     source-verified, the Decision Frontier is classified, and zero
     decision-changing static gaps remain.

2. **Close open decisions across triggered branches.** Load only routed
   branches and the design-time skills triggered inside [System
   Design](technical-design/system-design.md) and [Rust
   Architecture](technical-design/rust-architecture.md) (keep `rust-coder`,
   `rust-testing`, `rust-debugging`, and `rust-verification` for later phases).
   When **both** branches trigger, **close System Design first** so Rust seams
   encode system invariants; if Rust compile reality falsifies a System Design
   assumption, update the System Design decision before step 3.
   - **Reuse-first & steelman:** before adding any mechanism, crate, module,
     trait, dependency, queue, fallback, or config key, steelman the strongest
     simpler substitute (`extend owner / collapse shallow chain -> std ->
     locked Cargo.lock crate -> maintained OSS -> custom`) and name the exact
     `R<n>`, `G<n>`, invariant, or bound it breaks. Honor rejected choices in
     `intent.md` (`I<n>`), `docs/decisions/`, and
     `docs/backend-library-selection.md` unless their reopen condition is met or
     the user explicitly requests it, and apply `intent.md`'s `Priority /
     tie-breakers` on surviving trade-offs.
   - **Design It Twice:** load [Design It
     Twice](technical-design/design-it-twice.md) for any **one-way door** open
     fork unless existing repository conventions (`docs/decisions/`, sibling
     subsystem, `quality/architecture.json`) fix one concrete shape with no
     viable competing topology or interface.
   - **Uncertainty disposition:**

     | Uncertainty | Action |
     | --- | --- |
     | Statically checkable in code, `Cargo.lock`, or primary docs | Settle now from primary sources. |
     | Non-obvious locked crate source genuinely uncached locally | Record in `Assumptions` (`Reopen if`), not `Verified anchors`. |
     | Decision-changing runtime behavior, hard budget, or statically unproved Rust bound | Run an authorized read-only [Research](research.md) probe without mutating repo source, or flag `Planning hooks: integration-first` + concrete `Reopen if`. |
     | Non-decisive detail behind one seam | Leave open to Implementation. |

   - Falsified `spec.md` or user-owned trade-off → [Blocked
     Exit](technical-design/non-standard-paths.md#blocked-exit).
   - *Done when:* every fork, `Open to Technical Design:` marker, and
     terminology collision is closed with steelman rationale and objective
     `Reopen if`, and every relied-on symbol/bound is verified or flagged for
     `Reopen if` / `integration-first`.

3. **Persist draft artifact(s).** Write `specs/<task>/design/design.md` at
   `status: draft` (removing resolved `Blocked decisions`) per [Design
   V1](../interfaces/design-v1.md) via [Artifacts](../shared/artifacts.md), and
   `specs/<task>/rollout.md` at `status: draft` when [Release
   Closure](../rubrics/release-closure.md) needs a non-trivial sequence. Delete
   superseded `design/*.md` or `rollout.md`.
   - *Done when:* draft artifact(s) are on disk, concretely bound per `Design
     V1`, with zero residual `Blocked decisions`.

4. **Run Author Pre-Flight (static, on-disk files only).** Repair gaps in place:
   - **Trace & handoff:** every `spec.md` obligation sits in `Obligation trace`
     with owner and proving surface; zero `Non-goals` leaked; `Grounding and
     planning handoff` is complete for cold-start Planning.
   - **Walkthrough & witness:** walk every `S<n>` caller sketch line-by-line
     against primary signatures and compile reality; when System Design
     triggered, walk one worst-case failure or interleaving witness using only
     the written `design.md` and `spec.md`.
   - **Deletion & seam purity:** every added mechanism or structure traces to
     `spec.md` or a steelman `D<n>`; remove internals behind one module's seam,
     task slicing, or test/command prescriptions.
   - *Done when:* all three checks clear on the on-disk `draft`.

5. **Dispatch and synthesize independent review.** Submit `draft` under
   [Review](../shared/review.md) to fresh `reviewer-agent` lane(s) running
   [Technical Design Review](technical-design-review.md) (fresh context window,
   never forking parent history):
   - **Brief hygiene:** on initial or fresh reviews, pass strictly the
     candidate paths (`specs/<task>/design/design.md`, linked `design/*.md`,
     `rollout.md`), input paths (`intent.md`, `spec.md` or cited external
     contract, plus `research/*.md` cited in `Basis` or `design/`—pass uncited
     `research/*.md` only to Axis 1 or after Phase A so cold-start blindness
     holds), assigned axis/checks, and method path
     (`technical-design-review.md`)—omit author rationale or "already verified"
     claims that break cold-start blindness or anchor the reviewer. Bounded
     delta rechecks add only anchored findings and the repair delta or refuting
     primary locators.
   - **Two-axis parallel (2 reviewers):** **both** System Design and Rust
     Architecture triggered **and** at least one applies: multiple crates or
     crossings, a **one-way door** fork, or a non-trivial rollout. Dispatch
     **Axis 1** and **Axis 2** under [Review
     Topology](technical-design-review.md#review-topology).
   - **Default (1 reviewer):** all other designs (all applicable checks under
     [Review Topology](technical-design-review.md#review-topology)).
   - **On `FAIL`:** verify citations/witnesses and handle refuted findings
     under [Review Lifecycle](../shared/review.md#lifecycle):
     - *Confirmed `PHASE_DEFECT`:* repair in place, rerun affected step 4
       checks, and re-run every failed or invalidated axis (an Axis 1 repair
       altering seams, types, or handoff surfaces invalidates Axis 2; an Axis 2
       repair altering system mechanisms, failure semantics, or scope
       invalidates Axis 1).
     - *Confirmed `UPSTREAM_GAP`:* preserve clean independent local repairs
       without another review, then take [Blocked
       Exit](technical-design/non-standard-paths.md#blocked-exit).
   - *Done when:* every active axis returns `PASS` or `CONCERNS` on the current
     candidate (with any surviving disputed finding closed via
     `adjudicator-agent`), or [Blocked
     Exit](technical-design/non-standard-paths.md#blocked-exit) returns.

6. **Finalize and hand off.** Record bounded `CONCERNS` and unresolved `spec.md`
   `Carried concerns` in `design.md`, flip `status: ready` in `design/design.md`
   (and `rollout.md` when present), emit [Transition Result
   V1](../shared/transition.md#transition-result-v1) (`status: ready`, `owner:
   Technical Design`, `result: specs/<task>/design/design.md`, `review:
   <locator or inline result>`, `next_owner: Planning`), and stop.
