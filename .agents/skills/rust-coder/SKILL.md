---
name: rust-coder
description: "Reader. Use when implementing specified behavior in this Rust service workspace within its existing crate boundaries, runtime, and technical choices."
---

# Rust Coder

**Reader.** Implement the intended behavior at the earliest owner that can carry it, as a module a newcomer can follow top-down on first reading. Before typing, list every edge the requirements name and decide what the code does at each; a tidy module that misses an edge has failed. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Read the affected code and its callers, then extend the existing path. Ownership boundaries are crates: the service crate composes and owns process lifecycle, config owns the typed snapshot, health owns readiness, infra crates adapt one transport or provider, and feature crates own business behavior without depending on either. A new operation joins the router and a feature-owned handler; a new key joins its config section file; a new background task joins bootstrap's background registration with a child cancellation token. Before writing a helper, check the affected crate, the standard library, and the declared dependencies. A new abstraction, layer, trait, or crate must carry a current constraint, variation, or dependency direction; survey a new crate or feature as the dependency skill describes.

The entry function reads as the requirements in their order, each line a named step, and a function holding several rules becomes named steps; a step used once stays inline when a name would add nothing. Keep non-trivial decisions deterministic and free of I/O, without adding a layer solely for that.

State each rule once. When two branches or two functions test the same condition, route them through the one function that owns it, such as a state derived from stored data and the current time. Build a value in the same expression that decides it is valid, so no unreachable branch follows a check, and hand back the `&mut` you found instead of looking it up again.

Spend nothing the requirements did not buy. Use plain arithmetic where values are already bounded, and where caller input can reach a limit choose a width that cannot overflow; add no guard for a case the types or earlier code exclude. Keep output types as plain public structs: no alias, second entry point, getter or constructor over plain data, wrapper around a collection, mirrored private and public enums, or field copied from an input that can be borrowed. Derive only what this code uses. Prefer a plain type with one check over a precise type every reader must unpack. Comment only what the code cannot show: rounding, tie breaks, and boundary moments the requirements left open.

Inspect the far side of every touched boundary: callers matching an error variant, drop order, the shutdown stage that must now wait, the validation that must now hold. Choose tests while writing, beside the owner, from accepted behavior; bound every wait and join every task. Reproduce a reported bug before fixing it. Finish with formatting, a locked build, and the tests the validation budget selects, or state the concrete blocker, reporting as AGENTS.md describes.
