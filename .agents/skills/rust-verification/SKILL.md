---
name: rust-verification
description: "Use for verification-only work or when deciding whether existing evidence supports a requested claim at its stated scope."
metadata:
  invocation: model
  kind: method
---

# Rust Verification

An **evidence boundary** is the behavior a proof would fail on; a claim cannot
be wider than that boundary.

`claim -> observable -> command -> result -> exercised scope -> gap`

Each command in this workspace proves a bounded thing. `cargo fmt --check`
proves formatting. `make lint` proves clippy shape at `pedantic`, not
behavior. `make build` proves the workspace compiles, not that it runs.
`make test-package PKG=<crate>` proves that crate's unit tests; `make test`
proves the whole workspace including the process tests in
`crates/service/tests`, which exercise the built binary's startup, probes,
`/metrics`, SIGTERM drain, and exit codes. `make check` is the explicit
full gate CI runs. A manual run with `env/config/local.toml` observes the
startup summary and the shutdown sequence in the log.

A passing command proves only the surfaces it observed. File presence, a
compile, a test filter that matched zero tests, a skipped process test, or an
unrelated aggregate cannot carry a claim. A test that passes with a `sleep`
as synchronization has not proven ordering. `cargo tree` proves resolution,
not runtime behavior. Nothing local proves CI, a container, a collector
receiving spans, or production behavior; say so instead of implying it.

Record the exact command, its preconditions, whether the result was fresh or
reused, and the scope actually exercised. Reuse a passing result while its
inputs, configuration, toolchain, and environment are unchanged. Ordinary
local completion stops at the boundary [AGENTS.md](../../../AGENTS.md#validation-budget)
sets; do not add runs for confidence, and do not build an environment to
close an optional gap. A known real defect in scope still requires a fix.

Complete when each requested claim is supported at its stated scope or
returned with the exact missing proof and its owner. Never weaken an
explicitly requested CI, runtime, or deployment result into local success.
