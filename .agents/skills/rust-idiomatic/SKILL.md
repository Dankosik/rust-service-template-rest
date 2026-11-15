---
name: rust-idiomatic
description: "Contracts. Use when designing a Rust type, signature, trait impl, conversion, or resolving clone, Arc, Mutex, or borrow ownership."
---

# Rust Idiomatic

**Contracts.** Make ownership and caller expectations visible in ordinary Rust: admissible states, permitted transitions, who may mutate a value, and what absence or failure means. A type proves only the invariant its construction and operations enforce; it does not establish freshness, durable completion, or the lifetime of an external resource.

Inspect scope and data flow before adding a clone or shared owner. A deliberate `.clone()` or `Arc` can be appropriate when it supplies required sharing or simplifies ownership within cost constraints; cloning is not an automatic defect. Prefer owned fields unless a borrowed view earns the lifetime carried by its containers, and keep concrete parameters in application code when broader conversion or iterator inputs add no benefit to actual callers.

Use exclusive mutation where possible. Interior mutability and shared ownership need an identified invariant and access protocol; an `Arc` clone shares an allocation, not synchronized mutation. Check `Send`, `Sync`, and lifetime bounds at actual task or thread crossings without imposing them on deliberately thread-local types. Borrow retained state in place when a transition only inspects or updates it, and move a payload out with `take` or `replace` only when ownership transfers and the replacement state stays valid on every exit. The [ownership example](references/ownership.md) distinguishes a transfer from a snapshot and a restoration obligation. Prefer a scoped guard such as `std::mem::DropGuard` or an existing handle wrapper for one-off infallible cleanup instead of a single-use `Drop` struct that carries no cross-function invariant; RAII releases resources on normal exit and unwinding, but aborts and leaked values bypass destruction, so fallible, durable, or asynchronous completion needs an explicit operation whose outcome is observed rather than relying on `Drop`.

Keep independently varying facts independent and bind a justified derived projection or cache to its source owner in the type rather than passing both separately, as the [representation example](references/representation.md) shows, and decode external sentinel conventions at their boundary. Distinguish absent from empty only when the contract separates them, prefer named fields outside local scope and newtypes when distinct units or identifiers could be confused (a type alias does not prevent interchange), and replace unclear boolean arguments without forcing meaningful independent booleans into enums.

Parse and validate at the owning boundary, then carry the checked property inward where useful. Trace every construction and mutation path, including `Deserialize`, conversions, `Default`, and exposed fields: `new()` validation does not close bypasses elsewhere. Keep invariant-bearing fields private; passive data need not acquire getters. A builder is useful when optional inputs, construction variants, or incremental configuration make constructors hard to use, whereas straightforward construction or fallible validation alone does not require one. Typestate must justify its generic and API cost against an ordinary enum.

Implement standard traits when their semantics fit: `From` for lossless conversions, `TryFrom` when conversion failure matters, and `as` only for its exact casting semantics. Keep ordinary functionality in inherent methods, adding a forwarding method only when it aids discovery alongside a trait. Put bounds where operations need them while retaining bounds required by the representation, and verify whether actual consumers require `dyn` compatibility before designing the trait.

Derive implementations when they carry the required meaning; use `Display` for intentional user-facing text, and write `Debug` manually whenever secrets or credentials require redaction. Keep `Eq`, `PartialOrd`, `Ord`, and `Hash` coherent rather than adding ordering merely to complete a trait list. Use `Deref` only for smart-pointer access, operators only for mathematical or logical meaning, and `#[must_use]` when accidental discard defeats correct use.

Choose collections by required operations and ordering (`BTreeMap` supplies key order, not insertion order), and keep arithmetic overflow and byte-versus-text semantics explicit. Replacing fallible collection with `filter_map(Result::ok)` silently turns errors into discards, and moving `take` before `filter` changes which items qualify. Method prefixes do not prove measured cost.

Do not modernize unrelated code or add types, traits, lifetimes, or validation merely to resemble an example.
