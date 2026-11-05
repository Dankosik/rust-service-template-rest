---
name: rust-errors
description: "Failure semantics. Use when Rust errors, problem codes, exit codes, degradation, or cleanup can misrepresent an operation's outcome."
---

# Rust Errors

**Failure semantics.** Follow failure from the operation that knows what happened to the boundary that reports it, and let each hop decide one thing: stop, degrade, or continue. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Library crates define typed errors with thiserror, one enum per owner, preserving the cause through a source and adding useful context such as the configuration key or resource. A public error type implements std::error::Error, Display, and Debug, and is never (). Variants serve code that branches on them; the message and source chain serve the operator. Display is lowercase, concise, without trailing punctuation, and never re-exposes a secret; secret strings print as redacted, and error text must not undo that. Classify structured errors by variant rather than by matching display strings. Retain typed distinctions where callers choose different recovery behavior; an opaque wrapper is acceptable only at the composition root.

Application boundaries that only report and exit may use an existing contextual error type, such as anyhow, instead of inventing variants nobody matches. Preserve a typed error where a caller chooses recovery by its kind. Add context at the operation that knows the resource and failed action, and retain the source. Successful early termination, such as displaying help, is a normal outcome, not an empty error message used as a sentinel. A small application does not acquire the service's problem catalog, telemetry, or exit-code policy from this skill.

In this service, the transport edge is the only place an error becomes client-visible. Problem responses render the closed code catalog with stable type, title, and status; an unclassified failure uses the sanitized detail so a caller cannot tell which internal path broke. If a catalog code has no documented response, trace whether the affected served path can emit it: a reachable result needs a matching contract, while an unused catalog entry does not justify inventing a route. Adding a code means adding it to the catalog and its coverage test, never emitting an ad-hoc status.

In this service, startup failures return through the entry function, which prints the message and exits with failure; a teardown that overran a budget exits with a distinct code so the platform can tell it from a crash; process exit is never called directly because it skips destructors and the telemetry flush. Telemetry setup failures degrade with a logged reason and never block the service; configuration and bind failures stop it.

When resource completion can fail, lose data, or wait, expose an explicit completion operation whose owner observes its result within the existing lifecycle budget. Drop performs best-effort cleanup without panicking or blocking; destruction alone confirms neither flush nor durable completion. Keep infallible resource release in Drop when no separate completion outcome needs observation.

A panic reports a bug, never bad input: invalid input and external failures return Result. Where you own the error type, implement From so that ? converts, rather than map_err at each call. catch_unwind is defense in depth at a task boundary, never control flow. Keep panic, unwrap, and expect out of production paths; the workspace lints flag them and the lint gate promotes them to errors. A panic inside a handler becomes a sanitized server error through the panic-recovery layer, but a panic in a spawned task is visible only through its join handle. A retry, a swallowed error, or a substituted default can hide the mechanism without repairing it, and a printed diagnostic followed by success misleads automation.

For review, explain the failure policy and the false-success risk without editing. For implementation, test the changed variant or mapping at the boundary that renders it: the problem body for HTTP, the exit code and standard error for the process. Assert the relevant output and effect, not every failure mode this skill names, and report exactly what was exercised.
