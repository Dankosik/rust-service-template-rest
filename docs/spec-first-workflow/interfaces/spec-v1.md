# Specification V1

`specs/<task>/spec.md` is the reviewed behavioral contract for one structured
task: what external observers (users, callers, operators, downstream systems)
can rely on.

```markdown
# Specification: <short observable outcome>

status: draft | ready | blocked
Basis: intent.md[, research/<topic>.md ...]
Carried concerns: <risk, downstream owner, closing action; omit when none>

## Actors and surfaces

- **Actors & entry seams:** <actor + trust/auth boundary -> API route, gRPC RPC, CLI/config, or worker trigger — primary locator>
- **Affected consumers & state:** <callers, downstream systems, persisted/in-flight state, optional profiles — primary locator>
- **Observable seams:** <highest external boundaries where outcomes are observed>

## Current baseline

- <verified current behavior or guarantee shaping the delta — primary locator>

## Terminology

- **<Term>:** <canonical meaning> (not <confused/overloaded sense>)

## Behavior delta

### R1 — <short rule name>

<Observable rule, invariant, labeled numeric target, or state-transition / precedence table. Breaking changes include `Authorization:`, `Former-caller outcome:`, and `Mixed-version overlap:`.>
<Optional: **Open to Technical Design:** permitted variation or unfixed new identifier spelling preserving the invariant.>

- **Falsifier:** <plausible broken behavior at the highest external seam: concrete state + trigger -> wrong external outcome instead of promised outcome>

### Boundary scenarios

<Optional: 1–3 `Given -> When -> Then` scenarios when rules interact at seams.>

## Preserved guarantees

- **G1 — <short guarantee name>:** <concrete unchanged external guarantee for a named consumer/surface — primary locator>

## Non-goals

- <plausible adjacent work explicitly excluded from this change>

## Decisions, assumptions, and reopen triggers

- **Delegated decision:** <choice> over <steelman rejected alternative> because <constraint or evidence> — **Reopen if:** <objective condition or owner>
- **Bounded assumption:** <two-way assumption preserving requester meaning> — **Reopen if:** <objective condition or owner>
- **Downstream handoff:** <non-behavioral `intent.md` item or technical preference> — **Owner:** `Technical Design` | `Planning`
- **Durable contract update:** <`docs/repo-architecture.md#domain-vocabulary`, `docs/production-contract.md`, or `docs/decisions/...` update required in Implementation; omit when none>

## Blocked decisions

<Include only while `status: blocked`; remove once resolved.>

- **B1 — <question in requester language, worded so "yes" accepts recommendation when supported>:** <primary evidence> | **Options & consequences:** <A vs B> | **Recommendation:** <choice and reason, or none when balanced>
  - *Follow-up if `<option>` (Intake-only, hold until answered):* <dependent child fork; omit when none>
```

## Field Rules

- **Lifecycle & ID stability:** start at `draft`; flip to `ready` only after
  [Specification Review](../phases/specification-review.md) permits handoff and
  bounded `CONCERNS` appear in `Carried concerns`. Use `blocked` with `Blocked
  decisions` on a blocked handoff. On reopen, keep surviving `R<n>`/`G<n>` IDs
  stable (retire deleted IDs rather than renumbering) and re-verify prior
  `Carried concerns`.
- **Proportionality, highest seams & anchored tables:** size `spec.md` to real
  risk (a focused fix may need `R1`–`R2`) and prefer existing highest external
  seams. Give each coherent behavioral unit one `R<n>` or `G<n>` anchor. When
  states or ≥2 conditions interact, make a compact state-transition or
  precedence table the body of its owning `R<n>` (never float unanchored rules
  outside `R<n>` or duplicate tables in prose), and add 1–3 `Boundary scenarios`
  when rules interact at seams. Inline a small external wire/payload shape only
  when external callers pin it; link unchanged code, `api/`, migrations, and
  `research/*.md` instead of copying them.
- **Task-adaptive sections & guarantee discipline:** omit empty optional
  sections (`Carried concerns`, `Terminology`, `Current baseline` on greenfield,
  `Boundary scenarios`, `Decisions, assumptions, and reopen triggers`, `Blocked
  decisions`). Always keep `Actors and surfaces`, `Behavior delta` (`None —
  behavior-preserving change` only when zero functional, operational, or
  quantitative delta exists), `Preserved guarantees` (`None — verified
  greenfield surface` when greenfield), and `Non-goals`. Place authorized
  breaking changes as `R<n>` in `Behavior delta` (never in `Preserved
  guarantees`); keep every `G<n>` a concrete, falsifiable unchanged invariant
  for a named consumer/surface, never generic filler.
- **Non-tautological Falsifier at the highest external seam:** name a plausible
  broken implementation at the highest observable boundary (response, persisted
  state, emitted event, exit code, boot failure, operator signal), leaving
  tests, fixtures, proving layers, commands, and unmandated live gates to
  downstream phases ([Evidence
  Contract](../shared/evidence-contract.md#design-proof)):
  - *Strong:* `POST /orders` replays an `Idempotency-Key` with a modified body
    and returns the original `200` (or creates a second order) instead of `409`
    `idempotency_conflict`.
  - *Tautological:* Reusing an `Idempotency-Key` with a different body fails to
    return `409`.
  - *Wrong layer:* Unit test `test_idempotency_conflict` or
    `make test-integration-db` fails.
- **Steelman, triggers & quantities:** compare every `Delegated decision`
  against the strongest serious alternative (never a strawman), write objective
  `Reopen if` conditions, and label numeric bounds per
  [Research](../phases/research.md) (`measured`, `external limit`, `forecast`,
  `accepted target`, `assumption`).
