---
name: rust-coder
description: "Use when implementing specified behavior in this Rust service workspace within its existing crate boundaries, runtime, and technical choices."
---

# Rust Coder

**Execution.** When the intended behavior is clear, implement it directly at the earliest owner that can carry it. Treat supplied requirements as constraints. Preserve settled technical decisions outside the explicitly requested change; do not reopen unrelated choices.

Read the affected code and its callers, then extend the existing path. Ownership boundaries are crates: the service crate composes and owns process lifecycle, the config crate owns the typed snapshot, the health crate owns readiness, infra crates adapt one transport or provider, and feature crates own business behavior without depending on a transport or provider. A new operation joins the router and a feature-owned handler rather than the hardened middleware chain; a new key joins its config section file; a new background task joins the task tracker in bootstrap with a child cancellation token. A parallel path beside an existing owner is the wrong default.

**Reuse.** For a proposed helper, check the affected crate, the standard library, and the declared dependencies for matching semantics before writing it. A new abstraction, layer, trait, or crate must carry a current constraint, variation, or dependency direction; hypothetical reuse does not count. Before adding a crate or feature, survey and record the choice as the dependency skill describes; template-owned code exists only for a gap that survey names.

**Clarity.** Use intention-revealing names, cohesive responsibilities, explicit ownership, and visible effects and failure paths. Inspect the far side of every touched boundary: the callers that match on an error variant, drop order, the shutdown stage that must now wait, and the config validation that must now hold. Match the surrounding code's naming and comment density.

Work in behavior-sized changes with independent expectations. Choose tests while writing the code, beside the owner, from accepted behavior rather than the implementation's current output; bound every wait and join every task. For a bug with an available reproducer, observe that it fails for the reported reason before fixing it. A compile error is not evidence that a regression test detects the bug.

Carry the change through formatting, the workspace build, and the tests of each changed crate; run the whole workspace suite when a manifest or the lockfile changed. Clippy runs at pedantic with warnings as errors: fix the finding or add a site-local allow with its reason, never a workspace-wide relaxation. Every Cargo invocation is locked, so a lockfile change ships with the change that caused it. Delete each new seam whose removal preserves accepted behavior.

Finish when the requested outcome and required checks are satisfied, or state the concrete blocker and unavailable verification. Report the changed behavior and actual evidence, distinguishing introduced failures from unrelated baseline failures. Do not invent unrelated cleanup, extra environments, or new test infrastructure as completion gates.
