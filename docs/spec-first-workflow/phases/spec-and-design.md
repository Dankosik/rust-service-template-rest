# Spec And Design

Continue autonomously in the same session after [Intake](intake.md) confirms
shared understanding. Own both the observable behavioral contract (**what**)
and technical architecture (**how**) under [Decision
Ownership](../../../AGENTS.md#decision-ownership), leaving behind-seam
internals to Implementation.

## Steps

1. **Ground in primary sources (static, read-only).**
   - Verify assumptions against touched code, callers, schemas, migrations,
     config, `api/`, [Repository Architecture](../../repo-architecture.md),
     `docs/decisions/`, and [Production Contract](../../production-contract.md).
     When proposing Rust seams or crate usage, also ground against
     [`quality/architecture.json`](../../../quality/architecture.json),
     `Cargo.toml` `[workspace.lints]`, `clippy.toml`, `vendor/*/PATCHES.md`,
     [Backend Library Selection](../../backend-library-selection.md), and
     `cargo metadata --locked --offline --format-version 1`.
   - Inspect **one existing sibling subsystem** and adopt its error, config,
     wiring, and telemetry idioms by default; justify any divergence in `D<n>`.
     Offload wide multi-crate scans to a fresh `evidence-agent` via [Read-Only
     Delegation](../shared/read-only-delegation.md), verifying decision-critical
     anchors directly.
   - Surface any verified premise contradiction directly to the user instead of
     authoring around it; if the request already holds with zero delta, verify
     via [Plan Review](plan-review.md#zero-delta-check) before reporting
     completion.
   - *Done when:* affected surfaces, callers, exhaustive public types, sibling
     idioms, and baseline constraints are verified in primary sources.

2. **Close observable behavior (WHAT).**
   - Walk [Behavioral Probes](specification/behavioral-probes.md), loading
     `rust-api-contract`, `rust-security`, `rust-config`, `rust-reliability`,
     `rust-observability`, or `rust-performance` only for unresolved
     external-observer questions.
   - Pin externally relied-on rules (`R<n>`, using state/precedence tables when
     states interact), non-tautological external-seam **Falsifiers**, client
     wire identifiers, and unchanged guarantees (`G<n>`). For
     behavior-preserving work, record `Behavior delta: None —
     behavior-preserving change` with concrete `G<n>` and measurable targets.
   - Dry-run the hardest 1–2 unhappy or concurrent paths so no literal reading
     defeats the success signal or lets two implementations diverge externally.
     Escalate to the user only for an unresolved product/risk fork.
   - *Done when:* every confirmed requirement, guarantee, and probe has a
     closed, falsifiable behavioral rule.

3. **Close technical architecture (HOW).**
   - **Boundary rule:** close every choice that can collide across parallel
     tasks, alter a caller, wire, schema, config, or operator contract, break an
     invariant or bound, or lock in a **one-way door**. One-way doors include
     persisted schemas or durable state machines, external wire/API contracts,
     `≥2`-consumer cross-crate seams, async durability/recovery topologies, and
     new crates, dependencies, or profiles. Leave internals behind one module's
     seam open to Implementation.
   - Load only the triggered branches (keep `rust-coder`, `rust-testing`,
     `rust-debugging`, and `rust-verification` for Implementation):

     | Trigger | Load |
     | --- | --- |
     | Runtime crossings, data/tx, truth, trust, failure/concurrency, budgets, config, signals, or rollout | [System Design](technical-design/system-design.md) + [System Probes](technical-design/system-probes.md) |
     | Rust modules, deep seams, error/invariant ownership, workspace graph, or profiles | [Rust Architecture](technical-design/rust-architecture.md) |
     | Qualifying one-way-door open fork with competing topologies or interfaces | [Design It Twice](technical-design/design-it-twice.md) |
     | Non-trivial deploy, migration, worker, contract, or trust-material sequence | [Release Closure](../rubrics/release-closure.md) |
     | Decision-changing external contract or empirical runtime uncertainty | [Research](research.md) (persist `specs/<task>/research/<topic>.md` when reused) |

   - When both trigger, close **System Design before Rust Architecture** (revise
     System Design if compile reality falsifies an assumption).
   - Apply the **reuse-first steelman ladder** before adding any mechanism,
     crate, module, trait, dependency, queue, fallback, or config key: `extend
     owner / collapse shallow chain -> std -> locked Cargo.lock crate ->
     maintained OSS -> custom`—naming the exact `R<n>`, `G<n>`, invariant, or
     bound the simpler substitute breaks.
   - Handle remaining uncertainty explicitly: settle static facts now, run a
     bounded [Research](research.md) probe for decisive runtime/bound unknowns,
     or defer bounded proof over a closed choice via an `integration-first`
     slice and objective `Reopen if`.
   - *Done when:* every obligation maps to a semantic owner and proving surface,
     shared identifiers and mutated surfaces are pinned, `S<n>` caller sketches
     match live workspace types, and every genuine fork has a steelman `D<n>`.

4. **Write `specs/<task>/spec.md` (split `specs/<task>/design.md` if large).**
   - Persist `spec.md` at `status: draft` using [Artifact
     Format](#artifact-format). Keep `## Architecture` inline for focused
     changes; split it into `design.md` (`status: draft`) when architecture
     spans multiple crates, complex crossing tables, or non-trivial rollout
     gates. Continue directly into [Planning](planning.md).
   - *Done when:* `spec.md` (and `design.md` if split) is on disk at
     `status: draft` with zero unresolved forks, ready for cold-start slicing
     and review.

## Artifact Format

Omit empty optional sections; link unchanged code, `api/`, migrations, and
`research/*.md` instead of copying them. Keep durable IDs (`R<n>`, `G<n>`,
`D<n>`, `S<n>`, `A<n>`) stable on reopen.

- **Header:** `# Spec: <short observable outcome>` and `status: draft | ready`.
- **`## Context & success signal`:** problem, requester priorities/tie-breakers,
  concrete scenarios, and observable success signal.
- **`## Actors, baseline & terminology`:** actors/trust boundaries → seams with
  locators; verified baseline delta; canonical definitions for overloaded terms.
- **`## Behavior delta`:** `### R<n> — <name>` (observable rule, labeled numeric
  target, or state/precedence table + external-seam **Falsifier**).
- **`## Preserved guarantees`:** `G<n>` unchanged invariants with locators.
- **`## Non-goals`:** plausible adjacent work explicitly excluded.
- **`## Architecture`** *(inline, or split to `specs/<task>/design.md`)*:
  - `### Decisions` — `#### D<n>`: choice (citing `R<n>`/`G<n>`/bound),
    rejected steelman (what it breaks), accepted cost + objective `Reopen if`.
  - `### Flows, failure semantics & rollout` — entry-to-finality / crossing
    table, applicable System Probes #1–#7 contracts, deployment impact (`no
    deployment impact`, `standard single-step deploy`, or embedded [Release
    Closure](../rubrics/release-closure.md) gates; state `same-release` vs
    `multi-release` for staged refactors).
  - `### Rust seams & ownership` — touched `<crate>::<module>` actions
    (`extend | add | replace | delete`), owned invariants,
    `quality/architecture.json` / profile / exhaustive fan-out fit, and
    `#### S<n>` (compact signature, error/retryability/lifetime contract,
    dependency category, and 3–8 line realistic caller sketch against live
    workspace types).
  - `### Planning handoff` — **Verified anchors** (`path::Symbol`,
    `crate@version::item`, schemas/config); **Pinned shared identifiers** (keys,
    error codes, metrics, wire/table names); **Shared mutated surfaces**
    (canonical lock paths + high-fan-out exhaustive types); **Superseded paths &
    generated contracts**; **Durable doc updates** (`docs/decisions/`,
    `docs/repo-architecture.md`, `docs/production-contract.md`,
    `docs/backend-library-selection.md` when one-way door + surprising + real
    trade-off, or overturning a rejected choice); **Execution risks & staging**
    (unverified integration seams, wide-refactor blast radius, multi-release
    boundaries).
- **`## Assumptions & reopen triggers`:** `A<n>` bounded assumptions + objective
  `Reopen if`.

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
