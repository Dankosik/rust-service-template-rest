---
name: rust-idiomatic
description: "Contracts. Use for Rust ownership, borrowing, trait, error-identity, or async-boundary decisions where caller-visible semantics or readability need attention."
---

# Rust Idiomatic

**Contracts.** Make ownership and caller expectations visible in ordinary Rust: who owns a value, who may mutate it, what absence and failure mean, and what crosses a task boundary. Identify those before changing representation. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Borrow through slices, string slices, and paths when ownership is unnecessary; move owned values when responsibility transfers; let lifetimes describe actual relationships. Before a clone, an Arc, or a Mutex introduced to satisfy the borrow checker, inspect the data flow and the scope of the borrow: a shared owner that exists only to appease the compiler hides the real owner. Cloning an Arc shares an allocation, not a synchronized mutation, and a guard held across an await is a contention and deadlock surface.

Choose enums for meaningful alternatives and newtypes for distinctions that prevent real misuse, as the readiness handle, the problem code, and the secret string do here. Use Option and Result to preserve absence and failure; never encode either in a sentinel value. Add standard conversion and comparison traits when their semantics fit; keep equality and hashing consistent.

Library crates expose typed errors and preserve the cause through a source; only the composition root turns an error into an exit code. Keep Display operator-readable and free of secrets. Values that cross a spawn need to be Send and owned for the task's lifetime; prefer giving a task the data it needs over widening lifetimes. A trait stored as a trait object returns a boxed future, as the probe trait does; a trait used only generically may use an async method.

Use iterators for readable transformations and loops for clearer stateful control flow; collecting changes memory use. Preserve ordering, numeric overflow behavior, and byte versus text distinctions. Unsafe code is forbidden in this workspace and compilation is not a soundness proof; a need for unsafe reopens the design rather than the lint.

For review, explain the contract issue and the smallest justified change without editing files. For implementation, finish with formatted code and focused checks for the behavior or type contract affected. Avoid unrelated modernization or abstractions that obscure the operation; neither a clone nor a shared owner is wrong merely by its spelling.
