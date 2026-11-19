# Technical Design

Enter from [Phase Selection](../../spec-first-workflow.md#phase-selection) or a
Specification handoff assigned to Technical Design. Own technical architecture autonomously under [Decision
Ownership](../../../AGENTS.md#decision-ownership) while preserving accepted observable behavior.

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
   - Establish the accepted baseline from ready `spec.md` and ready
     `intent.md` when present. For skipped Definition or behavior-preserving
     work, apply [Non-Standard Paths](technical-design/non-standard-paths.md#behavior-preserving-change-or-skipped-definition).
     Throughout authoring and review, use the selected baseline wherever a
     step refers to specification obligations.
   - Read settled intent (binding strength, rejected options and reasons,
     scenarios, priorities), baseline obligations (requirements and open
     markers, preserved guarantees, vocabulary, non-goals, handoffs, durable
     updates, assumptions, reopen triggers and concerns), prior research, and
     the workflow plan when present.
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
       `cargo metadata --locked --offline --format-version 1`).
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
     | Decision-changing static or external evidence gap (code or external contract) | Supporting [Research](research.md) **inside this phase** (inline, or `specs/<task>/research/<topic>.md` when reused downstream) |

   - Close concerns assigned to Technical Design or route the missing input to
     its owner. Carry only bounded risks with a downstream owner and action.
   - Resolve evidence gaps that can change the route. Send upstream blockers
     to [Blocked Exit](technical-design/non-standard-paths.md#blocked-exit).
     If all obligations are forced, terminology is reconciled, and Release
     Closure needs no non-trivial sequence, take the [No-Open-Decisions
     Exit](technical-design/non-standard-paths.md#no-open-decisions-exit).
     Otherwise continue with the open decisions or required rollout.

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
     `R<n>`, `G<n>`, invariant, or bound it breaks. Honor [Intent binding rules](../interfaces/intent.md#field-rules) and use
     requester priorities to break ties. Revisit a rejected repository choice
     only under its recorded reopen condition or an explicit user request.
   - **Design It Twice:** load [Design It
     Twice](technical-design/design-it-twice.md) for any **one-way door** open
     fork unless existing repository conventions (`docs/decisions/`, sibling
     subsystem, `quality/architecture.json`) fix one concrete shape with no
     viable competing topology or interface.
   - **Uncertainty disposition:**

     | Uncertainty | Action |
     | --- | --- |
     | Statically checkable in code, `Cargo.lock`, or primary docs | Settle now from primary sources. |
     | Non-obvious locked crate source genuinely uncached locally | Record a bounded gap in `Assumptions and reopen triggers`. If it could reverse the selected mechanism, obtain primary evidence or reopen the missing input. |
     | Runtime behavior, hard budget, or statically unproved Rust bound | Use an authorized read-only [Research](research.md) probe when needed. Defer proof with `Planning hooks: integration-first` and a concrete `Reopen if` only for a bounded risk over a closed choice. Route a missing decision-critical input to its owner. |
     | Non-decisive detail behind one seam | Leave open to Implementation. |

   - Falsified baseline or user-owned trade-off → [Blocked
     Exit](technical-design/non-standard-paths.md#blocked-exit).
   - Finish when the baseline has an owner and proving surface for each
     obligation, shared identifiers are pinned, terminology collisions are
     resolved, and each genuine fork has a decision per Design V1.

3. **Persist draft artifact(s).** Write `specs/<task>/design/design.md` (and
   `specs/<task>/rollout.md` when [Release
   Closure](../rubrics/release-closure.md) needs a non-trivial sequence) at
   `status: draft` per [Design V1](../interfaces/design-v1.md) via
   [Artifacts](../shared/artifacts.md), deleting superseded `design/*.md` or
   `rollout.md`.
   - *Done when:* draft artifact(s) are on disk, concretely bound per `Design
     V1`, with zero residual `Blocked decisions`.

4. **Run Author Pre-Flight (static, on-disk files only).** Repair gaps in place:
   - **Trace & handoff:** every baseline obligation sits in `Obligation trace`
     with owner and proving surface; zero `Non-goals` leaked; `Grounding and
     planning handoff` is complete for cold-start Planning.
   - **Walkthrough & witness:** when Rust Architecture triggered, walk every
     `S<n>` caller sketch line-by-line against primary signatures and compile
     reality; when System Design triggered, walk one worst-case failure or
     interleaving witness using only the draft and its accepted baseline.
   - **Deletion & seam purity:** every added mechanism or structure traces to
     the baseline or a steelman `D<n>`; remove internals behind one module's seam,
     task slicing, or test/command prescriptions.
   - *Done when:* all three checks clear on the on-disk `draft`.

5. **Dispatch independent review.** Use [Technical Design Review](technical-design-review.md#review-topology)
   to select lanes and scope their inputs. Apply the [Review Lifecycle](../shared/review.md#lifecycle)
   to findings. For confirmed local defects, repair the draft and repeat
   affected pre-flight checks. Changes to Rust seams or handoff invalidate
   the corresponding cold-start and Rust checks. Changes to system mechanisms,
   failure semantics or scope invalidate the corresponding system checks.
   For an upstream gap, preserve independent local repairs and take the
   [Blocked Exit](technical-design/non-standard-paths.md#blocked-exit).

6. **Finalize and hand off.** Once review clears, finalize the design and any
   rollout under [Design V1](../interfaces/design-v1.md#field-rules). Emit a
   ready [Transition Result V1](../shared/transition.md#transition-result-v1)
   naming Technical Design as owner, the reviewed design path as result, the
   review and active concerns as movement evidence, and Planning as next owner.
   Stop at that handoff.
