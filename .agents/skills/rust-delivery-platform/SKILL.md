---
name: rust-delivery-platform
description: "Gate chain. Use when a CI job, tool pin, Dockerfile, image check, release step, or deployment profile governs shipping or gate waivers."
---

# Rust Delivery Platform

**Gate chain.** Connect every delivery decision to its exact candidate, artifact, observing command, pass criterion, and recovery path; treat a green check as evidence strictly for the surface that gate exercised.

**Routing owners.** Follow [CI/CD production readiness](../../../docs/ci-cd-production-ready.md) and [validation routing](../../../docs/validation-routing.md), inspecting live `.github/workflows/` and `scripts/` owners so local, pull-request, and post-merge stages each cover their assigned surface.

**Selection and aggregation.** Ensure every selected required job executes non-empty checks and fails closed on cancellation or missing results, allowing skips only where `.github/actions/detect-changes/detect.py` intentionally leaves a surface unselected. Register new path families in the existing classifier alongside its unit test.

**Pins and waivers.** Keep toolchain, crate, CLI tool (`mise.toml`), and container base pins canonical, delegating manifest, lockfile, artifact-path, and descriptor queries to structured Cargo, Git, and OCI outputs and verifying resolved digests and inventories so scanners, SBOMs, and provenance inspect the true artifact. Record every waiver with its owner, reason, scope, and expiry or reopen trigger.

**Runtime image and release.** Verify container configuration, user permissions, health probes, graceful drain, and embedded revision metadata through the current image check scripts, reusing receipts only while inputs match. Bind any authorized publication or deployment under [external-effect authority](../../../AGENTS.md#authority) to immutable digests, attestations, and remote read-back, reporting local checks separately from CI or registry gates.
