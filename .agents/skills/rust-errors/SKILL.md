---
name: rust-errors
description: "Failure semantics. Use when defining Rust errors, failure mappings, exit codes, degradation, or fallible completion."
---

# Rust Errors

**Failure semantics.** Trace failure from the operation that observes it to the consumer that acts on it, separating rejected inputs, partial effects, uncertain outcomes, and post-commit cleanup failures so each boundary can stop, degrade, retry, or continue accurately.

**Consumer-driven types.** Scope error types per operation (or a struct with a private kind and `is_*` predicates) rather than one crate-wide mega-enum so callers match only reachable failures, while letting report-and-exit CLI/main boundaries use contextual errors. Implement `std::error::Error`, `Display`, and `Debug` (via `thiserror`) on public library error types, preserving upstream `#[source]` causes and adding variants only for real consumers; return expensive consumed buffers or messages inside `Err` when callers may retry or inspect them, as shown in [error chain examples](references/error-chains.md).

**Context and redaction.** Attach safe resource identifiers (path, key, operation, input position) at the failing operation, and never interpolate a `#[source]` or `#[from]` field into `#[error("...")]` when the boundary renderer walks `Error::source()`, avoiding duplicated cause text and self-referential `source()` loops. Keep messages lowercase without trailing punctuation, redact secrets across every nested cause, and preserve failure precedence and all-or-nothing guarantees.

**Conversions and retries.** Implement `From` only for context-free canonical conversions and never blanket-`#[from]` wide errors like `std::io::Error` where `?` would erase the failing path or action—wrap with `map_err` instead, and match structured variants rather than formatted strings. Require safe effect and idempotency identity before retrying timeouts or broken connections, log ignored cleanup failures only under an explicit best-effort policy, and represent help or early clean exits as normal `Ok` outcomes.

**Fallible completion and panics.** Expose an explicit observed close or flush method whenever resource completion can fail or await, reserving `Drop` for infallible non-blocking release and keeping primary outcomes distinct from cleanup errors. Use `catch_unwind` solely as task-boundary defense in depth, observe spawned-task failures via their supervisor, and re-establish or quarantine any multi-step invariant a panic could leave partial before exposing shared state.

**Service catalog.** Route client-visible HTTP failures through `crates/service-failure` and `crates/infra-http`, and startup, degradation, shutdown, and process-exit policy through `crates/service/src/bootstrap`, testing behavioral mappings rather than boilerplate variants.
