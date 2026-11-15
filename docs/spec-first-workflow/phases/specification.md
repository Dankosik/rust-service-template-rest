# Specification

Use when structured work lacks a ready behavior delta. Own what must be true
before selecting a mechanism.

Consume the requester-meaning authority selected by [Phase
Selection](../../spec-first-workflow.md#phase-selection), decision-changing
Research, current runtime, generated and consumer authority, stable decisions,
and current findings. Disposition every material requester-meaning item as accepted
behavior, deliberately unchanged behavior, a non-goal, or an upstream open
decision; never silently replace requester meaning. Reconstruct affected actors
and surfaces; record changed, removed, deliberately unchanged, and non-goal
behavior. Start with a coherent behavioral outline and apply Outcome,
Necessity, and Composition below before expanding per-rule detail. Develop
the same specification throughout; the outline adds no artifact or review gate.

Load only a method whose pressure changes observable meaning:

- client-visible REST semantics -> `rust-api-contract`;
- persistence truth, transactions, or migrations -> `rust-sqlx`;
- failure semantics, problem codes, or degradation -> `rust-errors`;
- trust, authorization, isolation, or sensitive data -> `rust-security`;
- deadlines, retries, readiness, or drain behavior -> `rust-reliability`.

Business transitions have no dedicated skill; decide them from accepted intent
and [Repository Architecture](../../repo-architecture.md) and record the
decision.

Outcome: Check whether all requirements could pass while the accepted
user outcome still fails; resolve any such gap.

Necessity: Trace each added requirement to accepted intent or a mandatory
constraint. Treat research recommendations as evidence, not accepted
requirements.

Composition: Check interacting rules together through a representative
user-visible scenario; resolve conflicts in terminology, precedence,
and outcomes.

Then apply [Material Rule](#material-rule) to each materially affected rule so
scope, policy, invariants, compatibility, truth/finality, failure,
replay/recovery, and success meaning cannot diverge across two reasonable
implementations. Recheck any outline conclusions invalidated by that detail;
the completed contract remains the independent review boundary.

## Material Rule

Close only the parts of a materially affected rule that can change meaning:
actor/context, trigger/input/preconditions, normative rule and precedence,
states and invalid or repeated transitions, observable
normal/boundary/rejection/failure/recovery/compatibility outcomes, required or
forbidden effects, identifiers/units/bounds, absence/default behavior, and the
nearest feasible falsifier.

If two reasonable implementations could satisfy the wording yet produce different
user- or operator-visible results, resolve the fork from current authority, a
Specification decision, or a bounded assumption with an objective reopen
condition. Use plain prose unless a compact decision table, state model,
quality scenario, decisive example, or literal schema/type fragment closes the
ambiguity more precisely; keep only the decision-carrying part (mechanism and
placement belong to Technical Design).

Return a compact behavioral contract and reference unchanged code, contracts,
tests, mockups, or evidence. Persist `spec.md` only through
[Artifacts](../shared/artifacts.md). Apply [Specification
Review](specification-review.md) through shared [Review](../shared/review.md).

Ready when every material requester-meaning item and divergence has one grounded
disposition and the next owner need not invent product meaning. Reopen Intake
when intent changes or is incomplete, Research for evidence, or the named policy
owner for its missing decision.
