---
name: spec-first-brainstorming
description: "Frame an accepted change as a concrete actor, observable current-to-desired behavior delta, explicit non-goals, and specification readiness."
metadata:
  invocation: user
  kind: workflow
disable-model-invocation: true
---

# Spec First Brainstorming

Consume the requester-meaning authority selected by [Phase
Selection](../../../docs/spec-first-workflow.md#phase-selection). Return to
[Intake](../../../docs/spec-first-workflow/phases/intake.md) when that input is
missing or leaves requester meaning unresolved.

Frame the observable behavior for Specification. Leave the choice of mechanism
to Technical Design and task breakdown to Planning.

- Name the specific affected actor (such as `API client`, `operator`, or
  `tenant admin`) before any mechanism, and state the observable
  current-to-desired behavior delta. Distinguish a required constraint from a
  suggested mechanism.
- Write `Non-Goals` as plausible adjacent work excluded from this pass, and bind
  vague qualifiers (`fast`, `secure`, `compliant`) to a named policy or
  threshold owner (or an explicit open question). When two interpretations solve
  different problems, recommend the symptom-supported one or fail readiness on
  the missing discriminator.
- Classify remaining uncertainty: mark uncertain facts `[assumption]` paired
  with their risk and validation path, assign technical design branches to
  downstream design phases, and reserve `[user-owned]` strictly for questions
  surviving [Decision Ownership](../../../AGENTS.md#decision-ownership).

Disposition each unit using
[Specification](../../../docs/spec-first-workflow/phases/specification.md)'s
categories (accepted behavior, deliberately unchanged, non-goal, or upstream
open decision). Return the frame, readiness, and next owner. A `pass` means
Specification can proceed from the actor, delta, scope, and constraints. Route
directly to Design only when Phase Selection identifies a ready equivalent
behavior contract. Carry open design questions with their owner and unblock
condition. A `fail` names the smallest missing input and its owner.
