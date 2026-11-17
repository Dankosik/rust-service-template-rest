---
name: rust-coder
description: "Use when writing or changing Rust code within the task's API, ownership, and technical constraints."
---

# Rust Coder

**Execution.** Establish the observable result, its invariants, and distinguishable effect outcomes first, then choose representations and operations that make those facts visible. Resolve precedence up front whenever multiple rules apply to the same input or transition.

**Boundary completion.** Read the affected owner and its callers against accepted behavior, treating existing code as convention rather than proof that a current defect must stay. Locate composition, configuration, readiness, provider, and feature owners in the [repository architecture](../../../docs/repo-architecture.md), and complete the full operation through its boundary and failure path.

**Existing guarantees and delegation.** Lean on invariants the representation already enforces—ordering, uniqueness, contiguous storage, or `&mut` exclusivity—before adding manual coordination. Before writing a non-domain helper, guard, wrapper, formatter, encoder, or trait impl around `std` or imported types, inspect the pinned `std` surface and resolved crate modules and features in `Cargo.lock` source. Delegate generic framing, encoding, cleanup, or synchronization to an owner with matching inputs, failure precedence, cancellation/drop timing, time precision, and bytes while keeping domain policy local; consult `rust-dependencies` when enabling features, adding crates, or pinning a boundary divergence with a test, and preserve fixed public APIs or `std`-only constraints.

**Control flow.** Keep the main transformation, mutation boundary, and error paths flat and direct: favor flat `match` arms and early precondition returns, group related parameters into named values when that clarifies meaning, and add type annotations where inference hides an important boundary. Consult `rust-structural-quality` when weighing helper extraction, method receivers, or staging/rollback containers.

**Data flow.** Return newly computed values by default, taking `&mut` caller storage when in-place update or buffer reuse is the contract. Iterate at the decision's natural unit—item, window, run, or batch—streaming directly where slice materialization adds only allocation, and separate pure decisions from I/O when that yields a cleaner contract within the existing layer.

**Simpler alternatives and races.** Compare the central design choice against the strongest simpler representation satisfying the same contract, keeping only structure that enforces an invariant or cost bound. When an external or destructive effect depends on an earlier check, identify the lock, transaction, or conditional write that keeps the observation valid through execution, preserving failure order, ownership transfer, and completion obligations.

**Verification.** Match repository naming, lint, and comment policies: carry preconditions, effects, failures, and lifetimes in names, types, and tests rather than comments, and delete explanatory comments on touched items. Consult `rust-testing` when choosing proving layers, fixtures, or suite consolidation: derive expectations from observable contracts rather than internal steps, consolidate overlapping or obsolete cases around the strongest check while preserving independent safety guards, watch bug reproducers—and new guards when practical—fail for the targeted break, and verify edited code executes and passes targeted tests.
