---
name: rust-idiomatic
description: "Use when a Rust change affects ownership, borrowing, trait bounds, caller-visible error identity, Send/Sync at an async boundary, or a public type's contract."
metadata:
  invocation: model
  kind: method
---

# Rust Idiomatic

Correctness follows **contracts the type system can see**: who owns a value,
who may mutate it, what absence and failure mean, and what crosses a task
boundary.

Borrow through `&str`, slices, and `&Path` when ownership is unnecessary;
move owned values when responsibility transfers; let lifetimes describe real
relationships. Before a `clone`, `Arc`, or `Mutex` to satisfy the borrow
checker, inspect the data flow: a shared owner that exists only to appease
the compiler hides the real owner. `Arc` shares an allocation, not a
synchronized mutation; a guard held across an `.await` is a contention and
deadlock surface.

Choose enums for meaningful alternatives, newtypes for distinctions that
prevent misuse (`Readiness`, `Code`, `SecretString`), and `Option`/`Result`
for absence and failure; never encode either in a sentinel value. Library
crates expose typed errors with `thiserror` and preserve the cause through
`#[source]`; the composition root is the only place an error becomes an exit
code. Keep `Display` operator-readable and free of secrets.

At async boundaries, values that cross `tokio::spawn` need `Send + 'static`;
prefer owning the data the task needs over widening lifetimes. Return
`impl Future`/`async fn` in traits only when object safety is not required;
this repository's `health::Probe` returns a boxed future because probes are
stored as `Box<dyn Probe>`.

`unsafe_code` is forbidden workspace-wide; compilation is not a soundness
proof, and a need for `unsafe` reopens the design. Match the surrounding
code's naming and comment density. Prefer iterators for transformations and
loops for stateful control flow; collecting changes memory use.

For review, explain the contract issue and the smallest justified change
without editing. For implementation, finish with `cargo fmt` and the focused
tests for the changed contract. A `clone` or a shared owner is not wrong by
spelling; it is wrong when it hides the actual owner or copies what could be
borrowed on a hot path.
