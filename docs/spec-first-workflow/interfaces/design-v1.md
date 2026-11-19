# Design V1

`specs/<task>/design/design.md` is the reviewed technical architecture for one
structured task—closing every **two-engineer divergence** and **one-way door**
while leaving internals behind a single module's seam to Implementation.

```markdown
# Technical Design: <short technical outcome>

status: draft | ready | blocked
Basis: intent.md, spec.md | <cited external contract>[, research/<topic>.md ...][, rollout.md]
Carried concerns: <risk, downstream owner, closing action; omit when none>

## Obligation trace

| Obligation | Decision / Mechanism | Semantic owner | Proving surface |
| --- | --- | --- | --- |
| `spec.md#r1-...` (incl. `Open to Technical Design:`) / `G1` / handoff / durable update / concern | `D1` — <mechanism> or `forced:` <owner> | `<crate>::<module>` | <public endpoint/RPC/worker entry, `S<n>` seam, schema constraint, or durable effect; note required external input> |

## Key decisions

<Omit only when `design.md` is persisted with zero open forks (e.g. a non-trivial `rollout.md`); when zero open forks and no `rollout.md` apply, take [No-Open-Decisions Exit](../phases/technical-design/non-standard-paths.md#no-open-decisions-exit).>

### D1 — <short decision name>

- **Choice:** <concrete mechanism, seam, or dependency>
- **Decisive constraint:** <`R<n>`, `G<n>`, invariant, bound, or verified source fact>
- **Strongest rejected alternative:** <steelman substitute> — rejected because <exact obligation, invariant, or bound it breaks>
- **Accepted cost:** <concrete trade-off accepted>
- **Reopen if:** <objective condition or owner reversing the choice>

## System flows, invariants, and failure semantics

<Omit when System Design did not trigger; omit inapplicable subsections.>

### Flows and source of truth

<Entry -> finality (prefer State × Event or Crossing table); canonical writer; atomic tx scope (zero network I/O in tx); `PK`/`UNIQUE`/`FK`/`CHECK`, nullability/defaults, indexes, pruning/replay, cache rules.>

### Failure, concurrency, and trust contracts

<Applicable [System Probes](../phases/technical-design/system-probes.md) #2–#5 outputs: error/degradation matrix, `CommitUnknown`/idempotency, cancel/drop before & after commit, serialization + 4-step interleaving witness, trust/redaction bounds.>

### Budgets, config, and operator signals

<Applicable System Probes #6–#7 outputs: deadline/retry/drain arithmetic + single retry owner, per-unit/concurrency caps & shedding, config rules, bounded-cardinality signals, correlation, readiness/shutdown.>

### Rollout and mixed-version compatibility

<`no deployment impact`, `standard single-step deploy`, or `rollout.md`; lock-safe DDL + serialized payload compatibility; for `expand -> migrate -> contract`, state `same-release` vs `multi-release`.>

## Rust architecture, seams, and ownership

<Omit when Rust Architecture did not trigger.>

### Semantic owners and workspace graph

| Owner (`<crate>::<module>`) | Action | Invariant / Responsibility | Architecture & profile fit |
| --- | --- | --- | --- |
| `<crate>::<module>` | `extend \| add \| replace \| delete` | <owned invariant or seam> | <`quality/architecture.json` role / `allow_dependents` / profile / exhaustive fan-out> |

### S1 — <Module / Seam name>

- **Interface & invariants:** <compact Rust signature, error/retryability mapping, ordering, lifetimes>
- **Hidden complexity & adapters:** <what stays behind the seam, dependency category, proving seam>
- **Caller sketch:**
  ```rust
  // 3-8 line realistic call site against workspace types
  ```

## Grounding and planning handoff

- **Verified anchors:** <source-verified `path::Symbol`, `crate@version::item`, schema/config anchors; carry uncached crates or unproved bounds in `Assumptions` (`Reopen if`)>
- **Pinned shared identifiers:** <exact new/changed config keys, error codes, metric/span names, wire/event/job/table/column names, or cross-unit symbols closing every unfixed `Open to Technical Design:` identifier while preserving fixed `spec.md` contracts; omit when none>
- **Shared mutated surfaces:** <narrowest canonical repo-relative shared files or ordered directories actually mutated matching [Task Packet V1](task-packet-v1.md#field-rules) — manifests/policy, ordered/generated surfaces, shared wiring including parent `lib.rs`/`mod.rs`, concrete shared docs such as `docs/decisions/<topic>.md`, plus high-fan-out exhaustive public types — or `none`>
- **Superseded paths and generated contracts:** <code, config, contracts, or tests to delete; generated artifacts to regenerate (`api/`, `.sqlx/`, etc.); omit when none>
- **Durable doc updates:** <`docs/decisions/...`, `docs/backend-library-selection.md`, `docs/repo-architecture.md` (incl. `Domain Vocabulary`), or `docs/production-contract.md` updates citing `D<n>` or `spec.md`; omit when none>
- **Planning hooks:** <`prefactor`, `shared-contract` (≥2 lock-disjoint parallel consumers only), `integration-first`, or `expand -> migrate -> contract (same-release | multi-release)`; omit when none>

## Assumptions and reopen triggers

<Omit when none.>

- **<Bounded assumption>:** <assumption preserving accepted intent and spec> — **Reopen if:** <objective condition or owner>

## Blocked decisions

<Include only while `status: blocked`; remove once resolved.>

- **B1 — <upstream question or falsified rule; for `Intake`, phrase in requester language so `"yes"` accepts recommendation when supported>:** <primary evidence> | **Options & consequences:** <A vs B> | **Recommendation:** <choice and reason, or `none` when balanced>
```

## Field Rules

- **Proportionality & single home:** size `design.md` to actual open forks—one
  local choice fits on half a page (`Obligation trace`, `D1`, `S1`, `Grounding
  and planning handoff`). Omit inapplicable sections rather than writing `N/A`,
  split a large inventory into a linked sibling under `design/` only when it
  sharpens review, and link unchanged code, schemas, and `research/*.md`.
  Persist `specs/<task>/rollout.md` only when [Release
  Closure](../rubrics/release-closure.md) needs a non-trivial sequence.
- **Lifecycle (`status`) & ID stability:** start at `draft`; flip to `ready`
  only after [Technical Design Review](../phases/technical-design-review.md)
  permits handoff and bounded `CONCERNS` appear in `Carried concerns`. Use
  `blocked` with `Blocked decisions` whenever saving partial work or
  invalidating a persisted design on a blocked handoff. On reopen, keep
  surviving `D<n>` and `S<n>` IDs stable (retire deleted IDs instead of
  renumbering downstream-cited anchors).
- **Addressability & public seam contracts:** add `D<n>` only for genuine open
  forks and `S<n>` per non-trivial new or changed seam so Planning cites
  `design/design.md#d1-...` and `#s1-...` directly. Trace every `spec.md`
  obligation and pin exact keys, units, defaults, TTLs, budget formulas, error
  codes, signal names, serialization primitives, and narrow canonical lock
  paths using canonical nouns from `spec.md`, code, and
  `docs/repo-architecture.md`. Express contracts via compact Rust signatures,
  3–8 line caller sketches, SQL constraints, State × Event tables, and
  deterministic non-secret wire/signing vectors; leave function bodies, private
  file layout, task slicing, and test cases, fixtures, proving layers,
  commands, or unmandated live gates to Planning and Implementation.
- **Calibration bar:**
  - *Steelman `D<n>` vs strawman:* compare same-level substitutes (*"unique-key
    upsert + idempotent retry over Postgres advisory lock, rejected because
    session locks collide with PgBouncer transaction pooling"*) — never *"do
    nothing / put everything in one handler"*.
  - *Concrete vs vague `Reopen if`:* name a measurable or structural trigger
    (*"p99 write latency exceeds 50 ms at 200 rps, or a second crate outside
    `crates/service` needs the invariant"*) — never *"if performance is poor"*.
  - *Deep vs shallow `S<n>`:* expose 1–3 high-leverage entry points owning the
    invariant (*`OrderLedger::commit(&mut Tx<'_>, ValidatedOrder) ->
    Result<Receipt, CommitError>`*) — never granular CRUD pass-throughs leaking
    raw `PgPool` or internal state.
- **Durable record gate:** preserve every `Durable contract update` from
  `spec.md`, and add a durable update to `docs/decisions/`, [Backend Library
  Selection](../../backend-library-selection.md), [Repository
  Architecture](../../repo-architecture.md), or [Production
  Contract](../../production-contract.md) only when all three hold—(1)
  **one-way door**, (2) surprising without context, and (3) real trade-off with
  a rejected alternative—or when reopening a rejected choice or resolving a
  cross-task `Domain Vocabulary` collision. Implementation lands the doc update
  in the same change.
