# Design V1

`specs/<task>/design/design.md` is the reviewed technical architecture for one
structured task—closing every **two-engineer divergence** and **one-way door**
while leaving choices behind a single module's seam to Implementation.

```markdown
# Technical Design: <short technical outcome>

status: draft | ready | blocked
Basis: intent.md, spec.md[, research/<topic>.md ...][, rollout.md]
Carried concerns: <risk, downstream owner, closing action; omit when none>

## Obligation trace

| Obligation | Decision / Mechanism | Semantic owner | Proving surface |
| --- | --- | --- | --- |
| `spec.md#r1-...` / `G1` / handoff / durable update / concern | `D1` — <mechanism> or `forced:` <owner> | `<crate>::<module>` | <highest public seam, schema/probe, or durable effect; note required external input> |

## Key decisions

<Omit when every obligation is forced by existing owners.>

### D1 — <short decision name>

- **Choice:** <concrete mechanism, seam, or dependency>
- **Decisive constraint:** <`R<n>`, `G<n>`, invariant, bound, or verified source fact>
- **Strongest rejected alternative:** <steelman simplest/strongest substitute> — rejected because <exact obligation, invariant, or bound it breaks>
- **Accepted cost:** <concrete trade-off accepted>
- **Reopen if:** <objective condition or owner reversing the choice>

## System flows, invariants, and failure semantics

<Omit when System Design did not trigger; omit inapplicable subsections.>

### Flows and source of truth

<Entry -> finality; canonical writer; atomic tx scope (no network I/O in tx); schema/index/retention/cache rules. Prefer a State × Event or Crossing table when states or hops interact.>

### Failure, concurrency, and trust contracts

<Retryable vs terminal errors & degradation; `CommitUnknown` & idempotency; cancel/drop safety; serialization + 4-step interleaving witness; AuthN/AuthZ, redaction, egress/parser bounds.>

### Budgets, config, and operator signals

<Deadline/retry/drain arithmetic; unit cost, concurrency/queue cap & shedding; config section/units/defaults/validation; bounded-cardinality signals, correlation, readiness.>

### Rollout and mixed-version compatibility

<`none`, `standard single-step deploy`, or `rollout.md`; lock-safe DDL + serialized payload compatibility across rolling deploys.>

## Rust architecture, seams, and ownership

<Omit when Rust Architecture did not trigger.>

### Semantic owners and workspace graph

| Owner (`<crate>::<module>`) | Action | Invariant / Responsibility | Architecture & profile fit |
| --- | --- | --- | --- |
| `<crate>::<module>` | `extend \| add \| replace \| delete` | <owned invariant or seam> | <`quality/architecture.json` role / `allow_dependents` / profile> |

### S1 — <Module / Seam name>

- **Interface & invariants:** <compact Rust signature, error/retryability mapping, ordering, lifetimes>
- **Hidden complexity & adapters:** <what stays behind the seam, dependency category, proving seam>
- **Caller sketch:**
  ```rust
  // 3-8 line realistic call site against workspace types
  ```

## Grounding and planning handoff

- **Verified anchors:** <source-verified `path::Symbol`, `crate@version::item`, schema/config anchors, and uncached crates in `Reopen if`>
- **Pinned shared identifiers:** <exact new/changed config keys, error codes, metric/span names, wire/event/job/table/column names, or cross-unit symbols; omit when none>
- **Shared mutated surfaces:** <narrowest canonical repo-relative shared files or ordered directories actually mutated matching [Task Packet V1](task-packet-v1.md)—e.g. `Cargo.toml`, `migrations/`, shared `docs/decisions/*.md`, `crates/config/src/app.rs`, `crates/service/src/api.rs`, `crates/service/src/bootstrap/mod.rs`, or a crate `lib.rs`—or `none`>
- **Superseded paths and generated contracts:** <code, config, contracts, or tests to delete; generated artifacts to regenerate (`api/`, `.sqlx/`, etc.); omit when none>
- **Durable doc updates:** <`docs/decisions/...`, `docs/backend-library-selection.md`, `docs/repo-architecture.md` (incl. `Domain Vocabulary`), or `docs/production-contract.md` updates for Implementation, citing `D<n>` or `spec.md`; omit when none>
- **Planning hooks:** <`prefactor`, `shared-contract` (≥2 lock-disjoint parallel consumers only), `integration-first`, or `expand -> migrate -> contract`; omit when none>

## Assumptions and reopen triggers

<Omit when none.>

- **<Bounded assumption>:** <assumption preserving accepted intent and spec> — **Reopen if:** <objective condition or owner>

## Blocked decisions

<Include only while `status: blocked`; remove once resolved.>

- **B1 — <upstream question or falsified rule>:** <primary evidence> | **Options & consequences:** <A vs B> | **Recommendation:** <choice and reason>
```

## Field Rules

- **Proportionality & single home:** size `design.md` to actual open forks—one
  local choice fits on half a page (`Obligation trace`, `D1`, `S1`, `Grounding
  and planning handoff`). Omit inapplicable sections rather than writing `N/A`,
  split a large inventory into a linked sibling under `design/` only when it
  sharpens review, and link unchanged code, schemas, and `research/*.md`.
  Persist `specs/<task>/rollout.md` only when [Release
  Closure](../rubrics/release-closure.md) requires a non-trivial sequence.
- **Lifecycle (`status`) & ID stability:** start at `draft`; flip to `ready`
  only after [Technical Design Review](../phases/technical-design-review.md)
  permits handoff and bounded `CONCERNS` appear in `Carried concerns`. Use
  `blocked` with `Blocked decisions` whenever saving partial work or
  invalidating a persisted design on a blocked handoff. On reopen of a `ready`
  design, keep surviving `D<n>` and `S<n>` IDs stable (retire deleted IDs
  rather than renumbering downstream-cited anchors).
- **Addressability & concrete binding:** add `D<n>` only for genuine open forks
  (forced paths stay in `Obligation trace`) and `S<n>` per non-trivial new or
  changed seam so Planning cites `design/design.md#d1-...` and `#s1-...`
  directly. Trace every `spec.md` obligation (`R<n>`, `G<n>`, `Technical
  Design` `Downstream handoff`, `Durable contract update`, `Carried concerns`).
  Bind exact keys, units, defaults, TTLs, budget formulas, error codes, signal
  names, serialization primitives, and narrow canonical lock paths using
  canonical nouns from `spec.md`, code, and `docs/repo-architecture.md`.
- **Seams & sketches, not implementation:** pin invariants with compact Rust
  signatures, 3–8 line caller sketches, SQL constraints, State × Event tables,
  or deterministic non-secret wire/signing vectors. Leave function bodies,
  private file layout, task slicing, and test cases, fixtures, proving layers,
  commands, or unmandated live gates to Planning and Implementation.
- **Durable record gate:** preserve every `Durable contract update` from
  `spec.md`, and add a durable update to `docs/decisions/`, [Backend Library
  Selection](../../backend-library-selection.md), or [Repository
  Architecture](../../repo-architecture.md) only when all three hold—(1)
  **one-way door**, (2) surprising without context, and (3) real trade-off with
  a rejected alternative—or when reopening a rejected choice or resolving a
  cross-task `Domain Vocabulary` collision. Implementation lands the doc update
  in the same change.
