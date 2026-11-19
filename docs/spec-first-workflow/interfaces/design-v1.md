# Design V1

`specs/<task>/design/design.md` is the reviewed technical architecture for one
structured task—closing every choice where two competent Rust engineers reading
`spec.md` and current code could build incompatible mechanisms, contracts,
failure or trust semantics, rollout paths, or seams.

```markdown
# Technical Design: <short technical outcome>

status: draft | ready | blocked
Basis: intent.md, spec.md[, research/<topic>.md ...][, rollout.md]
Carried concerns: <bounded review CONCERNS or unresolved spec concerns; omit when none>

## Obligation trace

| Obligation | Decision / Mechanism | Semantic owner | Proving surface |
| --- | --- | --- | --- |
| `spec.md#r1-...` / `G1` / carried concern | `D1` — <mechanism> or `forced:` <existing owner> | `<crate>::<module>` | <public seam, schema/probe, or durable effect; note required external input> |

## Key decisions

<Omit when every obligation is forced by existing owners.>

### D1 — <short decision name>

- **Choice:** <concrete mechanism, seam, or dependency>
- **Decisive constraint:** <specific `R<n>`, `G<n>`, invariant, bound, or verified source fact>
- **Strongest rejected alternative:** <steelman simplest/strongest substitute> — rejected because <exact obligation, invariant, or bound it breaks>
- **Accepted cost:** <concrete trade-off accepted>
- **Reopen if:** <objective condition or owner reversing the choice>
- **Durable record:** <`docs/decisions/...`, `docs/backend-library-selection.md`, or `docs/repo-architecture.md` update in Implementation, or `none`>

## System flows, invariants, and failure semantics

<Omit when System Design did not trigger.>

- **Flows, data & truth:** <trigger -> finality; canonical writer; atomic tx scope (zero network I/O in DB tx), schema/index/retention/cache rules; compact diagram only if clearer>
- **Failure, cancellation & concurrency:** <`CommitUnknown` recovery, retry owner/budget, idempotency key/store/TTL (in-flight / committed / expired), drop/cancel safety, serialization primitive, interleaving witness>
- **Trust & enforcement:** <enforcing boundary, closed bypass paths, AuthN/AuthZ & tenant isolation, secret/PII redaction, egress/parser bounds>
- **Budgets, cost & signals:** <deadline/retry/drain budget arithmetic (`OperationContext`, bootstrap); per-unit cost formula + hard limit; exact discriminating log/metric/span/probe signal per non-success path and readiness>
- **Deployment impact:** <`none`, `standard single-step deploy`, or link to `rollout.md` + mixed-version rule>

## Rust architecture, seams, and ownership

<Omit when Rust Architecture did not trigger.>

- **Semantic owners:** <`<crate>::<module>` (`extend | add | replace | delete`), `quality/architecture.json` role/`allow_dependents`/profile fit, superseded paths deleted>
- **Deep modules & seams:** <interface (types, invariants, ordering, error mapping, lifetimes), hidden complexity, dependency category/adapters, test surface; compact Rust signature + 3–8 line caller usage sketch per non-trivial new/changed seam>

## Grounding and planning handoff

- **Verified anchors:** <source-verified `path::Symbol`, `crate@version::item`, and schema/config anchors, plus any uncached crate in `Reopen if`>
- **Pinned shared identifiers:** <exact new/changed config keys, error codes, metric/span names, wire/event/job/table/column names, or cross-unit symbols; omit when none>
- **Shared mutated surfaces:** <canonical repo-relative lock paths actually mutated—e.g. `Cargo.toml`, `Cargo.lock`, `api/openapi/service.yaml`, `api/proto/`, `migrations/`, `.sqlx/`, `quality/architecture.json`, `crates/config/`, `crates/service/src/`, `crates/jobs-worker/src/`, `crates/migrate/src/`, `make/template.mk`, `template-owned.paths`, `scripts/template/profiles.json`, or shared crate `lib.rs`—or `none`>
- **Planning hooks:** <`prefactor`, `shared seam` (≥2 parallel consumers only), `integration-first`, or `expand -> migrate -> contract`; omit when none>

## Assumptions and reopen triggers

<Omit when none.>

- **<Bounded assumption>:** <assumption preserving accepted intent and spec> — **Reopen if:** <objective condition or owner>
```

## Field Rules

- **Lifecycle (`status`):** start at `draft`; flip to `ready` only after
  [Technical Design Review](../phases/technical-design-review.md) returns
  `PASS` or `CONCERNS` and carried obligations appear in `Carried concerns`.
  Use `blocked` when saving partial work alongside an upstream `blocked`
  handoff.
- **Proportionality & single home:** size `design.md` to the genuine open forks
  (one local choice can be half a page). Omit empty optional sections instead
  of writing `N/A`, split a large inventory into a linked sibling under
  `design/` only when that sharpens review, and link unchanged code, schemas,
  and `research/*.md` rather than copying them. Persist `specs/<task>/rollout.md`
  only when [Release Closure](../rubrics/release-closure.md) requires a
  non-trivial rollout sequence.
- **Concrete commitments:** bind exact keys, TTLs, budget formulas, error
  codes, signal names, serialization primitives, and canonical repo-relative
  lock paths so Planning and parallel Leads converge without inventing
  architecture. Preserve canonical nouns from `spec.md`, code, and
  `docs/repo-architecture.md`.
- **Steelman `D<n>` & traceability:** add `D<n>` only for genuine open choices
  (forced paths stay in `Obligation trace`); each `Strongest rejected
  alternative` must be a serious simplest/strongest substitute, never a
  strawman. Every `R<n>`, `G<n>`, and `spec.md` `Carried concerns` item appears
  in `Obligation trace`; Planning packets cite `design/design.md#d1-...`
  directly.
- **Seams & sketches, not implementation:** inline a compact Rust signature +
  3–8 line caller usage sketch, SQL constraint, state table, or exact
  wire/signing vector (with one deterministic non-secret vector for
  signature-sensitive shapes) when it pins an invariant better than prose.
  Keep function bodies, private file layout, task slicing, and test
  cases/fixtures/proving layers/commands out of `design.md` ([Evidence
  Contract](../shared/evidence-contract.md#design-proof)).
- **Durable record gate:** mark a durable update to `docs/decisions/`,
  [Backend Library Selection](../../backend-library-selection.md), or
  [Repository Architecture](../../repo-architecture.md) only when all three
  hold—(1) hard to reverse, (2) surprising without context, and (3) a real
  trade-off with a rejected alternative—or when reopening a previously rejected
  choice there. Implementation lands the doc update in the same change.
