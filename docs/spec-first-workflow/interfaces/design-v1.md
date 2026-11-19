# Design V1

`specs/<task>/design/design.md` is the reviewed technical architecture for one
structured task. It closes every technical choice where two competent Rust
engineers reading `spec.md` and current code could build incompatible
mechanisms, contracts, failure semantics, or crate/module seams—preserving
`spec.md`'s observable behavior while leaving task slicing to Planning and
function bodies, private file layout, and test implementation to
Implementation.

```markdown
# Technical Design: <short technical outcome>

status: draft | ready | blocked
Basis: intent.md, spec.md[, research/<topic>.md ...][, rollout.md]
Carried concerns: <bounded review CONCERNS carried downstream; omit when none>

## Obligation trace

| Obligation | Decision / Mechanism | Semantic owner |
| --- | --- | --- |
| `spec.md#r1-...` / `G1` / carried concern | `D1` — <selected mechanism or forced existing path> | `<crate>::<module>` |

## Key decisions

### D1 — <short decision name>

- **Choice:** <selected mechanism, seam, or dependency>
- **Decisive constraint:** <accepted obligation, invariant, or verified source fact forcing this choice>
- **Strongest rejected alternative:** <simplest or strongest viable substitute> — rejected because <concrete invariant, cost, or constraint it violates>
- **Accepted cost:** <trade-off accepted>
- **Reopen if:** <objective condition or owner that would reverse the choice>
- **Durable record:** <`docs/decisions/...`, `docs/backend-library-selection.md`, or `docs/repo-architecture.md` update required in Implementation, or `none`>

## System flows, invariants, and failure semantics

<Omit when the System Design branch did not trigger.>

- **Flows & source of truth:** <actor/trigger to caller-visible completion or durable finality; canonical writer and representation; compact diagram only when ordering, fan-out, or recovery is clearer visually>
- **Failure, cancellation, and concurrency:** <failure points, `CommitUnknown` / ambiguous outcome handling, idempotency/replay/reconciliation, drop/cancellation safety, race serialization>
- **Enforcement & blast radius:** <mechanism and boundary enforcing each critical invariant, effectful bypass paths closed, mixed-version or consumer compatibility>
- **Operator signals & lifecycle:** <distinct log event, metric label, span outcome, or probe state per failure, degradation, or recovery path; readiness and staged shutdown participation>
- **Unit cost & bound:** <round trips, queries, held locks, and buffered bytes per request, message, job, or caller-controlled input, and what bounds them>
- **Deployment impact:** <`none`, `standard single-step deploy`, or link to `rollout.md`>

## Rust architecture, seams, and ownership

<Omit when the Rust Architecture branch did not trigger.>

- **Semantic owners:** <per changed responsibility: `<crate>::<module>` (`extend | add | replace | delete`), `quality/architecture.json` role and optional profile fit, superseded code/paths deleted>
- **Deep modules & seams:** <cross-crate or cross-boundary interfaces—types, ownership/lifetimes, error mapping, type-enforced invariants, hidden complexity, dependency category, and test surface; compact Rust type/trait/enum or SQL snippet only when more precise than prose>
- **Verified anchors:** <source-verified `path::Symbol` and `crate@version::item` depended on or reused, plus any uncached crate check carried in `Reopen if`>
- **Shared mutated surfaces:** <shared surfaces actually mutated—`Cargo.toml`/`Cargo.lock`, `migrations/`, `api/openapi/service.yaml`, `api/proto/`, `quality/architecture.json`, `crates/service` bootstrap, `crates/config`—or `none`>
- **Planning hooks:** <prerequisite shared seam, riskiest unverified integration seam, or wide blast-radius `expand -> migrate -> contract` constraint Planning must sequence; omit when none>

## Assumptions and reopen triggers

- **<Bounded assumption>:** <assumption preserving accepted intent and spec> — **Reopen if:** <objective condition or owner>
```

## Field Rules

- **Lifecycle (`status`):** start at `draft`; flip to `ready` only after
  [Technical Design Review](../phases/technical-design-review.md) returns
  `PASS` or `CONCERNS` and any carried obligations are recorded in `Carried
  concerns`. Use `blocked` when preserving partial work alongside a `blocked`
  handoff upstream.
- **Single entry point, task-adaptive sections:** keep
  `specs/<task>/design/design.md` as the default index and single-file home;
  split a large section (such as a component inventory) into a linked sibling
  file under `specs/<task>/design/` only when splitting creates a clearer
  review unit. Omit `Carried concerns`, `Key decisions` (when every path is
  forced), `System flows...`, `Rust architecture...`, `Planning hooks`, or
  `Assumptions...` when untriggered or empty instead of writing `N/A`
  boilerplate. Persist `specs/<task>/rollout.md` alongside `design.md` only
  when [Release Closure](../rubrics/release-closure.md) requires a non-trivial
  rollout sequence.
- **Addressable decisions (`D1`, `D2`, …) and full traceability:** every
  `R<n>`, `G<n>`, and `Carried concerns` item in [Specification
  V1](spec-v1.md) appears in `Obligation trace` (closing each upstream concern
  with a decision or carrying it into `Carried concerns` if it remains a
  bounded downstream risk). Planning packets cite `design/design.md#d1-...`
  directly without copying design prose.
- **Interfaces, not implementation:** inline a compact Rust type/trait/enum
  signature, SQL schema/constraint, state table, or exact wire/signing vector
  (including one deterministic non-secret vector for signature-sensitive
  shapes) when it pins a boundary invariant more precisely than prose. Leave
  function bodies, speculative internal `.rs` file lists, task slicing, and
  test cases/fixtures/commands to Planning and Implementation.
- **Proof boundary:** design ensures module interfaces are testable at their
  natural seams without mock-only traits and names the proving surface, while
  keeping a proposed check distinct from a mandatory local or live acceptance
  gate ([Evidence Contract](../shared/evidence-contract.md#design-proof)).
- **Durable record gate:** mark a durable update to `docs/decisions/`,
  [Backend Library Selection](../../backend-library-selection.md), or
  [Repository Architecture](../../repo-architecture.md) only when all three
  hold—(1) hard to reverse, (2) surprising to a future reader without context,
  and (3) a real trade-off with a rejected alternative—or whenever reopening a
  previously rejected choice in those records. Implementation lands the doc
  update in the same change.
