# Specification

Continue autonomously in the same session after [Intake](intake.md) confirms
shared understanding. Own the externally observable behavioral contract
(**WHAT**) without prescribing technical mechanisms, crates, or code structure
(those belong to [Technical Design](technical-design.md)).

## Steps

1. **Ground observable baseline (read-only).**
   - Build on Intake's verified facts (and `specs/<task>/intent.md` when
     present, verifying its baseline claims against live code rather than
     trusting prose blindly); inspect any remaining affected external contract
     (`api/`, config, CLI) or caller surface needed to pin exact rules,
     offloading wide scans to a fresh `evidence-agent` via [Read-Only
     Delegation](../shared/read-only-delegation.md) so this session stays in
     the **smart zone**.
   - Carry forward Intake's confirmed decisions, rejected product alternatives
     (*why*), `Must not` rules, delegated boundaries, settled domain terms, and
     surviving `A<n>` IDs without re-asking settled or delegated choices.
   - Surface any verified premise contradiction directly to the user instead of
     authoring around it. If every requested outcome already holds with zero
     delta, verify via [Plan Review](plan-review.md#zero-delta-check) before
     reporting completion.
   - *Done when:* affected external surfaces, callers, and baseline deltas are
     verified in primary sources.

2. **Close observable behavior (WHAT).**
   - Walk [Behavioral Probes](specification/behavioral-probes.md), loading
     `rust-api-contract`, `rust-security`, `rust-config`, `rust-reliability`,
     `rust-observability`, or `rust-performance` when an external-observer
     decision remains open.
   - Pin every externally relied-on rule (`R<n>`, using state or precedence
     tables when states interact), an observable **falsifier** at the public
     seam, client wire identifiers, and unchanged guarantees (`G<n>`). For
     behavior-preserving work, record `Behavior delta: None —
     behavior-preserving change` with concrete `G<n>` and measurable targets.
   - Dry-run the hardest 1–2 unhappy or concurrent paths: tighten any clause
     where malicious compliance defeats the success signal or two independent
     implementations could diverge externally. Escalate to the user strictly
     for an unresolved product or risk fork.
   - *Done when:* every confirmed requirement, guarantee, and applicable
     behavioral probe closes on a falsifiable external rule.

3. **Write `specs/<task>/spec.md` (`status: draft`) and enter [Technical Design](technical-design.md).**
   - Persist `spec.md` using [Artifact Format](#artifact-format), omitting
     empty optional sections and linking unchanged code, `api/`, migrations,
     and `research/*.md` rather than copying them. Because downstream fresh
     contexts (`Design Review`, `Plan Review`, `Implementation`) do not see
     Intake chat history, preserve Intake's non-obvious *why* rather than
     flattening it into bare rules. Continue directly into
     [Technical Design](technical-design.md).
   - *Done when:* `specs/<task>/spec.md` is on disk at `status: draft` with
     zero unresolved product forks, preserved Intake *why*, and
     implementation-free behavioral rules.

## Artifact Format

Keep durable IDs (`R<n>`, `G<n>`, `A<n>`) stable on reopen (preserving surviving
`A<n>` IDs from Intake / `intent.md`, and allocating any newly added spec
assumption past the highest existing `A<n>` across `spec.md` and `design.md`).

- **Header:** `# Spec: <short observable outcome>` and `status: draft | ready`.
- **`## Context & success signal`:** problem, primary actor/operator, concrete
  scenarios, observable success signal, priority tie-breakers, and settled
  product choices (including **rejected alternatives and *why***).
- **`## Actors, baseline & terminology`:** actors/trust boundaries → seams with
  locators; verified baseline delta; canonical definitions for overloaded terms
  (tagging any cross-task term to add to [Domain
  Vocabulary](../../repo-architecture.md#domain-vocabulary)).
- **`## Behavior delta`:** `### R<n> — <name>` (observable rule, labeled numeric
  target, or state/precedence table + public-seam **Falsifier**).
- **`## Preserved guarantees`:** `G<n>` unchanged invariants with locators.
- **`## Must not & Non-goals`:** forbidden outcomes (`Must not`) separated
  clearly from plausible adjacent work explicitly excluded (`Non-goals`).
- **`## Assumptions & reopen triggers`:** `A<n>` bounded assumptions + objective
  `Reopen if`, plus explicitly **delegated boundaries** (what the user
  authorized the agent to choose within stated limits).

### Calibration Bar

- **Strong vs tautological Falsifier:**
  - *Strong:* `POST /orders` replays an `Idempotency-Key` with a modified body
    and returns `200` (or creates a second order) instead of `409
    idempotency_conflict`.
  - *Weak:* repeating the rule verbatim, or naming a unit test / `just` command
    (tests belong to Implementation per [Evidence
    Contract](../shared/evidence-contract.md#design-proof)).
