---
name: rust-errors
description: "Use when a change defines, maps, or renders an error, chooses a problem code or exit code, or decides between failing, degrading, and continuing."
metadata:
  invocation: model
  kind: method
---

# Rust Errors

Follow failure from the operation that knows what happened to the boundary
that reports it. Each hop preserves the cause and decides one thing: stop,
degrade, or continue.

Library crates define typed errors with `thiserror`, one enum per owner,
`#[source]`/`#[from]` for the cause, and operator-readable `Display` that
names the configuration key or resource, never a secret (`SecretString`
fields print `[REDACTED]`; error text must not re-expose them). Classify
structured errors by variant, not by matching `Display` strings.

The transport edge is the only place an error becomes client-visible.
`infra_http::Problem` renders the closed `Code` catalog with stable `type`,
`title`, and `status`; unclassified failures use `SANITIZED_DETAIL` so a
caller cannot tell which internal path broke. A code without a matching
response in the API contract is unreachable, not wrong; adding a code means
adding it to the catalog and its tests, never an ad-hoc status.

Startup failures return through `bootstrap::run`, which prints the message
and returns `ExitCode::FAILURE`; teardown overruns return exit code 3;
`process::exit` is never called because it skips destructors and the
telemetry flush. Telemetry setup failures degrade with a logged reason and
never block the service; configuration and bind failures stop it.

Keep `panic!`, `unwrap`, and `expect` out of production paths (the workspace
lints warn and `make lint` promotes them); a panic inside a handler becomes a
sanitized 500 through `CatchPanicLayer`, but a panic in a spawned task is
only visible through its `JoinHandle`. A retry, a swallowed `Err`, or a
default value can hide the mechanism without repairing it.

For review, explain the failure policy and the false-success risk without
editing. For implementation, test the changed variant or mapping at the
boundary that renders it: the problem body for HTTP, the exit code and stderr
for the process. Assert the relevant output and effect, not every failure
mode this skill names.
