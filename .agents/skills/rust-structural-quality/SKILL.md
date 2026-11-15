---
name: rust-structural-quality
description: "Deletion test. Use when adding a crate, module, trait, layer, helper, or parallel path, deciding placement, or consolidating scattered rules."
---

# Rust Structural Quality

**Deletion test.** Judge every structure by the responsibility it owns and the facts a reader or caller must coordinate. Keep it when its interface makes that responsibility easier to understand, preserve, or change; collapse it when it merely relocates the same reasoning.

**Crate and module boundaries.** Follow the [repository architecture](../../../docs/repo-architecture.md) for composition, leaf, provider, and feature ownership. Add a crate only for a present dependency-direction, compile-time, or lifecycle boundary; split modules by coherent responsibility or authority rather than line count, and create directories alongside their first real artifact.

**Polymorphism.** Choose a concrete type for fixed behavior, an enum for closed alternatives, generics for required static variation, and `dyn` when consumers need type erasure or heterogeneous storage, weighing API, binary-size, and compile-time trade-offs. Reuse fitting standard traits, collapse single-implementation traits introduced solely for internal mocking, and require macros to earn what functions, generics, or traits cannot express cleanly.

**Functions and receivers.** Extract a single-caller private function when its signature expresses a complete decision (such as a validation or transformation) without reading its body; keep trivial forwarding and local-state fragments inline. Use methods for meaningful receivers, plain functions for stateless one-shot operations, and private context structs where grouped state clarifies ownership, collapsing one-off `Drop`-closure or trait-adapter wrappers satisfied by `std` or declared crates unless they enforce a cross-function invariant; see the [responsibility examples](references/responsibility.md). Treat broad names (`manager`, `service`, `helpers`) as prompts to inspect ownership rather than automatic defects.

**Co-change and single ownership.** Trace one realistic change: pieces that must change together through a copied rule, implicit call order, or split state belong under one owner or a compiler/test check. Place pure rules shared by unconnected crates in `crates/shared-rules` when they pass its crate-doc membership test. Exhaustive matches already surface new variants, independent similar text may stay separate, and flag-accumulating helpers warrant checking whether responsibilities diverged.

**Coupling and surface area.** Reduce coupling before adding layers by tightening visibility, narrowing parameters or results, or moving decisions to the existing owner (`loop`, error classifier, config section, or adapter). Keep public APIs minimal and single-path while using internal re-exports freely for crate layout. Retain shims or fallbacks only with an accepted requirement, owner, observable trigger, and removal condition, and verify where rollback or error-mapping guarantees live before collapsing a container or wrapper. Judge structure by ownership and behavior rather than line-count or syntax taste.
