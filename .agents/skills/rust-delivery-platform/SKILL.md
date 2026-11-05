---
name: rust-delivery-platform
description: "Gate chain. Use when a Rust service CI job, tool pin, Dockerfile, image check, publication step, or deployment profile decides whether a candidate may ship, or when a gate must be added, waived, or shown to have observed its surface."
---

# Rust Delivery Platform

**Gate chain.** Connect each required delivery decision to its exact candidate, artifact, observing command, pass condition, and recovery consequence. A green status is evidence only for what the selected gate actually exercised. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Use the project's current delivery and validation owners. In this service, [CI/CD production readiness](../../../docs/ci-cd-production-ready.md) and [validation routing](../../../docs/validation-routing.md) govern classification, local versus CI execution, post-merge proof, publication, and recovery. Read current workflow and script owners instead of reconstructing them from a remembered job list. The same classification may intentionally select different local, pull-request, and post-merge commands; verify that the required surface has an owner in the appropriate phase.

A selected required job must succeed. A skipped job is acceptable only when the classifier or gate contract intentionally leaves it unselected; a swallowed failure, empty test selection, or absent job is not equivalent. Check conditional execution and aggregation together, including cancellation and missing-result cases. A new path family or gate joins the existing classifier and its focused proof rather than creating a parallel selection rule.

Keep toolchain, dependency, tool, and image identities with their canonical pins and compatibility policy. Validate the actual resolved artifact rather than only the spelling of a tag or command. Security scanners, SBOM generation, and provenance need access to the artifact information they claim to inspect; a successful scan of an incomplete inventory does not cover the omitted surface. A waiver must retain the accepted owner, reason, scope, and expiry or reopen condition.

Prove runtime-image behavior from the boundary a consumer uses. Configuration, permissions, health, lifecycle completion, and built revision identity are distinct checks, and a build alone establishes none of them. Use the current image owner for the selected profile and runtime budgets rather than copying a fixed grace period into this method. Reuse adequate evidence only when its candidate and relevant inputs still match.

Before publication or deployment, follow [external-effect authority](../../../docs/spec-first-workflow/shared/external-effects.md). Bind the authorized candidate to its immutable output, signing and attestation identity, promotion target, and remote read-back. Changing a gate or having credentials does not itself authorize release, a local heavy run, or a destructive recovery. A failed required check keeps its original outcome visible; an infrastructure retry is not permission to rerun code until it passes.

For review, name any gap between a required property and the artifact or execution that supposedly proves it, without editing. For implementation, extend the existing owner and run the focused self-test or make target selected by the validation policy. Preserve local, CI-owned, and post-merge boundaries and report pending owners explicitly. A local pass cannot substitute for a requested CI or registry result, and a method mentioning a tool does not make every delivery gate mandatory for the task.
