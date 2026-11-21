---
name: grilling
description: "Stress-test a plan, spec, design, or idea through structured interview rounds until every open decision is resolved. Stops at a confirmed decision summary unless you choose to proceed."
metadata:
  invocation: user
  kind: workflow
disable-model-invocation: true
---

# Grill

Apply [Grilling](../../../docs/spec-first-workflow/shared/grilling.md) standalone
to stress-test the proposal, plan, specification, technical design, or idea in
front of you until every open branch is resolved.

- **Scope to the subject:** when the user invokes `/grilling` on a technical
  design, ADR, or refactor plan, those technical choices are in scope; on a
  product idea or feature request, keep out-of-scope technical internals off
  the frontier under [Decision Ownership](../../../AGENTS.md#decision-ownership).
- **Do not act mid-interview:** run read-only fact-finding to verify claims
  against primary sources, but make no file edits and start no downstream
  workflow phase until the frontier is empty and the user confirms shared
  understanding.
- **Stop or transition cleanly:** once the user confirms the final summary from
  [Grilling](../../../docs/spec-first-workflow/shared/grilling.md#closing-and-escape-hatch),
  stop if scoped as grill-only (or if the user just wanted a sharpened decision
  record). If the user confirms and wants structured preparation, carry every
  settled choice, rejected alternative, constraint, labeled `A<n>` assumption,
  and delegated boundary straight into
  [Intake](../../../docs/spec-first-workflow/phases/intake.md) without
  re-asking them.
