---
name: rust-verification
description: "Evidence boundary. Use for verification-only work or deciding whether existing evidence supports a claim at its stated scope."
---

# Rust Verification

**Evidence boundary.** A proof covers exactly the behavior it would fail on, and a claim cannot be wider than that boundary. Name the observable whose absence would make the selected proof fail before running it.

Workspace checks prove bounded surfaces: `make lint` checks clippy shape at pedantic, not runtime behavior; `make check-skills` checks instruction shape, not model effect; workspace process tests cover startup, probes, metrics, `SIGTERM` drain, and exit codes; a manual baseline run observes the startup summary and shutdown sequence in logs.

A passing command proves only the surfaces it observed. File presence, a compile, a test filter that matched nothing, a skipped process test, or an unrelated aggregate cannot carry a claim. A test that passes with a sleep as synchronization has not proven ordering, and the dependency tree proves resolution rather than runtime behavior. Nothing local proves CI, a container, a collector receiving spans, or production behavior; say so rather than implying it.

Record the exact command, its preconditions, whether the result was fresh or reused, and the scope actually exercised. Reuse a passing result while its inputs, configuration, toolchain, and environment are unchanged. For verification-only work, do not edit files; return unsupported claims with their exact missing proof and owner, never weaken a requested CI, runtime, or deployment result into local success, and never present a serial self-review as independent review.
