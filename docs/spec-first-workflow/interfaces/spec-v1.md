# Specification V1

`specs/<task>/spec.md` is the reviewed behavioral contract for one structured
task. It records what affected users, operators, and downstream systems can
rely on—leaving mechanism and placement to Technical Design, slicing to
Planning, and test implementation to Implementation.

```markdown
# Specification: <short observable outcome>

status: draft | ready | blocked
Basis: intent.md[, research/<topic>.md ...]
Carried concerns: <bounded review CONCERNS carried downstream; omit when none>

## Actors and surfaces

- <actor or consumer> — <affected surface and primary repo locator>

## Current baseline

- <verified current behavior or guarantee shaping the delta — primary locator>

## Terminology

- **<Term>:** <canonical meaning; omit section when code/contract nouns have no ambiguity>

## Behavior delta

### R1 — <short rule name>

<Observable rule: trigger, target behavior, and boundary/failure/precedence/promise semantics where implementations could diverge.>

- **Falsifier:** <nearest feasible observable condition that disproves this rule>

## Preserved guarantees

- **G1 — <short guarantee name>:** <unchanged guarantee, or authorized breaking change with what former callers observe — primary locator>

## Non-goals

- <plausible adjacent work explicitly excluded from this change>

## Decisions, assumptions, and reopen triggers

- **<Delegated decision | Bounded assumption>:** <choice within limits> — **Reopen if:** <objective condition or owner>
```

## Field Rules

- **Lifecycle (`status`):** start at `draft`; flip to `ready` only after
  [Specification Review](../phases/specification-review.md) permits handoff and
  any bounded `CONCERNS` appear in `Carried concerns`. Use `blocked` when
  preserving partial work alongside a `blocked` handoff to
  [Intake](../phases/intake.md) or another upstream owner.
- **Task-adaptive sections:** omit `Carried concerns`, `Terminology`,
  `Current baseline` (pure greenfield), or `Decisions, assumptions, and reopen
  triggers` when empty. Always keep `Actors and surfaces`, `Behavior delta`,
  `Preserved guarantees` (write `None — verified greenfield surface` when no
  existing consumers exist), and `Non-goals`.
- **Addressable atomic rules (`R1`, `R2`, … and `G1`, `G2`, …):** give each
  distinct obligation its own heading or bullet so Technical Design, Planning
  (`Sources: spec.md#<rule-anchor>`), and Review can cite clauses without
  splitting dense paragraphs.
- **Format & links:** use prose by default; switch to a compact decision table,
  state-transition table, decisive example, or exact wire/type fragment when
  prose leaves ambiguity. Link unchanged code, `api/openapi.yaml`, migrations,
  and `research/*.md` rather than copying them.
- **Falsifier vs proof boundary:** name the highest observable boundary
  (response, persisted outcome, exit code, log/metric signal) that disproves
  the rule. Never prescribe unit tests, fixtures, proving layers, or commands,
  and never promote a falsifier into a mandatory live gate
  ([Evidence Contract](../shared/evidence-contract.md#design-proof)).
- **Decisions & assumptions:** record every choice made within
  [Intent](intent.md)'s delegated limits and every bounded assumption with a
  concrete `Reopen if` trigger—never boilerplate ("if requirements change").
