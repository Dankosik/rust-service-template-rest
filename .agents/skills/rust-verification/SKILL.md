---
name: rust-verification
description: "Evidence boundary. Use for verification-only work or deciding whether existing evidence supports a claim at its stated scope."
---

# Rust Verification

**Evidence boundary.** Bound every verification claim to the exact behavior whose failure the executed check would catch, naming that observable before running the command.

**Surface mapping.** Map each workspace check to what it actually proves: `make lint` proves static lint conformance, `make check-instructions` proves skill shape and harness carrier parity, process tests prove binary startup, probes, metrics, `SIGTERM` drain, and exit codes, and a baseline run proves emitted startup/shutdown logs.

**Executed scope.** Count only executed, non-skipped checks that observe the claimed boundary—separating compilation, lockfile resolution, and sleep-timed runs from behavioral proof, and separating local test passes from CI, container, collector-delivery, or production outcomes.

**Receipts and read-only boundary.** Record the exact command, preconditions, fresh-versus-reused status (reusing passes only while inputs, config, toolchain, and environment match), and exercised scope. During verification-only work, keep the tree untouched, report unverified claims with their missing proof and owner, preserve requested CI/runtime/deployment boundaries, and keep author checks distinct from independent review.
