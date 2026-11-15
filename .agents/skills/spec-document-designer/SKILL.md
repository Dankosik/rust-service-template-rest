---
name: spec-document-designer
description: "Synthesize or repair spec.md into falsifiable behavior rules, unchanged invariants, and explicit dispositions from accepted inputs."
metadata:
  invocation: user
  kind: workflow
disable-model-invocation: true
---

# Spec Document Designer

Read and apply [Specification](../../../docs/spec-first-workflow/phases/specification.md).
Trace every accepted requester-meaning input, decision-changing evidence
implication, runtime or generated authority, unchanged invariant, and proof
expectation into a falsifiable rule or an explicit unchanged, non-goal, or
blocked disposition. Complete once Specification's ready condition holds, or
return the named user or external decision blocking it.
