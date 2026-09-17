---
name: rust-structural-quality
description: "Use when a diff adds a crate, module, trait, layer, helper, or parallel path whose current necessity must be judged, or when placement in the workspace is not forced."
metadata:
  invocation: model
  kind: method
---

# Rust Structural Quality

Judge structure with a **deletion test**: which current responsibility or
constraint becomes harder to satisfy if the structure is removed?

Ownership boundaries are crates, and the crate graph is the dependency rule:
`service` composes; `config`, `health`, and `infra-*` are leaves that never
import each other's policy; `<feature>` crates will depend on no transport or
provider crate. A new crate needs an independent reason to exist (a
dependency direction the graph must enforce, a compile-time boundary, or an
independent lifecycle), not a tidy name. A new module inside a crate needs a
present artifact; do not create a directory before its first real file.

A trait with one implementation is justified only when it protects a real
dependency direction or is stored as a trait object (`Box<dyn Probe>`); a
trait created to mock internal choreography is a collapse candidate. A
one-use helper survives when it uniquely carries a protocol or ownership
constraint (the request-id grammar, the secret-key predicate), not when it
shortens a call site. Generic `util`, `common`, and `helpers` modules have
no owner and are rejected by name.

Parallel execution paths, compatibility shims, and stale surfaces are
collapse candidates unless an accepted current requirement keeps them, with
one owner, an observable activation, and a removal condition. The
research-first rule applies to structure too: a maintained crate that owns
a general mechanism replaces template-owned code once it meets the
requirement (see `rust-dependencies`).

Filenames name the owned behavior (`access_log.rs`, `secret_policy.rs`),
not chronology or size; split by independent lifecycle, audience, or
authority. Tests live beside their owner; `crates/service/tests` holds only
black-box process proof.

For review, try to delete or collapse each added structure and retain it only
when removal violates a current constraint or increases complexity; a named
smell alone is not a defect. For implementation, select the least structure
that owns the current responsibility and simulate one realistic next change
to confirm it touches fewer owners with the structure than without.
