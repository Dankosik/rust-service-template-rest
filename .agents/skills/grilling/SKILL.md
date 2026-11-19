---
name: grilling
description: "Stress-test a plan or design one decision at a time and summarize the decisions, assumptions, and remaining risks."
metadata:
  invocation: user
  kind: workflow
disable-model-invocation: true
---

# Grill

Identify the decisions that could change the proposal using repository evidence
and the requester's prior choices. Use the [grilling interview
method](../../../docs/spec-first-workflow/shared/grilling.md) for the most
consequential unresolved choice. Continue independent authorized work while
waiting for a decision that blocks the rest.

Grilling completes when every material branch (any branch able to change the plan, implementation, authority, failure behavior, or proof) is answered, repository-resolved, assigned to an owner, or explicitly deferred with its consequence. Summarize decisions, assumptions, and residual risks once all branches are terminal, and wait for explicit confirmation before acting unless an explicit grill-only or phase-only request already sets the boundary.
