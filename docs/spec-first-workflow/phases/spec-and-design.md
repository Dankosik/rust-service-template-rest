# Spec And Design

Continue autonomously in the same session after [Intake](intake.md) confirms
shared understanding. Own both the observable behavioral contract (**what**)
and technical architecture (**how**) under [Decision
Ownership](../../../AGENTS.md#decision-ownership), leaving behind-seam
internals to Implementation.

## Steps

1. **Ground in primary sources (static, read-only).**
   - Verify every assumption against real code, callers, schemas, migrations,
     config, `api/`, `Cargo.toml` `[workspace.lints]`, `clippy.toml`,
     `vendor/*/PATCHES.md`, locked crate sources (`cargo metadata --locked
     --offline --format-version 1`),
     [`quality/architecture.json`](../../../quality/architecture.json),
     [Repository Architecture](../../repo-architecture.md), `docs/decisions/`,
     [Backend Library Selection](../../backend-library-selection.md), and
     [Production Contract](../../production-contract.md).
   - Inspect **one existing sibling subsystem** and adopt its error, config,
     wiring, and telemetry idioms by default; justify any deliberate divergence
     in `D<n>`.
   - Offload wide multi-crate scans to a fresh `evidence-agent` via [Read-Only
     Delegation](../shared/read-only-delegation.md), verifying decision-critical
     anchors directly. Revisit a rejected `docs/decisions/` choice only on its
     recorded `Reopen if` or an explicit user mandate.
   - If primary evidence contradicts a confirmed user premise, report the
     verified discrepancy directly to the user instead of authoring around it.
     If primary evidence shows the request already holds with zero delta,
     verify that claim via [Plan Review](plan-review.md#zero-delta-check)
     before reporting completion to the user.
   - *Done when:* affected surfaces, callers, exhaustive public types, sibling
     idioms, and baseline constraints are verified in primary sources.

2. **Close observable behavior (WHAT).**
   - Walk [Behavioral Probes](specification/behavioral-probes.md), loading
     `rust-api-contract`, `rust-security`, `rust-config`, `rust-reliability`,
     `rust-observability`, or `rust-performance` only for unresolved
     external-observer questions.
   - Pin externally relied-on rules (`R<n>`), trust and failure guarantees,
     client wire identifiers, and concrete unchanged guarantees (`G<n>`). For
     behavior-preserving work, record `Behavior delta: None —
     behavior-preserving change` and pin concrete `G<n>` plus measurable
     targets.
   - Use compact state-transition or precedence tables inside the owning `R<n>`
     when states interact, and pair each `R<n>` with a non-tautological
     **Falsifier** at the highest external seam.
   - Dry-run the hardest 1–2 unhappy or concurrent paths to verify that no
     literal reading defeats the success signal or allows two implementations to
     diverge externally.
   - Escalate to the user only for an unresolved product/risk fork that cannot
     be settled within confirmed intent and repository policy.
   - *Done when:* every confirmed requirement, guarantee, and probe has a
     closed, falsifiable behavioral rule.

3. **Close technical architecture (HOW).**
   - **Boundary rule:** close every choice that can collide across parallel
     tasks, alter a caller, wire, schema, config, or operator contract, break an
     invariant or bound, or lock in a **one-way door** (persisted schema or
     durable state machine, external wire/API contract, ≥2-consumer cross-crate
     seam, async durability/recovery topology, or new crate, dependency, or
     profile). Leave internals behind one module's seam open to Implementation.
   - Disclose only the branches this change triggers (keep `rust-coder`,
     `rust-testing`, `rust-debugging`, and `rust-verification` for
     Implementation):

     | Trigger | Load |
     | --- | --- |
     | Runtime crossings, data/tx, truth, trust, failure/concurrency, budgets, config, signals, or rollout | [System Design](technical-design/system-design.md) + [System Probes](technical-design/system-probes.md) |
     | Rust modules, deep seams, error/invariant ownership, workspace graph, or profiles | [Rust Architecture](technical-design/rust-architecture.md) |
     | Qualifying one-way-door open fork with competing topologies or interfaces | [Design It Twice](technical-design/design-it-twice.md) |
     | Non-trivial deploy, migration, worker, contract, or trust-material sequence | [Release Closure](../rubrics/release-closure.md) |
     | Decision-changing external contract or empirical runtime uncertainty | [Research](research.md) (persist `specs/<task>/research/<topic>.md` when reused) |

   - When both System Design and Rust Architecture trigger, close **System
     Design first** so Rust seams encode system invariants; revise System Design
     if Rust compile reality falsifies an assumption.
   - Apply the **reuse-first steelman ladder** before adding any mechanism,
     crate, module, trait, dependency, queue, fallback, or config key: `extend
     owner / collapse shallow chain -> std -> locked Cargo.lock crate ->
     maintained OSS -> custom`. Name the exact `R<n>`, `G<n>`, invariant, or
     bound the simpler substitute breaks.
   - Handle remaining uncertainty explicitly: settle statically checkable facts
     now; run a bounded read-only [Research](research.md) probe for decisive
     runtime/bound unknowns; defer bounded proof over a closed choice via an
     `integration-first` slice and objective `Reopen if`.
   - *Done when:* every obligation maps to a semantic owner and proving surface,
     shared identifiers and mutated surfaces are pinned, `S<n>` caller sketches
     match live workspace types, and every genuine fork has a steelman `D<n>`.

4. **Write `specs/<task>/spec.md` (and `specs/<task>/design.md` if large).**
   - Persist `specs/<task>/spec.md` at `status: draft` using [Artifact
     Format](#artifact-format).
   - Keep `## Architecture` inside `spec.md` for focused changes. Split it into
     `specs/<task>/design.md` (starting with `status: draft`) when the
     architecture spans multiple crates, complex crossing/seam tables, or
     non-trivial rollout gates so `spec.md` stays readable. Embed non-trivial
     [Release Closure](../rubrics/release-closure.md) gates directly under
     `### Flows, failure semantics & rollout`.
   - Continue directly into [Planning](planning.md).
   - *Done when:* `spec.md` (and `design.md` if split) is on disk at
     `status: draft` with zero unresolved forks, ready for cold-start slicing
     and review.

## Artifact Format

Omit empty optional sections. Link unchanged code, `api/`, migrations, and
`research/*.md` instead of copying them.

````markdown
# Spec: <short observable outcome>

status: draft | ready

## Context & success signal
<Problem, requester priorities/tie-breakers, concrete scenarios, and how success is observed>

## Actors, baseline & terminology
- **Actors & seams:** <actor + trust boundary -> route, RPC, CLI, or worker — locator>
- **Baseline:** <verified current behavior shaping the delta — locator>
- **Terms:** **<Term>** = <canonical meaning> (distinguished from <overloaded sense>)

## Behavior delta
### R1 — <short rule name>
<Observable rule, invariant, labeled numeric target, or state/precedence table.>
- **Falsifier:** <concrete state + trigger -> wrong outcome at the highest external seam>

## Preserved guarantees
- **G1 — <name>:** <concrete unchanged external invariant for a named surface — locator>

## Non-goals
- <plausible adjacent work explicitly excluded>

## Architecture *(inline here, or split to `specs/<task>/design.md` when large)*

### Decisions
#### D1 — <short decision name>
- **Choice:** <mechanism, seam, or dependency> (driven by <`R<n>`, `G<n>`, invariant, or bound>)
- **Rejected steelman:** <strongest simpler substitute> — breaks <exact obligation or bound>
- **Accepted cost & Reopen if:** <trade-off; measurable or structural reopen trigger>

### Flows, failure semantics & rollout
<Entry-to-finality table, System Probes #1–#7 contracts, and deployment impact (`no deployment impact`, `standard single-step deploy`, or embedded Release Closure gates; state `same-release` vs `multi-release` for staged refactors).>

### Rust seams & ownership
| Owner (`<crate>::<module>`) | Action | Invariant / Responsibility | Architecture & profile fit |
| --- | --- | --- | --- |
| `<crate>::<module>` | `extend \| add \| replace \| delete` | <owned invariant> | <`quality/architecture.json` role / profile / exhaustive fan-out> |

#### S1 — <Module / Seam name>
- **Interface & invariants:** <compact signature, error/retryability mapping, ordering, lifetimes, dependency category>
- **Caller sketch:**
  ```rust
  // 3-8 line realistic call site against live workspace types
  ```

### Planning handoff
- **Verified anchors:** <`path::Symbol`, `crate@version::item`, schema/config anchors>
- **Pinned shared identifiers:** <new/changed config keys, error codes, metrics, wire/table names>
- **Shared mutated surfaces:** <canonical repo-relative lock paths + high-fan-out exhaustive types>
- **Superseded paths & generated contracts:** <code/tests to delete; `api/`, `.sqlx/`, etc. to regenerate>
- **Durable doc updates:** <updates to `docs/decisions/`, `docs/repo-architecture.md`, `docs/production-contract.md`, or `docs/backend-library-selection.md` when one-way door + surprising + real trade-off, or overturning a rejected choice>
- **Planning hooks:** <applicable shape tags from Planning, if any>

## Assumptions & reopen triggers
- **A1 — <name>:** <bounded assumption preserving intent> — **Reopen if:** <objective trigger or owner>
````

### Calibration Bar

- **Strong vs tautological Falsifier:** *Strong:* `POST /orders` replays an
  `Idempotency-Key` with a modified body and returns `200` (or creates a second
  order) instead of `409 idempotency_conflict`. *Weak:* repeating the rule
  verbatim or naming a unit test / `just` command (tests belong to
  Implementation per [Evidence Contract](../shared/evidence-contract.md#design-proof)).
- **Steelman vs strawman `D<n>`:** compare same-level substitutes (*"unique-key
  upsert + idempotent retry over Postgres advisory lock, rejected because
  session locks collide with PgBouncer transaction pooling"*), using measurable
  or structural `Reopen if` triggers.
- **Deep vs shallow `S<n>`:** expose 1–3 high-leverage entry points owning the
  invariant (*`OrderLedger::commit(&mut Tx<'_>, ValidatedOrder) ->
  Result<Receipt, CommitError>`*), hiding wiring and storage details behind the
  seam.
