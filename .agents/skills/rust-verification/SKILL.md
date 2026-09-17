---
name: rust-verification
description: "Evidence boundary. Use for Rust service verification-only work, or when deciding whether existing evidence supports a requested claim at its stated scope."
---

# Rust Verification

**Evidence boundary.** A proof covers exactly the behavior it would fail on, and a claim cannot be wider than that boundary. Name the observable whose absence would make the selected proof fail before running it. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Each command in this workspace proves a bounded thing. The format check proves formatting. The lint gate proves clippy shape at pedantic, not behavior. The build proves the workspace compiles, not that it runs. A crate's tests prove that crate; the workspace suite adds the process tests, which exercise the built binary's startup, probes, metrics route, SIGTERM drain, and exit codes. The full check is the explicit gate CI runs. A manual run with the local baseline file observes the startup summary and the shutdown sequence in the log. The skills check proves the shape of instructions, not their effect on a model.

A passing command proves only the surfaces it observed. File presence, a compile, a test filter that matched nothing, a skipped process test, or an unrelated aggregate cannot carry a claim. A test that passes with a sleep as synchronization has not proven ordering. The dependency tree proves resolution, not runtime behavior. Nothing local proves CI, a container, a collector receiving spans, or production behavior; say so rather than implying it.

Record the exact command, its preconditions, whether the result was fresh or reused, and the scope actually exercised. Reuse a passing result while its inputs, configuration, toolchain, and environment are unchanged. Ordinary local completion stops at the build and relevant tests the repository contract sets; do not add runs for confidence, and do not build an environment to close an optional gap. A known real defect in scope still requires a fix.

For verification-only work, do not edit files. Finish when each requested claim is supported at its stated scope or returned with the exact missing proof and its owner. Never weaken an explicitly requested CI, runtime, or deployment result into local success, and never present a serial self-review as independent review.
