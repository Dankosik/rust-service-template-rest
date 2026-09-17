---
name: rust-structural-quality
description: "Deletion test. Use when a Rust service diff adds a crate, module, trait, layer, helper, or parallel path whose current necessity must be judged, or when placement in the workspace is not forced."
---

# Rust Structural Quality

**Deletion test.** Judge each added structure by asking which current responsibility or constraint would become harder to satisfy if it were removed. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Ownership boundaries are crates, and the crate graph is the dependency rule: the service crate composes; the config, health, and infra crates are leaves that never import one another's policy; feature crates depend on no transport or provider. A new crate needs an independent reason to exist, such as a dependency direction the graph must enforce, a compile-time boundary, or an independent lifecycle; a tidy name is not a reason. A new module needs a present artifact, and a directory is never created before its first real file.

A trait with a single implementation is justified when it protects a real dependency direction or is stored as a trait object, as the probe trait is; a trait created to mock internal choreography is a collapse candidate. A one-use helper survives when it uniquely carries a protocol or ownership rule, such as the request-id grammar or the secret-key predicate, not when it shortens a call site. Modules named util, common, or helpers have no owner and are rejected by name.

Parallel execution paths, compatibility shims, and stale surfaces are collapse candidates unless an accepted current requirement keeps them with one owner, an observable activation, and a removal condition. Structure follows the same reuse rule as code: a maintained crate that owns a general mechanism replaces template-owned code once it meets the requirement, and the dependency skill records that choice.

Filenames name the owned behavior rather than chronology or size, and files split by independent lifecycle, audience, or authority. Tests live beside their owner; the service crate's integration tests hold only black-box process proof.

For analysis or review, make no edits; try to delete or collapse each candidate and retain it only when removal would violate a current constraint or increase complexity. A named smell alone is not a defect, and a review need not manufacture findings. For implementation, select the least structure that owns the current responsibility, simulate one realistic next change to confirm it touches fewer owners with the structure than without, and preserve behavior, failures, ordering, and resource lifetime through any refactoring.
