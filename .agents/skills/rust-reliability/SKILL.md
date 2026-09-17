---
name: rust-reliability
description: "Use for any decision involving deadlines, timeouts, retries, overload, readiness, drain, shutdown budgets, or what a dependency failure does to the service."
metadata:
  invocation: model
  kind: method
---

# Rust Reliability

Resilience is **budget arithmetic**: every wait, attempt, and teardown stage
spends a parent budget that someone else already promised.

`parent budget -> per-hop deadline -> failure disposition -> retry, degrade, or shed -> lifecycle -> proof`

Read the existing owner before adding policy. `crates/config`'s `http`
section owns the request budget, header timeout, drain, propagation delay,
capacity bounds, and their validation (`request_timeout <= shutdown_timeout -
readiness_propagation_delay`, `max_connections >= max_in_flight`).
`bootstrap::shutdown` owns the grace deadline and the stage ceilings
(diagnostics 2 s, background join 5 s, dependency close 5 s, telemetry flush
5 s) and validates `grace_period >= shutdown_timeout + tail`. `crates/health`
owns readiness cadence, failure threshold, and staleness. `infra_http::harden`
owns shedding (503 with `Retry-After`, no queue) and the 504 request timeout.
`infra_http::Server` owns connection-level bounds.

A new dependency call fits inside the request budget with a reserve for the
terminal response: `sum(attempts) + sum(backoffs) + reserve <= parent`. Use
accepted values or values read from their owner; when a reserve, backoff
bound, or useful-attempt minimum is not accepted, return that exact gap
instead of choosing a number that makes the inequality fit. A retry needs
jitter and a safely repeatable effect; a queue needs a bound and a shed
behavior; a degrade path needs a named signal.

A new pooled dependency adds a `Probe`, a readiness consequence, a close in
the dependency stage of the shutdown plan under its budget, and a place in
the startup admission that decides whether to admit traffic at all. Shutdown
stages draw from the remaining grace deadline; a slow stage shortens the ones
after it rather than pushing the process into SIGKILL.

For review, trace each affected failure path to its terminal disposition and
reject any worst-case spend that exceeds the parent bound even when each
attempt has a timeout. For implementation, prove the bound with a controlled
test (paused clock, injected slow probe, occupied semaphore) and confirm the
process test still observes readiness flipping before the listener closes.
