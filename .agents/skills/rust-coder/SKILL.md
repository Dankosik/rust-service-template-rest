---
name: rust-coder
description: "Use when writing or changing Rust code within the task's API, ownership, and technical constraints."
---

# Rust Coder

**Execution.** Establish the observable result, its invariants, and distinguishable effect outcomes, then choose a representation and operation that make those facts visible. When requirements meet on the same input or transition, establish their precedence before implementing them.

Read the affected owner and its callers against the accepted behavior. Existing code supplies context and conventions, not proof that a defect is required. Use the [repository architecture](../../../docs/repo-architecture.md) to locate composition, configuration, readiness, provider, and feature responsibilities. Complete the requested behavior at those boundaries, including its failure path; a plausible implementation of the middle of an operation is not a complete outcome.

Use what the current representation already guarantees: ordering, uniqueness, contiguous storage, or exclusive access can remove manual coordination. Check existing code, standard-library operations, and declared dependencies for matching semantics, and follow `rust-dependencies` when adding or choosing between a library and custom code. Keep application-specific policy with its owner and preserve explicit constraints such as a fixed public API or std-only implementation.

Make the main transformation, mutation boundary, and failure paths easy to follow. Complexity and nesting matter more than length: a long flat match can remain cohesive, and early returns can expose preconditions when the callee's contract remains explicit. Group parameters when a named value clarifies their relationship, and name or annotate a complex type when inference obscures useful meaning. Use `rust-structural-quality` when judging helper splits, free functions versus methods, or deleting staging and rollback containers.

Return newly produced results, while accepting mutable caller-owned storage when updating or reusing it is the operation. Choose the unit of iteration that matches the decision: an item, an adjacent pair, a run, or a batch. Prefer an existing operation over repeating the same transition when its ordering, failure timing, and effects match. Do not materialize a stream merely to obtain a convenient slice operation. Keep pure decisions separate from I/O when that gives them a complete, useful contract, without inventing an extra layer.

For the central unsettled design choice, compare the current representation with the strongest simpler alternative under the same contract, preferring fewer independently maintained relationships while keeping structure that establishes a required invariant or justified cost bound. Before an external or destructive effect relies on an earlier observation, identify what keeps that observation valid until the effect; rechecking may narrow a race without making the operation atomic. Preserve failures, order, ownership transfer, and completion obligations through simplification.

Follow the project's naming, documentation, and lint policies. Where documentation is warranted, describe what the signature cannot establish: preconditions, effects, failure, or lifetime obligations. Keep examples and comments true when their owner changes. Reuse adequate tests and select missing cases from the accepted contract, with an expectation independent of the implementation. A bug reproducer must fail for the reported reason; a compile error does not establish that behavior, and an empty test run or a pass before the last relevant edit is not evidence for the final behavior.
