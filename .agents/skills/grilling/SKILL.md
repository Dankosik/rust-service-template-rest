---
name: grilling
description: "Interview the requester relentlessly in frontier rounds until every branch of a plan, design, spec, or idea is resolved."
metadata:
  invocation: user
  kind: workflow
disable-model-invocation: true
---

# Grill

Apply [Grilling](../../../docs/spec-first-workflow/shared/grilling.md) standalone to stress-test the proposal, plan, specification, design, or idea in front of you. Continue independent authorized work while waiting for decisions that block the rest.

Once the **frontier** is empty, summarize the settled decisions (including rejected alternatives and why), labeled assumptions with `Reopen if` triggers, and residual risks, then wait for explicit confirmation before acting or launching any workflow phase—unless the user explicitly scoped this run as grill-only. If the user confirms and wants to enter structured planning, carry the settled branches straight into [Intake](../../../docs/spec-first-workflow/phases/intake.md) without re-asking them.
