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

Build the behavior-delta frame while keeping architecture, API, data, security,
reliability, rollout, and task breakdown in downstream phases:

- Name the specific affected actor (such as `API client`, `operator`, or
  `tenant admin`) before any mechanism, and state the observable
  current-to-desired behavior delta while recording any user-proposed mechanism
  as context.
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
open decision). Readiness is `pass` when downstream Specification or Design can
proceed from the stated actor, delta, scope, and constraints (carrying open
design questions with their owner and unblock condition); `fail` names the
minimum missing discriminator and its owner. Complete with the frame, readiness
decision, and named next owner.
