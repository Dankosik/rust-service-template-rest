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

- **Entry seams:** <API route, gRPC RPC, CLI/config, worker trigger — primary locator>
- **Affected consumers & state:** <callers, downstream systems, persisted/in-flight state, optional profiles — primary locator>
- **Observable seams:** <highest external boundaries where outcomes are observed>

## Current baseline

- <verified current behavior or guarantee shaping the delta — primary locator>

## Terminology

- **<Term>:** <canonical meaning; omit when existing vocabulary is unambiguous>

## Behavior delta

<Start with a compact state-transition or precedence/decision table when states or >=2 conditions interact.>

### R1 — <short rule name>

<Observable rule, invariant, or labeled numeric target; state non-obvious observer consequences inline.>
<Optional: **Open to Technical Design:** permitted variation (or unfixed new identifier spelling) preserving the observable invariant.>

- **Falsifier:** <plausible broken behavior at the highest external seam: concrete state + trigger -> wrong external outcome instead of promised outcome>

## Preserved guarantees

- **G1 — <short guarantee name>:** <unchanged guarantee, or authorized breaking change with former-caller outcome and mixed-version expectation — primary locator>

## Non-goals

- <plausible adjacent work explicitly excluded from this change>

## Decisions, assumptions, and reopen triggers

- **Delegated decision:** <choice> over <steelman rejected alternative> because <constraint or evidence> — **Reopen if:** <objective condition or owner>
- **Bounded assumption:** <assumption preserving requester meaning> — **Reopen if:** <objective condition or owner>
- **Downstream handoff:** <non-behavioral `intent.md` item or technical preference> — **Owner:** `Technical Design` | `Planning`
- **Durable contract update:** <`docs/repo-architecture.md#domain-vocabulary` or `docs/production-contract.md` update required in Implementation; omit when none>

## Blocked decisions

<Include only while `status: blocked`; remove once resolved.>

- **B1 — <question in requester language, worded so "yes" accepts recommendation when supported>:** <primary evidence> | **Options & consequences:** <A vs B> | **Recommendation:** <choice and reason, or none when balanced>
```

## Field Rules

- **Lifecycle & ID stability:** start at `draft`; flip to `ready` only after
  [Specification Review](../phases/specification-review.md) permits handoff and
  bounded `CONCERNS` appear in `Carried concerns`. Use `blocked` with `Blocked
  decisions` on a blocked handoff. On reopen, keep surviving `R<n>`/`G<n>` IDs
  stable (retire deleted IDs rather than renumbering).
- **Proportionality & tables first:** size `spec.md` to real risk (a focused
  fix may need `R1`–`R2`). Give each coherent behavioral contract unit one
  `R<n>` or `G<n>` anchor—avoiding micro-fragmented single-sentence rules and
  monolithic dumps. Start `Behavior delta` with a state-transition or
  precedence/decision table whenever states or ≥2 conditions interact, adding
  1–3 `Given -> When -> Then` boundary scenarios when rules interact at seams.
  Link unchanged code, `api/`, migrations, and `research/*.md` instead of
  copying them.
- **Task-adaptive sections:** omit empty optional sections (`Carried concerns`,
  `Terminology`, `Current baseline` on greenfield, `Decisions, assumptions, and
  reopen triggers`, `Blocked decisions`). Always keep `Actors and surfaces`,
  `Behavior delta` (`None — behavior-preserving change` only when zero
  functional, operational, or quantitative delta exists and only `Preserved
  guarantees` plus a non-behavioral `Downstream handoff` apply), `Preserved
  guarantees` (`None — verified greenfield surface` when greenfield), and
  `Non-goals`.
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
