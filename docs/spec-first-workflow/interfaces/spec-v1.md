# Specification V1

`specs/<task>/spec.md` is the reviewed behavioral contract for one structured
task: what external observers (users, callers, operators, and downstream
systems) can rely on.

```markdown
# Specification: <short observable outcome>

status: draft | ready | blocked
Basis: intent.md[, research/<topic>.md ...]
Carried concerns: <risk, downstream owner, closing action; omit when none>

## Actors and surfaces

- <actor, consumer, or persisted/in-flight state> — <surface and primary locator>

## Current baseline

- <verified current behavior or guarantee shaping the delta — primary locator>

## Terminology

- **<Term>:** <canonical meaning; omit section when existing vocabulary is unambiguous>

## Behavior delta

### R1 — <short rule name>

<Observable rule, cross-cutting invariant, or labeled numeric target.>

- **Falsifier:** <highest-boundary violation: trigger/state -> wrong external outcome instead of promised outcome>

## Preserved guarantees

- **G1 — <short guarantee name>:** <unchanged guarantee, or authorized breaking change with former-caller outcome and mixed-version expectation — primary locator>

## Non-goals

- <plausible adjacent work explicitly excluded from this change>

## Decisions, assumptions, and reopen triggers

- **Delegated decision:** <choice> over <strongest rejected alternative> because <constraint or evidence> — **Reopen if:** <objective condition or owner>
- **Bounded assumption:** <assumption preserving requester meaning> — **Reopen if:** <objective condition or owner>
- **Downstream handoff:** <non-behavioral `intent.md` item or technical preference> — **Owner:** `Technical Design` | `Planning`
- **Durable contract update:** <`docs/repo-architecture.md#domain-vocabulary` or `docs/production-contract.md` update required in Implementation; omit when none>

## Blocked decisions

<Include only while `status: blocked`; remove once resolved.>

- **B1 — <question in requester language>:** <primary evidence> | **Options & consequences:** <A vs B> | **Recommendation:** <choice and reason>
```

## Field Rules

- **Lifecycle (`status`) & ID stability:** start at `draft`; flip to `ready`
  only after [Specification Review](../phases/specification-review.md) permits
  handoff and any bounded `CONCERNS` appear in `Carried concerns`. Use
  `blocked` with `Blocked decisions` when preserving partial work alongside a
  blocked handoff to [Intake](../phases/intake.md) or an external policy owner.
  When reopening a `ready` spec, keep surviving `R<n>` and `G<n>` IDs stable
  (retire deleted IDs rather than renumbering downstream-cited anchors).
- **Task-adaptive sections:** omit `Carried concerns`, `Terminology`, `Current
  baseline` (pure greenfield), `Decisions, assumptions, and reopen triggers`,
  or `Blocked decisions` when empty. Always keep `Actors and surfaces`,
  `Behavior delta` (write `None — behavior-preserving change` only when zero
  functional, operational, or quantitative behavior delta exists and only
  `Preserved guarantees` and a non-behavioral `Downstream handoff` apply),
  `Preserved guarantees` (`None — verified greenfield surface` when no existing
  consumers or persisted state exist), and `Non-goals`.
- **Addressable clauses & representation:** give each obligation its own `R<n>`
  heading or `G<n>` bullet so downstream phases cite `spec.md#<rule-anchor>`
  without copying prose. Use prose by default; switch to a compact decision
  table, state-transition table, decisive boundary example, or exact wire/type
  fragment whenever rules interact. Link unchanged code, committed contracts
  (`api/`), migrations, and `research/*.md` instead of copying them.
- **Falsifier at the highest observable boundary:** name a concrete black-box
  violation at the highest external seam (HTTP/gRPC response, persisted state,
  emitted event, exit code, boot failure, or operator signal)—never a test,
  fixture, proving layer, command, or unmandated live gate ([Evidence
  Contract](../shared/evidence-contract.md#design-proof) owns mandatory proof
  expansion):
  - *Observable:* `POST /orders` with a reused `Idempotency-Key` and different
    body returns `200` or creates a second order instead of `409`
    `idempotency_conflict`.
  - *Wrong layer:* Unit test `test_idempotency_conflict` or
    `make test-integration-db` fails.
- **Triggers & quantities:** write objective `Reopen if` triggers (never
  boilerplate like "if requirements change"). Label every numeric bound using
  [Research](../phases/research.md)'s vocabulary: `measured`, `external limit`,
  `forecast`, `accepted target`, or `assumption`.
