---
name: rust-testing
description: "Behavior. Use when writing or reviewing Rust tests: assertions, proving layer, real dependencies, time control, and defect detection."
---

# Rust Testing

**Behavior first.** Name the observable contract a defect would break, select the smallest layer able to watch it fail, and assert concrete caller- or operator-visible outcomes (status, body, Problem code and headers, committed rows, published message, telemetry event, or exit code) against independent expected values.

**Smallest proving layer.** Match the harness to the boundary under test: unit tests beside pure logic, mounted `Router` tests when middleware and extractors participate, bounded loopback listeners for socket behavior, binary process tests for startup/drain/exit codes, and Compose integration suites for real providers. Assert public behavior so internal refactors stay green.

**Real providers versus doubles.** Exercise real PostgreSQL or broker instances via `ALLOW_HEAVY=1 make test-integration-db` and the Compose targets in `make/template.mk` whenever provider semantics belong to the claim; reserve test doubles for unavailable external peers, injected faults, or deterministic inputs outside the asserted contract. Reuse standard and workspace primitives in fixtures while keeping minimal custom wire or body fixtures where tests inject partial reads, mid-stream errors, or cancellation timing beyond library mocks.

**Clocks and synchronization.** Drive paused Tokio time exclusively with Tokio timers (accounting for idle auto-advance), synchronize concurrent steps on observable state transitions, notifications, or task joins with bounded timeouts, and in process tests resolve built executables from Cargo's test environment or `compiler-artifact` JSON, await readiness before signalling `SIGTERM`, and wait for child termination through the existing harness.

**Defect reproduction and discovery.** Organize tests by behavior with small fixtures, explicit boundary limits, and independent wire fixtures where serialization compatibility matters. Reproduce bugs with a failing test first, verify non-zero executed test counts, and use `make mutants PKG=crate FILE=path`, property tests, Miri, or fuzzing selectively as discovery aids rather than mandatory gates.
