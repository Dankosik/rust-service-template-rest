---
name: rust-dependencies
description: "Resolution. Use before adding or upgrading a Rust crate or feature, changing the toolchain pin or edition, or choosing between a library and template-owned code."
---

# Rust Dependencies

**Resolution.** Prefer a maintained crate that already solves the problem idiomatically over template-owned code, and prove the choice before declaring it. Explain what Cargo actually selects before changing its declarations. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Before a new crate or a new general mechanism, survey the candidates against the actual requirement and record the comparison in the research synthesis for the current topic under specs: the latest version and release date from the registry, the maintenance signal from the repository, the exact behavior verified in source or in a scratch project outside the repository, known issues, and what the crate does not do. The Go template supplies the problem and its reasons, never the shape of the solution; when Rust solves it differently, record the deviation. Template-owned code exists only for a gap the synthesis names.

Declare each version once in the workspace dependency table with default features disabled, enable features per crate, and keep every Cargo invocation locked so a lockfile change ships with the change that caused it. Inspect what actually resolved: the tree with duplicates shown for a second version of the same crate, and the feature-edge view for a feature another dependency turned on. A second OpenTelemetry minor or an unexpected exporter client feature silently sends telemetry to a no-op provider; the OpenTelemetry family moves together, with the tracing bridge one minor ahead.

The toolchain is pinned exactly in the toolchain file and mirrored by the workspace minimum version; bump both in one reviewed change after the full check passes on the new compiler, and let clippy's new suggestions land in that change rather than silently. Edition 2024 forms are preferred where they clarify, and a modernization must preserve behavior and error identity. Declaring a minimum version does not prove the code compiles on it.

Review maintenance, licensing, and advisory evidence in proportion to the change; a build script or procedural macro joins the trust analysis. Never edit the lockfile by hand or bypass integrity checks to fix a download.

For analysis, explain the failing or proposed resolution without editing. For a requested change, finish when the crate is declared once, its features are explicit, the resolved tree has been inspected, the synthesis names the decision, and the full check passes. All-features success is not proof that the default path works, and compilation is not runtime verification.
