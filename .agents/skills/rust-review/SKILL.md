---
name: rust-review
description: "Falsification. Use whenever reviewing, auditing, or critiquing Rust crates, .rs files, PR diffs, or modules read-only to find correctness, concurrency, type, error, API, and test defects."
---

# Rust Review

**Falsification first.** Read the target Rust diff or module alongside its callers before commenting, hunting for reachable runtime defects, broken invariants, and fragile seams rather than surface style nits. When working inside this repository's spec-first workflow, follow [Implementation Review](../../../docs/spec-first-workflow/phases/implementation-review.md); for standalone or ad-hoc Rust reviews, apply the method below directly.

**Domain lenses.** Load matching domain skills before judging—`rust-idiomatic` for state representation, ownership, and API contracts, `rust-errors` for failure semantics and error chains, `rust-tokio` for cancellation safety, lock scopes, and task lifetime, `rust-security` for untrusted boundaries and secret handling, `rust-performance` for algorithmic or allocation regressions, and `rust-structural-quality` for ownership seams and unnecessary wrappers. Test each candidate issue against a concrete reachable path rather than subjective style preference.

**Test falsifiability.** Audit accompanying tests with `rust-testing` using the Mutation Mental Test—mentally invert or delete each guarded production branch, boundary check, or error path and flag tests that would still pass, assert tautologies, omit time or concurrency edge cases, or prove only mock interactions rather than real observable behavior.

**Actionable output.** Inspect statically without building speculative scratch binaries unless disambiguating an ambiguous compiler or runtime fact. Group confirmed findings by severity (`Must fix`, `Should fix`, `Consider`) with exact `file:line` citations, the concrete reachable failure trace or surviving mutant, and a minimal compiling replacement snippet for each fix, stating explicitly when no defect survives.
