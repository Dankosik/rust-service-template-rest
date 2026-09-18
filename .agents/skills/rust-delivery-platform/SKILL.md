---
name: rust-delivery-platform
description: "Gate chain. Use when a Rust service CI job, tool pin, Dockerfile, image check, publication step, or deployment profile decides whether a candidate may ship, or when a gate must be added, waived, or shown to have observed its surface."
---

# Rust Delivery Platform

**Gate chain.** Delivery trust is a chain of gates, each with one artifact, one command, a fail-closed pass condition, and a recovery consequence; a candidate ships when every required gate has observed its surface, not when a status is green. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Decide against the existing owners. scripts/ci/changed-surfaces.sh names every surface and fails closed on an unknown path, so a new path family joins it with the gate that observes it, and the self-test pins the row. The same output drives ci.yml and make verify; a gate present in only one is drift. Tool versions live once in tools/versions.env; make tools-check proves the Cargo tools resolve, the Dockerfile ARG defaults agree, and the builder tag equals the toolchain channel. Base images are digest-pinned in the Dockerfile FROM lines Dependabot moves; a rust tag bump lands with rust-toolchain.toml in one change.

A gate observes something or it does not belong. cargo-deny reads the locked graph, and an advisory ignore names the crate, the reason, and what reopens it; cargo-shear finds unused declarations; Gitleaks scans the range on pull requests and the history on tags; Dependency Review needs the repository dependency graph, an operator setting. The required and codeql-required jobs accept a skipped gate and reject a failed or cancelled one: skipping is the classifier's decision, never a step that swallowed its failure.

The image is proven from outside. scripts/ci/runtime-image-check.sh starts it read-only with all capabilities dropped, waits for /health/ready from the host, asserts app.commit in the service_starting record equals the built commit, and requires exit zero from docker stop inside the forty-five second grace the runtime budget policy derives. Trivy sees the Rust dependencies only because cargo auditable build embedded them, so dropping the auditable wrapper silently blinds the scan; build and check share one tag.

Publication is fail-closed by construction. cd.yml runs only when the repository variable is set, only for a successful same-repository push run at the exact SHA, and only through the one composite action, because a reusable workflow would change the keyless signing identity consumers verify against. The order is build, lifecycle check, scan, SBOM, push, digest, sign, attest, verify back out of the registry, then promote with a digest read-back per tag; a release tag that does not equal the crate version is refused before anything is built. A waived required gate keeps an owner and an expiry and stays a finding.

For review, trace each required gate to its artifact, command, pass condition, and recovery, and name any green gate that did not exercise its surface, without editing. For implementation, add the gate at its owner, run the matching self-test and make target, and report what it observed; a heavy or publication run is required only when the change alters what it proves, and local success never stands in for a requested CI or registry result.
