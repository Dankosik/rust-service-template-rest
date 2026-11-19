---
name: rust-idiomatic
description: "Contracts. Use when designing a Rust type, signature, trait impl, conversion, or resolving clone, Arc, Mutex, or borrow ownership."
---

# Rust Idiomatic

**Contracts.** Make ownership and caller expectations visible in ordinary Rust: admissible states, permitted transitions, mutation authority, and the meaning of absence or failure. Remember that a type enforces only its construction and transition invariants; freshness, durable completion, and external resource lifetimes require explicit runtime coordination.

**Sharing and parameters.** Inspect scope and data flow before choosing a clone or shared owner. Use a deliberate `.clone()` or `Arc` when required sharing or simpler ownership fits within cost bounds; prefer owned struct fields unless a borrowed view earns its container lifetimes, and keep concrete parameter types in application code unless actual callers benefit from generic conversions.

**Mutation and completion.** Prefer `&mut` exclusive mutation; pair interior mutability or `Arc` sharing with an explicit invariant and synchronization protocol. Require `Send`, `Sync`, and `'static` bounds only at real task or thread crossings. Borrow retained state in place for inspection or update, reserving `mem::take` or `mem::replace` for true ownership transfer whose replacement state remains valid on every exit; see the [ownership example](references/ownership.md). Prefer a scoped guard (`std::mem::DropGuard` or an existing handle wrapper) for one-off infallible cleanup, use RAII for unwinding release, and expose an explicit observed completion step whenever flushing or closing can fail or suspend.

**Representation.** Keep independently varying facts independent and bind any justified derived projection or cache to its source owner in the type, as shown in the [representation example](references/representation.md), decoding external sentinel conventions at their boundary. Separate absent from empty only when the contract distinguishes them, prefer named fields outside narrow local scope, wrap distinct units or identifiers in newtypes so the compiler prevents interchange, and replace ambiguous boolean parameters while keeping independent boolean flags simple. Keep public workspace enums and structs exhaustive rather than marking them `#[non_exhaustive]`, reserving `#[non_exhaustive]` for crates with external semver consumers.

**Boundary invariants.** Parse and validate at the owning boundary, then carry checked values inward. Audit every construction and mutation path—`Deserialize`, `From`/`TryFrom`, `Default`, and field visibility—so invariant-bearing fields stay private while plain data stays direct. Reserve builders for multi-option or staged construction and typestate for APIs whose compile-time protocol outweighs its generic cost over an enum.

**Traits and derivations.** Implement `From` for lossless conversions, `TryFrom` for fallible ones, and explicit numeric cast methods where available; keep primary behavior in inherent methods and check whether consumers truly need `dyn` compatibility before constraining a trait. Derive standard traits where semantics match, implement `Display` for user-facing text and redacted `Debug` for secrets, keep `Eq`/`Ord`/`Hash` consistent, reserve `Deref` for smart pointers, and mark `#[must_use]` where ignoring a return value loses a required check or effect.

**Collections and iterators.** Select collections by access pattern and ordering (`BTreeMap` orders by key; `IndexMap` preserves insertion order), checking [workspace utility crates](../rust-dependencies/references/workspace-crates.md) before hand-rolling compact strings, inline vectors, enum-indexed tables, fast non-crypto maps, or boilerplate derives; keep overflow and byte-versus-UTF-8 boundaries explicit, propagate fallible iterator results with `collect::<Result<_, _>>()`, and verify `filter`/`take` order, scoping changes strictly to touched code.
