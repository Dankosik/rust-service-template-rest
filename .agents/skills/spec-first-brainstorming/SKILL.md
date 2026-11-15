---
name: spec-first-brainstorming
description: "Problem frame: Use when a chosen change needs behavior delta, scope, constraints, or readiness. Own frame/next owner; Skip ideation, design, and tasks."
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

Build the behavior-delta frame without inventing architecture, API, data,
security, reliability, rollout, or task decisions:

- Name the specific affected actor (such as `API client`, `operator`, or
  `tenant admin`, not `"users"`) before any mechanism, and state the observable
  current-to-desired behavior delta while preserving any user-proposed mechanism
  only as context rather than an approved design.
- Write `Non-Goals` as plausible adjacent work excluded from this pass, and turn
  vague qualifiers (`fast`, `secure`, `compliant`) into a named policy or
  threshold owner (or an explicit open question). If two interpretations solve
  different problems, recommend the symptom-supported one rather than `"support
  both"`, or fail readiness on the missing discriminator.
- Classify remaining uncertainty cleanly: mark uncertain facts `[assumption]`
  only with their risk and validation path, defer agent-owned technical design
  branches to downstream design phases (never hand a technical menu to the user),
  and keep `[user-owned]` strictly for questions surviving
  [Decision Ownership](../../../AGENTS.md#decision-ownership).

Disposition each unit using
[Specification](../../../docs/spec-first-workflow/phases/specification.md)'s
categories (accepted behavior, deliberately unchanged, non-goal, or upstream
open decision). Readiness is `pass` only when downstream Specification or
Design can proceed without inventing actor, delta, scope, or constraints
(carrying open design questions only with owner and unblock condition); `fail`
names the minimum missing discriminator and its owner. Complete with the frame,
readiness decision, and named next owner.
