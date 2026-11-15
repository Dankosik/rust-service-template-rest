---
name: grilling
description: "Stress-test a plan or design one high-impact unresolved decision branch at a time, pairing each user-owned question with a clear recommendation."
metadata:
  invocation: user
  kind: workflow
disable-model-invocation: true
---

# Grill

Reconstruct material decision branches from the proposal, repository evidence, and user-owned choices, then select one highest-impact unresolved branch. Resolve technical branches yourself from repository evidence and the responsible owner. When a surviving branch requires a user-owned decision, state its consequence, ask one question paired with a recommendation and tradeoff, and wait before advancing dependent work while continuing independent authorized work.

Grilling completes when every material branch (any branch able to change the plan, implementation, authority, failure behavior, or proof) is answered, repository-resolved, assigned to an owner, or explicitly deferred with its consequence. Summarize decisions, assumptions, and residual risks once all branches are terminal, and wait for explicit confirmation before acting unless an explicit grill-only or phase-only request already sets the boundary.
