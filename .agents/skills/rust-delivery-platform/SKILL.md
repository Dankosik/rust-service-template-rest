---
name: rust-delivery-platform
description: "Gate chain. Use when a CI job, tool pin, Dockerfile, image check, release step, or deployment profile governs shipping or gate waivers."
---

# Rust Delivery Platform

**Gate chain.** Connect each required delivery decision to its exact candidate, artifact, observing command, pass condition, and recovery consequence. A green status is evidence only for what the selected gate actually exercised.

[CI/CD production readiness](../../../docs/ci-cd-production-ready.md) and [validation routing](../../../docs/validation-routing.md) govern classification, local versus CI execution, post-merge proof, publication, and recovery. Read current workflow and script owners instead of reconstructing them from a remembered job list. The same classification may intentionally select different local, pull-request, and post-merge commands; verify that the required surface has an owner in the appropriate phase.

A selected required job must succeed; a skipped job is acceptable only when the classifier or gate contract intentionally leaves it unselected, not via swallowed failure, empty test selection, or an absent job. Check conditional execution and aggregation together, including cancellation and missing-result cases. A new path family or gate joins the existing classifier and its focused proof rather than creating a parallel selection rule.

Keep toolchain, dependency, tool, and image identities with their canonical pins and compatibility policy, delegating manifest, lockfile, artifact-path, and descriptor queries to Cargo, Git, and OCI structured outputs rather than custom lockfile surgery or CLI-text scraping, and validating the actual resolved artifact rather than tag or command spelling. Security scanners, SBOM generation, and provenance need access to the artifact inventory they claim to inspect. A waiver must retain the accepted owner, reason, scope, and expiry or reopen condition.

Prove runtime-image behavior from the boundary a consumer uses: configuration, permissions, health, lifecycle completion, and built revision identity are distinct checks that a build alone does not establish. Use the current image owner for the selected profile and runtime budgets rather than copying a fixed grace period into this method, and reuse adequate evidence only when its candidate and relevant inputs still match.

Before publication or deployment, follow [external-effect authority](../../../docs/spec-first-workflow/shared/external-effects.md), binding the authorized candidate to its immutable output, signing and attestation identity, promotion target, and remote read-back. Changing a gate or having credentials does not authorize release, a local heavy run, or a destructive recovery, and an infrastructure retry is not permission to rerun code until it passes.

Mentioning a tool or gate here does not make every delivery gate mandatory for the task, and a local pass cannot substitute for a requested CI or registry result.
