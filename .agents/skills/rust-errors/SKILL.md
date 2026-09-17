---
name: rust-errors
description: "Failure semantics. Use when Rust service errors, problem codes, exit codes, degradation, or cleanup can misrepresent an operation's outcome."
---

# Rust Errors

**Failure semantics.** Follow failure from the operation that knows what happened to the boundary that reports it, and let each hop decide one thing: stop, degrade, or continue. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Library crates define typed errors with thiserror, one enum per owner, preserving the cause through a source and adding useful context such as the configuration key or resource. Display stays operator-readable and never re-exposes a secret; secret strings print as redacted, and error text must not undo that. Classify structured errors by variant rather than by matching display strings. Retain typed distinctions where callers choose different recovery behavior; an opaque wrapper is acceptable only at the composition root.

The transport edge is the only place an error becomes client-visible. Problem responses render the closed code catalog with stable type, title, and status; an unclassified failure uses the sanitized detail so a caller cannot tell which internal path broke. A code without a matching response in the API contract is unreachable, not wrong. Adding a code means adding it to the catalog and its coverage test, never emitting an ad-hoc status.

Startup failures return through the entry function, which prints the message and exits with failure; a teardown that overran a budget exits with a distinct code so the platform can tell it from a crash; process exit is never called directly because it skips destructors and the telemetry flush. Telemetry setup failures degrade with a logged reason and never block the service; configuration and bind failures stop it.

Keep panic, unwrap, and expect out of production paths; the workspace lints flag them and the lint gate promotes them to errors. A panic inside a handler becomes a sanitized server error through the panic-recovery layer, but a panic in a spawned task is visible only through its join handle. A retry, a swallowed error, or a substituted default can hide the mechanism without repairing it, and a printed diagnostic followed by success misleads automation.

For review, explain the failure policy and the false-success risk without editing. For implementation, test the changed variant or mapping at the boundary that renders it: the problem body for HTTP, the exit code and standard error for the process. Assert the relevant output and effect, not every failure mode this skill names, and report exactly what was exercised.
